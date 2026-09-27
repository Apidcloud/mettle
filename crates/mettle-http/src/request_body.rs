//! Pull-based request bodies; no read-ahead task or complete upload capture.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Frame, SizeHint};
use mettle_capability::{ByteReader, CapabilityError, Span};

#[derive(Default)]
pub(crate) struct UploadControl {
    pub stopped: AtomicBool,
    pub error: Mutex<Option<CapabilityError>>,
    waker: Mutex<Option<Waker>>,
}

impl UploadControl {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().expect("upload waker lock").take() {
            waker.wake();
        }
    }
}

pub(crate) struct UploadGuard(pub Arc<UploadControl>);

impl Drop for UploadGuard {
    fn drop(&mut self) {
        self.0.stop();
    }
}

type PendingRead = Pin<
    Box<
        dyn Future<
                Output = (
                    Box<dyn ByteReader>,
                    Result<Option<Arc<[u8]>>, CapabilityError>,
                ),
            > + Send,
    >,
>;

pub(crate) enum RequestBody {
    Full(Full<Bytes>),
    Source {
        reader: Option<Box<dyn ByteReader>>,
        pending: Option<PendingRead>,
        control: Arc<UploadControl>,
        finished: bool,
        max_bytes: usize,
        sent: usize,
        span: Span,
    },
}

impl RequestBody {
    pub fn source(
        reader: Box<dyn ByteReader>,
        control: Arc<UploadControl>,
        max_bytes: usize,
        span: Span,
    ) -> Self {
        Self::Source {
            reader: Some(reader),
            pending: None,
            control,
            finished: false,
            max_bytes,
            sent: 0,
            span,
        }
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = CapabilityError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|result| result.map_err(|never| match never {}))),
            Self::Source {
                reader,
                pending,
                control,
                finished,
                max_bytes,
                sent,
                span,
            } => {
                *control.waker.lock().expect("upload waker lock") = Some(cx.waker().clone());
                if *finished || control.stopped.load(Ordering::Acquire) {
                    *pending = None;
                    *reader = None;
                    *finished = true;
                    return Poll::Ready(None);
                }
                if pending.is_none() {
                    let mut current = reader.take().expect("active source reader");
                    *pending = Some(Box::pin(async move {
                        let result = current.next_chunk().await;
                        (current, result)
                    }));
                }
                let Poll::Ready((current, result)) =
                    pending.as_mut().expect("pending read").as_mut().poll(cx)
                else {
                    return Poll::Pending;
                };
                *pending = None;
                match result {
                    Ok(Some(bytes)) => {
                        if bytes.len() > max_bytes.saturating_sub(*sent) {
                            *finished = true;
                            let error = CapabilityError::new(
                                "streamed request exceeds maxBodyBytes",
                                *span,
                            );
                            *control.error.lock().expect("upload error lock") = Some(error.clone());
                            return Poll::Ready(Some(Err(error)));
                        }
                        *sent += bytes.len();
                        *reader = Some(current);
                        Poll::Ready(Some(Ok(Frame::data(Bytes::copy_from_slice(&bytes)))))
                    }
                    Ok(None) => {
                        *finished = true;
                        Poll::Ready(None)
                    }
                    Err(error) => {
                        *finished = true;
                        *control.error.lock().expect("upload error lock") = Some(error.clone());
                        Poll::Ready(Some(Err(error)))
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_end_stream(),
            Self::Source { finished, .. } => *finished,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => body.size_hint(),
            Self::Source { .. } => SizeHint::default(),
        }
    }
}
