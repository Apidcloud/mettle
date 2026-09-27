//! Pull-based response acquisition with one shared consumption claim and entry ownership.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use hyper::body::Incoming;
use mettle_capability::media_type::MediaType;
use mettle_capability::{
    ByteReader, ByteSource, CapabilityError, CapabilityFuture, ChunkFuture, Deferred,
    DeferredField, IoContext, IoResource, ReaderFuture, SourceFactory, Span, Value,
};
use tokio::sync::{Notify, OwnedSemaphorePermit};

const OPEN: u8 = 0;
const CAPTURING: u8 = 1;
const CHUNKS: u8 = 2;
const COMPLETE: u8 = 3;
const CLOSED: u8 = 4;
type Capture = Result<Value, CapabilityError>;

pub struct ResponseStream {
    incoming: Mutex<Option<Incoming>>,
    state: AtomicU8,
    cancelled: AtomicBool,
    changed: Notify,
    captured: tokio::sync::Mutex<Option<Capture>>,
    headers: hyper::HeaderMap,
    media: Option<MediaType>,
    bodyless: bool,
    deadline: tokio::time::Instant,
    max_transfer: usize,
    max_capture: usize,
    span: Span,
    sensitive: bool,
}

pub struct Settings {
    pub media: Option<MediaType>,
    pub bodyless: bool,
    pub deadline: tokio::time::Instant,
    pub max_transfer: usize,
    pub max_capture: usize,
    pub span: Span,
    pub sensitive: bool,
}

impl ResponseStream {
    pub fn new(
        incoming: Incoming,
        headers: hyper::HeaderMap,
        settings: Settings,
        context: &IoContext,
    ) -> Result<Arc<Self>, CapabilityError> {
        let value = Arc::new(Self {
            incoming: Mutex::new(Some(incoming)),
            state: AtomicU8::new(OPEN),
            cancelled: AtomicBool::new(false),
            changed: Notify::new(),
            captured: tokio::sync::Mutex::new(None),
            headers,
            media: settings.media,
            bodyless: settings.bodyless,
            deadline: settings.deadline,
            max_transfer: settings.max_transfer,
            max_capture: settings.max_capture,
            span: settings.span,
            sensitive: settings.sensitive,
        });
        context.register_resource(&(value.clone() as Arc<dyn IoResource>), settings.span)?;
        Ok(value)
    }
    pub fn field(self: &Arc<Self>, kind: FieldKind) -> Value {
        Value::Deferred(Deferred(Arc::new(Field {
            response: self.clone(),
            kind,
        })))
    }
    pub fn chunks(self: &Arc<Self>, context: &IoContext) -> Value {
        let value = Value::Source(Arc::new(ByteSource::new(
            Box::new(Factory(self.clone())),
            self.span,
            context,
        )));
        if self.sensitive {
            value.sensitive()
        } else {
            value
        }
    }
    fn take(&self) -> Result<Incoming, CapabilityError> {
        self.incoming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                CapabilityError::new("response body is closed or already consumed", self.span)
            })
    }
    async fn capture(self: &Arc<Self>, span: Span) -> Result<Value, CapabilityError> {
        // Concurrent aliases share one cached capture, rather than competing reads.
        let mut cache = self.captured.lock().await;
        if let Some(result) = &*cache {
            return result.clone();
        }
        self.state
            .compare_exchange(OPEN, CAPTURING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                CapabilityError::new(
                    "response body is closed or chunks consumption has already started",
                    span,
                )
            })?;
        let result = async {
            let body = self.take()?;
            let mut reader = Reader { response: self.clone(), body: Some(body), pending: bytes::Bytes::new(), total: 0, permit: None, complete: false };
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.next_chunk().await? {
                if chunk.len() > self.max_capture.saturating_sub(bytes.len()) {
                    return Err(CapabilityError::new(format!("HTTP capture exceeded the {} byte maxCaptureBytes limit; consume response.chunks instead", self.max_capture), span));
                }
                bytes.extend_from_slice(&chunk);
            }
            let value = Value::Bytes(Arc::from(bytes));
            Ok(if self.sensitive { value.sensitive() } else { value })
        }.await;
        *cache = Some(result.clone());
        result
    }
}

impl IoResource for ResponseStream {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.state.store(CLOSED, Ordering::Release);
        self.incoming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.changed.notify_waiters();
    }
    fn active(&self) -> bool {
        matches!(
            self.state.load(Ordering::Acquire),
            OPEN | CAPTURING | CHUNKS
        )
    }
}

#[derive(Clone, Copy)]
pub enum FieldKind {
    Body,
    Bytes,
    Close,
}
struct Field {
    response: Arc<ResponseStream>,
    kind: FieldKind,
}
impl DeferredField for Field {
    fn resolve(&self, span: Span) -> CapabilityFuture<'_> {
        Box::pin(async move {
            if matches!(self.kind, FieldKind::Close) {
                return Err(CapabilityError::new(
                    "call response.close() to abandon its body",
                    span,
                ));
            }
            let bytes = self.response.capture(span).await?;
            if matches!(self.kind, FieldKind::Bytes) {
                return Ok(bytes);
            }
            super::incoming::decode(
                &bytes,
                self.response.media.as_ref(),
                &self.response.headers,
                self.response.bodyless,
                self.response.max_capture,
                span,
            )
        })
    }
    fn call(&self, span: Span) -> CapabilityFuture<'_> {
        Box::pin(async move {
            if !matches!(self.kind, FieldKind::Close) {
                return Err(CapabilityError::new("response field is not callable", span));
            }
            self.response.cancel();
            Ok(Value::Null)
        })
    }
    fn snapshot(&self) -> Value {
        Value::String(
            if matches!(self.kind, FieldKind::Close) {
                "<response.close()>"
            } else {
                "<deferred response body>"
            }
            .to_owned(),
        )
    }
}

struct Factory(Arc<ResponseStream>);
impl SourceFactory for Factory {
    fn open(&self, context: &IoContext) -> ReaderFuture<'_> {
        let context = context.clone();
        Box::pin(async move {
            let permit = context.reader_permit(self.0.span).await?;
            self.0
                .state
                .compare_exchange(OPEN, CHUNKS, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| {
                    CapabilityError::new(
                        "response body was already captured, consumed, or closed",
                        self.0.span,
                    )
                })?;
            let body = self.0.take()?;
            Ok(Box::new(Reader {
                response: self.0.clone(),
                body: Some(body),
                pending: bytes::Bytes::new(),
                total: 0,
                permit: Some(permit),
                complete: false,
            }) as Box<dyn ByteReader>)
        })
    }
}

struct Reader {
    response: Arc<ResponseStream>,
    body: Option<Incoming>,
    pending: bytes::Bytes,
    total: usize,
    permit: Option<OwnedSemaphorePermit>,
    complete: bool,
}
impl Reader {
    async fn read(&mut self) -> Result<Option<Arc<[u8]>>, CapabilityError> {
        loop {
            // Register notification before inspecting cancellation to avoid lost wakeups.
            let changed = self.response.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.response.cancelled.load(Ordering::Acquire) {
                return Err(CapabilityError::new(
                    "HTTP response body was closed",
                    self.response.span,
                ));
            }
            if tokio::time::Instant::now() >= self.response.deadline {
                return Err(CapabilityError::new(
                    "HTTP response body exceeded its request timeout",
                    self.response.span,
                ));
            }
            if self.complete {
                return Ok(None);
            }
            if !self.pending.is_empty() {
                let piece = self
                    .pending
                    .split_to(self.pending.len().min(mettle_capability::CHUNK_BYTES));
                return Ok(Some(Arc::from(piece.as_ref())));
            }
            let Some(body) = self.body.as_mut() else {
                return Ok(None);
            };
            let frame = tokio::select! {
                biased;
                () = &mut changed => return Err(CapabilityError::new("HTTP response body was closed", self.response.span)),
                result = tokio::time::timeout_at(self.response.deadline, body.frame()) => result.map_err(|_| CapabilityError::new("HTTP response body exceeded its request timeout", self.response.span))?,
            };
            let Some(frame) = frame else {
                self.complete = true;
                self.body.take();
                self.permit.take();
                self.response.state.store(COMPLETE, Ordering::Release);
                return Ok(None);
            };
            let frame = frame.map_err(|error| {
                CapabilityError::new(
                    format!("HTTP response body failed: {error}"),
                    self.response.span,
                )
            })?;
            if let Ok(chunk) = frame.into_data() {
                self.total = self.total.saturating_add(chunk.len());
                if self.total > self.response.max_transfer {
                    return Err(CapabilityError::new(
                        format!(
                            "HTTP response exceeded the {} byte maxResponseBytes limit",
                            self.response.max_transfer
                        ),
                        self.response.span,
                    ));
                }
                self.pending = chunk;
            }
        }
    }
}
impl ByteReader for Reader {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async move {
            let result = self.read().await;
            if result.is_err() {
                self.body.take();
                self.pending = bytes::Bytes::new();
                self.permit.take();
                self.response.cancel();
            }
            result
        })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        if !self.complete {
            self.response.cancel();
        }
    }
}
