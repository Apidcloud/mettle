//! Execution-owned blocking work and pull-based byte sources.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

use crate::{CapabilityError, Span};

pub const CHUNK_BYTES: usize = 64 * 1024;
pub const DEFAULT_READ_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_TRANSFER_BYTES: u64 = 1024 * 1024 * 1024;

type Job = (JoinHandle<()>, Arc<AtomicU8>);

struct Resources {
    directory: PathBuf,
    cancelled: AtomicBool,
    cleanup_failed: AtomicBool,
    capacity: Arc<Semaphore>,
    readers: Arc<Semaphore>,
    jobs: Mutex<Vec<Job>>,
    temporary: Mutex<Vec<PathBuf>>,
    owned: Mutex<Vec<Weak<dyn IoResource>>>,
}

/// A live transfer owned by an entry, without retaining it in observations.
pub trait IoResource: Send + Sync {
    fn cancel(&self);
    fn active(&self) -> bool;
}

/// Resources belonging to one execution entry, shared by its owned branches.
/// Owners must call `cleanup` after joining or aborting all entry futures, including
/// when an embedding application drops the runtime future. Cleanup closes this
/// context permanently; create a fresh context for another entry.
#[derive(Clone)]
pub struct IoContext(Arc<Resources>);

impl fmt::Debug for IoContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoContext").finish_non_exhaustive()
    }
}

impl IoContext {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self(Arc::new(Resources {
            directory,
            cancelled: AtomicBool::new(false),
            cleanup_failed: AtomicBool::new(false),
            capacity: Arc::new(Semaphore::new(64)),
            readers: Arc::new(Semaphore::new(64)),
            jobs: Mutex::new(Vec::new()),
            temporary: Mutex::new(Vec::new()),
            owned: Mutex::new(Vec::new()),
        }))
    }

    #[must_use]
    pub fn resolve(&self, path: &Path) -> PathBuf {
        self.0.directory.join(path)
    }

    /// # Errors
    /// Returns an error when incomplete output capacity is exhausted.
    pub fn register_temporary(&self, path: PathBuf, span: Span) -> Result<(), CapabilityError> {
        let mut paths = self
            .0
            .temporary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if paths.len() >= 64 {
            return Err(CapabilityError::new(
                "too many incomplete filesystem outputs",
                span,
            ));
        }
        paths.push(path);
        Ok(())
    }

    /// # Errors
    /// Returns an error if the execution has been cancelled.
    pub async fn reader_permit(
        &self,
        span: Span,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, CapabilityError> {
        if self.0.cancelled.load(Ordering::Acquire) {
            return Err(CapabilityError::new("I/O execution cancelled", span));
        }
        self.0
            .readers
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CapabilityError::new("I/O execution cancelled", span))
    }

    pub fn forget_temporary(&self, path: &Path) {
        self.0
            .temporary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|p| p != path);
    }

    /// Cancel work before its publication point; already committed work is joined.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.readers.close();
        self.0.capacity.close();
        for resource in self
            .0
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
        {
            resource.cancel();
        }
        for (_, state) in self
            .0
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            let _ = state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// Register a bounded, execution-owned live transfer.
    /// # Errors
    /// Fails if the owner is closed or already owns 64 live transfers.
    pub fn register_resource(
        &self,
        resource: &Arc<dyn IoResource>,
        span: Span,
    ) -> Result<(), CapabilityError> {
        let mut resources = self
            .0
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        resources.retain(|weak| weak.upgrade().is_some_and(|resource| resource.active()));
        if self.0.cancelled.load(Ordering::Acquire) {
            resource.cancel();
            return Err(CapabilityError::new("I/O execution cancelled", span));
        }
        if resources.len() >= 64 {
            resource.cancel();
            return Err(CapabilityError::new(
                "too many live response transfers; consume or close earlier responses",
                span,
            ));
        }
        resources.push(Arc::downgrade(resource));
        Ok(())
    }

    /// Run filesystem work off the executor and retain ownership if its waiter drops.
    ///
    /// # Errors
    /// Returns operation errors or cancellation.
    pub async fn blocking<T, F>(&self, span: Span, action: F) -> Result<T, CapabilityError>
    where
        T: Send + 'static,
        F: FnOnce(&IoCancellation) -> Result<T, CapabilityError> + Send + 'static,
    {
        let permit = self
            .0
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CapabilityError::new("I/O execution is closed", span))?;
        let state = Arc::new(AtomicU8::new(0));
        let guard = WorkGuard(state.clone());
        let cancellation = IoCancellation {
            state: state.clone(),
            context: self.clone(),
            span,
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut jobs = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            jobs.retain(|(job, _)| !job.is_finished());
            cancellation.check()?;
            let handle = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let result = cancellation.check().and_then(|()| action(&cancellation));
                let _ = sender.send(result);
            });
            jobs.push((handle, state));
        }
        let result = receiver
            .await
            .map_err(|_| CapabilityError::new("I/O worker failed", span))?;
        drop(guard);
        result
    }

    /// Join started work and remove incomplete outputs before an entry is reported.
    ///
    /// # Errors
    /// Returns a worker or temporary cleanup error.
    pub async fn cleanup(&self) -> Result<(), CapabilityError> {
        self.cancel();
        self.join_workers().await;
        let paths = std::mem::take(
            &mut *self
                .0
                .temporary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if !paths.is_empty() {
            let context = self.clone();
            let mut jobs = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let worker = tokio::task::spawn_blocking(move || {
                let mut failed = false;
                for path in paths {
                    failed |= std::fs::remove_file(path)
                        .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound);
                }
                if failed {
                    context.0.cleanup_failed.store(true, Ordering::Release);
                }
            });
            // Cleanup itself remains execution-owned if this cleanup future drops.
            // It must run after cancellation, so mark it past the commit point.
            jobs.push((worker, Arc::new(AtomicU8::new(2))));
        }
        self.join_workers().await;
        if self.0.cleanup_failed.load(Ordering::Acquire) {
            Err(CapabilityError::new(
                "failed to clean up execution I/O",
                Span::default(),
            ))
        } else {
            Ok(())
        }
    }

    async fn join_workers(&self) {
        std::future::poll_fn(|cx| {
            let mut jobs = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            jobs.retain_mut(|(job, _)| match Pin::new(job).poll(cx) {
                std::task::Poll::Pending => true,
                std::task::Poll::Ready(result) => {
                    if result.is_err() {
                        self.0.cleanup_failed.store(true, Ordering::Release);
                    }
                    false
                }
            });
            if jobs.is_empty() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
    }
}

struct WorkGuard(Arc<AtomicU8>);

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let _ = self
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
}

pub struct IoCancellation {
    state: Arc<AtomicU8>,
    context: IoContext,
    span: Span,
}

impl IoCancellation {
    /// # Errors
    /// Returns an error after cancellation.
    pub fn check(&self) -> Result<(), CapabilityError> {
        if self.context.0.cancelled.load(Ordering::Acquire)
            || self.state.load(Ordering::Acquire) == 1
        {
            Err(CapabilityError::new("I/O operation cancelled", self.span))
        } else {
            Ok(())
        }
    }

    /// Establish the publication point before changing the destination.
    /// # Errors
    /// Returns an error if cancellation won the race.
    pub fn commit(&self) -> Result<(), CapabilityError> {
        self.check()?;
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| CapabilityError::new("I/O operation cancelled", self.span))
    }
}

pub type ChunkFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Arc<[u8]>>, CapabilityError>> + Send + 'a>>;
pub type ReaderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn ByteReader>, CapabilityError>> + Send + 'a>>;

pub trait ByteReader: Send {
    fn next_chunk(&mut self) -> ChunkFuture<'_>;
}

pub trait SourceFactory: Send + Sync {
    fn open(&self, context: &IoContext) -> ReaderFuture<'_>;
}

/// An opaque source whose aliases share one consumption claim.
pub struct ByteSource {
    claimed: AtomicBool,
    factory: Box<dyn SourceFactory>,
    owner: IoContext,
    pub span: Span,
}

impl ByteSource {
    #[must_use]
    pub fn new(factory: Box<dyn SourceFactory>, span: Span, owner: &IoContext) -> Self {
        Self {
            claimed: AtomicBool::new(false),
            factory,
            owner: owner.clone(),
            span,
        }
    }

    /// # Errors
    /// Returns an error on repeated/concurrent consumption or acquisition failure.
    pub async fn open(&self, context: &IoContext) -> Result<Box<dyn ByteReader>, CapabilityError> {
        if !Arc::ptr_eq(&self.owner.0, &context.0) {
            return Err(CapabilityError::new(
                "byte source belongs to another execution",
                self.span,
            ));
        }
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                CapabilityError::new(
                    "byte source already consumed; construct a fresh source",
                    self.span,
                )
            })?;
        self.factory.open(context).await
    }
}

impl fmt::Debug for ByteSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ByteSource(<opaque>)")
    }
}

impl PartialEq for ByteSource {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    #[test]
    fn dropped_waiter_and_cleanup_retain_worker_ownership() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let context = IoContext::new(PathBuf::from("."));
                let published = Arc::new(AtomicBool::new(false));
                let worker_published = published.clone();
                let finished = Arc::new(AtomicBool::new(false));
                let worker_finished = finished.clone();
                let (started, mut started_rx) = oneshot::channel();
                let (release, release_rx) = std::sync::mpsc::channel();
                let mut operation = Box::pin(context.blocking(Span::default(), move |cancel| {
                    started.send(()).unwrap();
                    release_rx.recv().unwrap();
                    let result = cancel.commit();
                    if result.is_ok() {
                        worker_published.store(true, Ordering::Release);
                    }
                    worker_finished.store(true, Ordering::Release);
                    result
                }));
                std::future::poll_fn(|cx| {
                    assert!(operation.as_mut().poll(cx).is_pending());
                    match Pin::new(&mut started_rx).poll(cx) {
                        Poll::Ready(result) => {
                            result.unwrap();
                            Poll::Ready(())
                        }
                        Poll::Pending => Poll::Pending,
                    }
                })
                .await;
                drop(operation);
                let mut cleanup = Box::pin(context.cleanup());
                std::future::poll_fn(|cx| {
                    assert!(cleanup.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(cleanup);
                release.send(()).unwrap();
                context.cleanup().await.unwrap();
                assert!(finished.load(Ordering::Acquire));
                assert!(!published.load(Ordering::Acquire));
                assert!(context.blocking(Span::default(), |_| Ok(())).await.is_err());
            });
    }
}
