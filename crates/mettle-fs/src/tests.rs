use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use mettle_capability::{
    ByteReader, ByteSource, CHUNK_BYTES, Capability, ChunkFuture, IoContext, Object, ReaderFuture,
    SourceFactory, Span, Value,
};

use super::FsCapability;

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mettle-fs-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("temporary directory");
        Self(path)
    }
    fn context(&self) -> IoContext {
        IoContext::new(self.0.clone())
    }
    fn assert_no_temporary(&self) {
        assert!(!std::fs::read_dir(&self.0).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mettle-")
        }));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove temporary directory");
    }
}
fn run(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}
async fn call(
    context: &IoContext,
    op: usize,
    args: Vec<Value>,
    options: Object,
) -> Result<Value, mettle_capability::CapabilityError> {
    FsCapability
        .invoke_with_context(op, args, options, Span::default(), context)
        .await
}
fn path(name: &str) -> Vec<Value> {
    vec![Value::String(name.to_owned())]
}
fn source(value: &Value) -> &Arc<ByteSource> {
    let Value::Source(source) = value.revealed() else {
        panic!("expected source")
    };
    source
}

#[test]
fn complete_reads_are_bounded_reusable_and_utf8_checked() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        std::fs::write(fixture.0.join("text"), "Hello 🦀").unwrap();
        std::fs::write(fixture.0.join("invalid"), [0xff]).unwrap();
        let bytes = call(&context, 0, path("text"), Object::new())
            .await
            .unwrap();
        let text = call(&context, 1, path("text"), Object::new())
            .await
            .unwrap();
        assert_eq!(text, Value::String("Hello 🦀".into()));
        assert_eq!(bytes.clone(), bytes);
        assert!(
            call(&context, 1, path("invalid"), Object::new())
                .await
                .unwrap_err()
                .message
                .contains("UTF-8")
        );
        assert!(
            call(
                &context,
                0,
                path("text"),
                Object::from([("maxBytes".into(), Value::Integer(2))])
            )
            .await
            .unwrap_err()
            .message
            .contains("maxBytes")
        );
        assert!(
            call(&context, 0, path("."), Object::new())
                .await
                .unwrap_err()
                .message
                .contains("regular file")
        );
        context.cleanup().await.unwrap();
    });
}

#[test]
fn sources_are_lazy_single_consumer_and_owned_by_the_execution() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        let value = call(&context, 2, path("later"), Object::new())
            .await
            .unwrap();
        std::fs::write(fixture.0.join("later"), vec![7; CHUNK_BYTES * 2 + 9]).unwrap();
        let other = fixture.context();
        assert!(
            source(&value)
                .open(&other)
                .await
                .err()
                .unwrap()
                .message
                .contains("another execution")
        );
        let alias = value.clone();
        let mut reader = source(&value).open(&context).await.unwrap();
        assert!(
            source(&alias)
                .open(&context)
                .await
                .err()
                .unwrap()
                .message
                .contains("already consumed")
        );
        let mut count = 0;
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            assert!(chunk.len() <= CHUNK_BYTES);
            count += chunk.len();
        }
        assert_eq!(count, CHUNK_BYTES * 2 + 9);
        drop(reader);
        context.cleanup().await.unwrap();
    });
}

#[test]
fn changing_files_are_limited_during_consumption() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        std::fs::write(fixture.0.join("growing"), vec![0; CHUNK_BYTES]).unwrap();
        let value = call(
            &context,
            2,
            path("growing"),
            Object::from([(
                "maxBytes".into(),
                Value::Integer(i64::try_from(CHUNK_BYTES).unwrap()),
            )]),
        )
        .await
        .unwrap();
        let mut reader = source(&value).open(&context).await.unwrap();
        reader.next_chunk().await.unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.0.join("growing"))
            .unwrap()
            .write_all(b"extra")
            .unwrap();
        assert!(
            reader
                .next_chunk()
                .await
                .unwrap_err()
                .message
                .contains("maxBytes")
        );
        drop(reader);
        context.cleanup().await.unwrap();
    });
}

#[test]
fn writes_publish_complete_values_and_streams_and_preserve_existing_files() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        std::fs::write(fixture.0.join("input"), "payload").unwrap();
        let value = call(&context, 2, path("input"), Object::new())
            .await
            .unwrap();
        let mut args = path("output");
        args.push(value);
        let result = call(&context, 3, args, Object::new()).await.unwrap();
        assert_eq!(
            result.as_object().unwrap()["bytesWritten"],
            Value::Integer(7)
        );
        let mut args = path("output");
        args.push(Value::String("new".into()));
        assert!(
            call(&context, 3, args.clone(), Object::new())
                .await
                .unwrap_err()
                .message
                .contains("already exists")
        );
        assert_eq!(std::fs::read(fixture.0.join("output")).unwrap(), b"payload");
        call(
            &context,
            3,
            args,
            Object::from([("overwrite".into(), Value::Boolean(true))]),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(fixture.0.join("output")).unwrap(), b"new");
        let mut args = path("output");
        args.push(Value::String("too long".into()));
        assert!(
            call(
                &context,
                3,
                args,
                Object::from([
                    ("overwrite".into(), Value::Boolean(true)),
                    ("maxBytes".into(), Value::Integer(2))
                ])
            )
            .await
            .is_err()
        );
        context.cleanup().await.unwrap();
        assert_eq!(std::fs::read(fixture.0.join("output")).unwrap(), b"new");
        fixture.assert_no_temporary();
    });
}

#[test]
fn concurrent_writers_have_one_winner_without_overwrite() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        let args = vec![
            Value::String("destination".into()),
            Value::String("data".into()),
        ];
        let mut first = Box::pin(call(&context, 3, args.clone(), Object::new()));
        let mut second = Box::pin(call(&context, 3, args, Object::new()));
        let mut a = None;
        let mut b = None;
        std::future::poll_fn(|cx| {
            if a.is_none()
                && let Poll::Ready(result) = first.as_mut().poll(cx)
            {
                a = Some(result);
            }
            if b.is_none()
                && let Poll::Ready(result) = second.as_mut().poll(cx)
            {
                b = Some(result);
            }
            if a.is_some() && b.is_some() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        assert_ne!(a.unwrap().is_ok(), b.unwrap().is_ok());
        context.cleanup().await.unwrap();
        fixture.assert_no_temporary();
    });
}

struct PendingSource(Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>);
struct PendingReader {
    first: bool,
    started: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}
impl SourceFactory for PendingSource {
    fn open(&self, _context: &IoContext) -> ReaderFuture<'_> {
        Box::pin(async {
            Ok(Box::new(PendingReader {
                first: true,
                started: self.0.clone(),
            }) as Box<dyn ByteReader>)
        })
    }
}
impl ByteReader for PendingReader {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async move {
            if self.first {
                self.first = false;
                Ok(Some(Arc::from(&b"new"[..])))
            } else {
                if let Some(sender) = self.started.lock().unwrap().take() {
                    sender.send(()).unwrap();
                }
                std::future::pending().await
            }
        })
    }
}

#[test]
fn cancellation_cleans_partial_outputs_without_replacing_destinations() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        std::fs::write(fixture.0.join("destination"), "old").unwrap();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        let input = Value::Source(Arc::new(ByteSource::new(
            Box::new(PendingSource(Arc::new(Mutex::new(Some(sender))))),
            Span::default(),
            &context,
        )));
        let args = vec![Value::String("destination".into()), input];
        let options = Object::from([("overwrite".into(), Value::Boolean(true))]);
        let mut action = Box::pin(call(&context, 3, args, options));
        std::future::poll_fn(|cx| {
            assert!(action.as_mut().poll(cx).is_pending());
            std::pin::Pin::new(&mut receiver)
                .poll(cx)
                .map(|result| result.unwrap())
        })
        .await;
        drop(action);
        context.cleanup().await.unwrap();
        assert_eq!(
            std::fs::read(fixture.0.join("destination")).unwrap(),
            b"old"
        );
        fixture.assert_no_temporary();
    });
}

#[cfg(unix)]
#[test]
fn reads_follow_regular_symlinks_and_writes_reject_destination_symlinks() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        std::fs::write(fixture.0.join("target"), "old").unwrap();
        std::os::unix::fs::symlink("target", fixture.0.join("link")).unwrap();
        assert_eq!(
            call(&context, 1, path("link"), Object::new())
                .await
                .unwrap(),
            Value::String("old".into())
        );
        assert!(
            call(
                &context,
                3,
                vec![Value::String("link".into()), Value::String("new".into())],
                Object::from([("overwrite".into(), Value::Boolean(true))])
            )
            .await
            .unwrap_err()
            .message
            .contains("symlink")
        );
        context.cleanup().await.unwrap();
        assert_eq!(std::fs::read(fixture.0.join("target")).unwrap(), b"old");
    });
}

#[test]
fn sensitive_paths_are_redacted_and_read_values_stay_sensitive() {
    run(async {
        let fixture = Fixture::new();
        let context = fixture.context();
        let secret = vec![Value::String("private-name".into()).sensitive()];
        let error = call(&context, 0, secret.clone(), Object::new())
            .await
            .unwrap_err();
        assert!(!error.message.contains("private-name"));
        std::fs::write(fixture.0.join("private-name"), "secret").unwrap();
        assert!(
            call(&context, 1, secret, Object::new())
                .await
                .unwrap()
                .is_sensitive()
        );
        context.cleanup().await.unwrap();
    });
}
