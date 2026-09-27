use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mettle_capability::{
    ByteReader, ByteSource, Capability, CapabilityError, ChunkFuture, IoContext, Object,
    ReaderFuture, SourceFactory, Span, Value,
};

use super::HttpCapability;

fn deferred_server(
    body: &'static [u8],
) -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    let (release, released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let content_length = std::str::from_utf8(&headers)
            .unwrap()
            .lines()
            .find_map(|line| {
                let line = line.to_ascii_lowercase();
                line.strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        assert!(content_length < 1024);
        let mut uploaded = vec![0; content_length];
        socket.read_exact(&mut uploaded).unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = socket.write_all(body);
    });
    (url, release, server)
}

fn chunked_server(
    body: &'static [u8],
) -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    let (release, released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
        write!(socket, "{:x}\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
        socket.write_all(b"\r\n0\r\n\r\n").unwrap();
    });
    (url, release, server)
}
fn async_test(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}
async fn streamed(url: String, context: &IoContext, options: Object) -> Value {
    let mut settings = Object::from([
        ("stream".to_owned(), Value::Boolean(true)),
        (
            "timeout".to_owned(),
            Value::Duration(Duration::from_secs(3)),
        ),
    ]);
    settings.extend(options);
    HttpCapability::new()
        .invoke_with_context(
            0,
            vec![Value::String(url)],
            settings,
            Span::new(10, 20),
            context,
        )
        .await
        .unwrap()
}
async fn field(value: &Value, name: &str) -> Result<Value, CapabilityError> {
    let Value::Deferred(field) = &value.as_object().unwrap()[name] else {
        panic!("deferred field")
    };
    field.0.resolve(Span::new(30, 40)).await
}

#[test]
fn streamed_headers_return_before_body_and_capture_is_shared() {
    let (url, release, server) = deferred_server(b"{\"ok\":true}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        // The server cannot send its body until this call returns.
        let response = streamed(url, &context, Object::new()).await;
        assert_eq!(response.as_object().unwrap()["status"], Value::Integer(200));
        assert!(
            !HttpCapability::new()
                .observed_result(0, &response)
                .contains_source()
        );
        assert!(
            HttpCapability::new()
                .report(0, &response)
                .unwrap()
                .payload
                .is_none()
        );
        release.send(()).unwrap();
        let decoded = field(&response, "body").await.unwrap();
        assert_eq!(decoded.as_object().unwrap()["ok"], Value::Boolean(true));
        let bytes = field(&response, "bodyBytes").await.unwrap();
        assert_eq!(field(&response, "body").await.unwrap(), decoded);
        assert_eq!(bytes, Value::Bytes(Arc::from(b"{\"ok\":true}".as_slice())));
        let Value::Source(chunks) = &response.as_object().unwrap()["chunks"] else {
            panic!()
        };
        assert!(chunks.open(&context).await.is_err());
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

#[test]
fn raw_chunks_are_single_consumer_and_exclude_capture() {
    let (url, release, server) = deferred_server(b"{\"ok\":true}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = streamed(url, &context, Object::new()).await;
        let Value::Source(chunks) = &response.as_object().unwrap()["chunks"] else {
            panic!()
        };
        let mut reader = chunks.open(&context).await.unwrap();
        assert!(chunks.open(&context).await.is_err());
        assert!(field(&response, "body").await.is_err());
        release.send(()).unwrap();
        let mut received = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received, b"{\"ok\":true}");
        assert!(field(&response, "bodyBytes").await.is_err());
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

#[test]
fn close_is_idempotent_and_entry_cleanup_invalidates_unread_aliases() {
    for explicit in [false, true] {
        let (url, release, server) = deferred_server(b"{}");
        async_test(async {
            let context = IoContext::new(std::env::temp_dir());
            let response = streamed(url, &context, Object::new()).await;
            if explicit {
                let Value::Deferred(close) = &response.as_object().unwrap()["close"] else {
                    panic!()
                };
                close.0.call(Span::default()).await.unwrap();
                close.0.call(Span::default()).await.unwrap();
            } else {
                context.cleanup().await.unwrap();
            }
            assert!(field(&response, "body").await.is_err());
            release.send(()).unwrap();
            context.cleanup().await.unwrap();
        });
        server.join().unwrap();
    }
}

#[test]
fn streamed_capture_and_transfer_bounds_are_distinct() {
    let (url, release, server) = deferred_server(b"{\"ok\":true}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = streamed(
            url,
            &context,
            Object::from([("maxCaptureBytes".to_owned(), Value::Integer(2))]),
        )
        .await;
        release.send(()).unwrap();
        assert!(
            field(&response, "bodyBytes")
                .await
                .unwrap_err()
                .message
                .contains("maxCaptureBytes")
        );
        assert!(field(&response, "body").await.is_err());
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

#[test]
fn unknown_length_transfer_limit_is_enforced_during_raw_consumption() {
    let (url, release, server) = chunked_server(b"abcdefgh");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = streamed(
            url,
            &context,
            Object::from([("maxResponseBytes".to_owned(), Value::Integer(3))]),
        )
        .await;
        let Value::Source(chunks) = &response.as_object().unwrap()["chunks"] else {
            panic!()
        };
        let mut reader = chunks.open(&context).await.unwrap();
        release.send(()).unwrap();
        assert!(
            reader
                .next_chunk()
                .await
                .unwrap_err()
                .message
                .contains("maxResponseBytes")
        );
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

#[test]
fn timeout_continues_after_headers_and_close_interrupts_pending_capture() {
    let (url, release, server) = deferred_server(b"{}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = streamed(
            url,
            &context,
            Object::from([(
                "timeout".to_owned(),
                Value::Duration(Duration::from_millis(500)),
            )]),
        )
        .await;
        let result = field(&response, "body").await;
        assert!(result.unwrap_err().message.contains("timeout"));
        release.send(()).unwrap();
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();

    let (url, release, server) = deferred_server(b"{}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = streamed(url, &context, Object::new()).await;
        let mut acquisition = Box::pin(field(&response, "body"));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(acquisition.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let Value::Deferred(close) = &response.as_object().unwrap()["close"] else {
            panic!()
        };
        close.0.call(Span::default()).await.unwrap();
        assert!(acquisition.await.unwrap_err().message.contains("closed"));
        release.send(()).unwrap();
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

#[test]
fn sensitive_request_payload_protects_both_deferred_and_buffered_response_content() {
    let (url, release, server) = deferred_server(b"{\"echo\":\"private-payload\"}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        let response = HttpCapability::new()
            .invoke_with_context(
                1,
                vec![Value::String(url)],
                Object::from([
                    ("stream".to_owned(), Value::Boolean(true)),
                    (
                        "body".to_owned(),
                        Value::String("private-payload".to_owned()).sensitive(),
                    ),
                ]),
                Span::default(),
                &context,
            )
            .await
            .unwrap();
        assert!(response.as_object().unwrap()["chunks"].contains_sensitive());
        release.send(()).unwrap();
        assert!(
            field(&response, "bodyBytes")
                .await
                .unwrap()
                .contains_sensitive()
        );
        assert!(field(&response, "body").await.unwrap().contains_sensitive());
        assert!(
            !HttpCapability::new()
                .report(1, &response)
                .unwrap()
                .outcome
                .contains("private-payload")
        );
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();

    let (url, release, server) = deferred_server(b"{\"echo\":\"private-payload\"}");
    async_test(async {
        let context = IoContext::new(std::env::temp_dir());
        release.send(()).unwrap();
        let response = HttpCapability::new()
            .invoke_with_context(
                1,
                vec![Value::String(url)],
                Object::from([(
                    "body".to_owned(),
                    Value::String("private-payload".to_owned()).sensitive(),
                )]),
                Span::default(),
                &context,
            )
            .await
            .unwrap();
        assert!(response.as_object().unwrap()["body"].contains_sensitive());
        assert!(response.as_object().unwrap()["bodyBytes"].contains_sensitive());
        context.cleanup().await.unwrap();
    });
    server.join().unwrap();
}

struct PendingFactory(Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>);
struct PendingReader(Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>);

impl SourceFactory for PendingFactory {
    fn open(&self, _: &IoContext) -> ReaderFuture<'_> {
        Box::pin(async { Ok(Box::new(PendingReader(self.0.clone())) as Box<dyn ByteReader>) })
    }
}
impl ByteReader for PendingReader {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(std::future::pending())
    }
}
impl Drop for PendingReader {
    fn drop(&mut self) {
        if let Some(sender) = self.0.lock().unwrap().take() {
            let _ = sender.send(());
        }
    }
}

#[test]
fn early_final_response_stops_a_blocked_producer_and_remains_inspectable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/upload", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 65_536);
        }
        socket
            .write_all(
                b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let _ = socket.shutdown(std::net::Shutdown::Write);
        let _ = std::io::copy(&mut socket, &mut std::io::sink());
    });
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let context = IoContext::new(std::env::temp_dir());
            let (sender, dropped) = tokio::sync::oneshot::channel();
            let input = Value::Source(Arc::new(ByteSource::new(
                Box::new(PendingFactory(Arc::new(Mutex::new(Some(sender))))),
                Span::default(),
                &context,
            )));
            let result = HttpCapability::new()
                .invoke_with_context(
                    1,
                    vec![Value::String(url)],
                    Object::from([
                        ("body".into(), input),
                        ("timeout".into(), Value::Duration(Duration::from_secs(3))),
                    ]),
                    Span::default(),
                    &context,
                )
                .await
                .unwrap();
            assert_eq!(result.as_object().unwrap()["status"], Value::Integer(413));
            tokio::time::timeout(Duration::from_secs(3), dropped)
                .await
                .unwrap()
                .unwrap();
            context.cleanup().await.unwrap();
        });
    server.join().unwrap();
}

struct FailingFactory;
struct FailingReader;
impl SourceFactory for FailingFactory {
    fn open(&self, _: &IoContext) -> ReaderFuture<'_> {
        Box::pin(async { Ok(Box::new(FailingReader) as Box<dyn ByteReader>) })
    }
}
impl ByteReader for FailingReader {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async {
            Err(CapabilityError::new(
                "fixture source failed",
                Span::new(12, 23),
            ))
        })
    }
}

#[test]
fn source_errors_preserve_their_diagnostic_and_source_span() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/upload", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = std::io::copy(&mut socket, &mut std::io::sink());
    });
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let context = IoContext::new(std::env::temp_dir());
            let input = Value::Source(Arc::new(ByteSource::new(
                Box::new(FailingFactory),
                Span::new(12, 23),
                &context,
            )));
            let error = HttpCapability::new()
                .invoke_with_context(
                    1,
                    vec![Value::String(url)],
                    Object::from([("body".into(), input)]),
                    Span::default(),
                    &context,
                )
                .await
                .unwrap_err();
            assert_eq!(error.message, "fixture source failed");
            assert_eq!(error.span, Span::new(12, 23));
            context.cleanup().await.unwrap();
        });
    server.join().unwrap();
}
