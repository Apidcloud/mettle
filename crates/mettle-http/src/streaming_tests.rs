use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mettle_capability::{
    ByteReader, ByteSource, Capability, CapabilityError, ChunkFuture, IoContext, Object,
    ReaderFuture, SourceFactory, Span, Value,
};

use super::HttpCapability;

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
