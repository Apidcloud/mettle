use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mettle_capability::{
    ByteReader, CHUNK_BYTES, CapabilityError, ChunkFuture, IoContext, ReaderFuture, SourceFactory,
    Span,
};

use crate::file_error;

pub(crate) struct FileSource {
    pub path: PathBuf,
    pub max_bytes: u64,
    pub timeout: Duration,
    pub span: Span,
    pub sensitive: bool,
}

impl SourceFactory for FileSource {
    fn open(&self, context: &IoContext) -> ReaderFuture<'_> {
        let context = context.clone();
        Box::pin(async move {
            let deadline = Instant::now().checked_add(self.timeout).ok_or_else(|| {
                CapabilityError::new("file source timeout is too large", self.span)
            })?;
            let opening = async {
                let permit = context.reader_permit(self.span).await?;
                let path = self.path.clone();
                let span = self.span;
                let sensitive = self.sensitive;
                let max_bytes = self.max_bytes;
                let file = context
                    .blocking(span, move |cancel| {
                        let metadata = std::fs::metadata(&path)
                            .map_err(|e| file_error(&path, sensitive, "inspect file", &e, span))?;
                        if !metadata.is_file() {
                            return Err(CapabilityError::new(
                                "filesystem source must be a regular file",
                                span,
                            ));
                        }
                        if metadata.len() > max_bytes {
                            return Err(CapabilityError::new(
                                format!("file exceeds maxBytes ({max_bytes})"),
                                span,
                            ));
                        }
                        cancel.check()?;
                        let file = open_file(&path)
                            .map_err(|e| file_error(&path, sensitive, "open file", &e, span))?;
                        if !file
                            .metadata()
                            .map_err(|e| {
                                file_error(&path, sensitive, "inspect open file", &e, span)
                            })?
                            .is_file()
                        {
                            return Err(CapabilityError::new(
                                "filesystem source must be a regular file",
                                span,
                            ));
                        }
                        Ok(file)
                    })
                    .await?;
                Ok(Box::new(FileReader {
                    file: Arc::new(Mutex::new(file)),
                    context,
                    path: self.path.clone(),
                    max_bytes,
                    read: 0,
                    deadline,
                    span,
                    sensitive,
                    finished: false,
                    _permit: permit,
                }) as Box<dyn ByteReader>)
            };
            tokio::time::timeout(self.timeout, opening)
                .await
                .map_err(|_| CapabilityError::new("opening file source timed out", self.span))?
        })
    }
}

fn open_file(path: &std::path::Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    // Avoid blocking on a FIFO if the path changes after the regular-file check.
    // Opened-descriptor metadata is checked again before any reads.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

struct FileReader {
    file: Arc<Mutex<File>>,
    context: IoContext,
    path: PathBuf,
    max_bytes: u64,
    read: u64,
    deadline: Instant,
    span: Span,
    sensitive: bool,
    finished: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl ByteReader for FileReader {
    fn next_chunk(&mut self) -> ChunkFuture<'_> {
        Box::pin(async move {
            if self.finished {
                return Ok(None);
            }
            let Some(remaining) = self.deadline.checked_duration_since(Instant::now()) else {
                return Err(CapabilityError::new("file source timed out", self.span));
            };
            let file = self.file.clone();
            let path = self.path.clone();
            let span = self.span;
            let sensitive = self.sensitive;
            let result = self.context.blocking(span, move |cancel| {
                cancel.check()?;
                let mut bytes = vec![0; CHUNK_BYTES];
                let count = file
                    .lock()
                    .expect("file reader lock")
                    .read(&mut bytes)
                    .map_err(|e| file_error(&path, sensitive, "read file", &e, span))?;
                bytes.truncate(count);
                Ok(bytes)
            });
            let bytes = tokio::time::timeout(remaining, result)
                .await
                .map_err(|_| CapabilityError::new("file source timed out", span))??;
            self.read += u64::try_from(bytes.len()).expect("chunk length");
            if self.read > self.max_bytes {
                return Err(CapabilityError::new(
                    format!("file exceeds maxBytes ({})", self.max_bytes),
                    span,
                ));
            }
            if bytes.is_empty() {
                self.finished = true;
                Ok(None)
            } else {
                Ok(Some(Arc::from(bytes)))
            }
        })
    }
}
