use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mettle_capability::{CHUNK_BYTES, CapabilityError, IoContext, Object, Span, Value};

use crate::file_error;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn validate_destination(path: &Path, span: Span) -> Result<(), CapabilityError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            Err(CapabilityError::new(
                "filesystem destination must be a regular file, not a symlink",
                span,
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CapabilityError::new(
            format!("could not inspect filesystem destination: {error}"),
            span,
        )),
    }
}

pub(crate) async fn write(
    context: &IoContext,
    path: PathBuf,
    payload: &Value,
    overwrite: bool,
    max_bytes: u64,
    span: Span,
    sensitive: bool,
) -> Result<Value, CapabilityError> {
    let source = match payload.revealed() {
        Value::Source(source) => Some(source),
        Value::Bytes(_) | Value::String(_) => None,
        _ => {
            return Err(CapabilityError::new(
                "fs.write content must be a string, bytes, or byte source",
                span,
            ));
        }
    };
    let (temporary, file) =
        create_temporary(context, path.clone(), overwrite, sensitive, span).await?;
    let mut source = if let Some(source) = source {
        Some(source.open(context).await?)
    } else {
        None
    };
    let mut count = 0_u64;
    if let Some(reader) = &mut source {
        while let Some(chunk) = reader.next_chunk().await? {
            count = count
                .checked_add(u64::try_from(chunk.len()).expect("chunk length"))
                .filter(|value| *value <= max_bytes)
                .ok_or_else(|| {
                    CapabilityError::new(format!("write exceeds maxBytes ({max_bytes})"), span)
                })?;
            write_chunk(context, file.clone(), chunk, span).await?;
        }
    } else {
        let bytes = match payload.revealed() {
            Value::String(text) => text.as_bytes(),
            Value::Bytes(bytes) => bytes,
            _ => unreachable!(),
        };
        count = u64::try_from(bytes.len()).expect("value length");
        if count > max_bytes {
            return Err(CapabilityError::new(
                format!("write exceeds maxBytes ({max_bytes})"),
                span,
            ));
        }
        for chunk in bytes.chunks(CHUNK_BYTES) {
            write_chunk(context, file.clone(), Arc::from(chunk), span).await?;
        }
    }
    context
        .blocking(span, move |_| {
            drop(file);
            Ok(())
        })
        .await?;
    publish(context, temporary, path, overwrite, sensitive, span).await?;
    Ok(Value::Object(Object::from([(
        "bytesWritten".to_owned(),
        Value::Integer(
            i64::try_from(count).map_err(|_| CapabilityError::new("write size overflow", span))?,
        ),
    )])))
}

async fn create_temporary(
    context: &IoContext,
    path: PathBuf,
    overwrite: bool,
    sensitive: bool,
    span: Span,
) -> Result<(PathBuf, Arc<Mutex<File>>), CapabilityError> {
    let directory = path
        .parent()
        .ok_or_else(|| CapabilityError::new("invalid destination path", span))?
        .to_path_buf();
    let create_context = context.clone();
    let destination = path.clone();
    context
        .blocking(span, move |cancel| {
            validate_destination(&destination, span)?;
            if !overwrite && destination.exists() {
                return Err(CapabilityError::new(
                    "destination already exists; use overwrite: true",
                    span,
                ));
            }
            for _ in 0..64 {
                cancel.check()?;
                let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let temporary = directory.join(format!(".mettle-{}-{id}.tmp", std::process::id()));
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                {
                    Ok(file) => {
                        if let Err(error) =
                            create_context.register_temporary(temporary.clone(), span)
                        {
                            drop(file);
                            let _ = std::fs::remove_file(&temporary);
                            return Err(error);
                        }
                        return Ok((temporary, Arc::new(Mutex::new(file))));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(file_error(
                            &destination,
                            sensitive,
                            "create output",
                            &error,
                            span,
                        ));
                    }
                }
            }
            Err(CapabilityError::new(
                "could not allocate temporary output",
                span,
            ))
        })
        .await
}

async fn publish(
    context: &IoContext,
    temporary: PathBuf,
    path: PathBuf,
    overwrite: bool,
    sensitive: bool,
    span: Span,
) -> Result<(), CapabilityError> {
    let published = temporary.clone();
    context
        .blocking(span, move |cancel| {
            validate_destination(&path, span)?;
            cancel.commit()?;
            if overwrite {
                std::fs::rename(&published, &path)
                    .map_err(|e| file_error(&path, sensitive, "publish output", &e, span))?;
            } else {
                std::fs::hard_link(&published, &path).map_err(|e| {
                    file_error(
                        &path,
                        sensitive,
                        "publish output (destination must not exist)",
                        &e,
                        span,
                    )
                })?;
                std::fs::remove_file(&published).map_err(|e| {
                    file_error(&published, sensitive, "remove temporary output", &e, span)
                })?;
            }
            Ok(())
        })
        .await?;
    context.forget_temporary(&temporary);
    Ok(())
}

async fn write_chunk(
    context: &IoContext,
    file: Arc<Mutex<File>>,
    bytes: Arc<[u8]>,
    span: Span,
) -> Result<(), CapabilityError> {
    context
        .blocking(span, move |cancel| {
            cancel.check()?;
            file.lock()
                .expect("output file lock")
                .write_all(&bytes)
                .map_err(|error| {
                    CapabilityError::new(format!("could not write temporary output: {error}"), span)
                })
        })
        .await
}
