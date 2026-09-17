use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use flow_compiler::find_definition;
use flow_syntax::Span;
use serde_json::{Value, json};

use super::{CliError, load_project_with_overlays};

pub(super) fn run() -> Result<(), CliError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut server = Server::default();

    loop {
        let message = read_message(&mut reader).map_err(|error| io_failure(&error))?;
        let Some(message) = message else {
            return Ok(());
        };
        let should_exit = server
            .handle(&message, &mut writer)
            .map_err(|error| io_failure(&error))?;
        if should_exit {
            return Ok(());
        }
    }
}

fn io_failure(error: &io::Error) -> CliError {
    eprintln!("error: Flow language server I/O failed: {error}");
    CliError::Failure
}

#[derive(Default)]
struct Server {
    documents: HashMap<PathBuf, String>,
    shutdown: bool,
}

impl Server {
    fn handle(&mut self, message: &Value, writer: &mut impl Write) -> io::Result<bool> {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();

        match (method, id) {
            (Some("initialize"), Some(id)) => write_result(
                writer,
                &id,
                &json!({
                    "capabilities": {
                        "definitionProvider": true,
                        "textDocumentSync": {
                            "openClose": true,
                            "change": 1
                        }
                    },
                    "serverInfo": {
                        "name": "flow",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )?,
            (Some("shutdown"), Some(id)) => {
                self.shutdown = true;
                write_result(writer, &id, &Value::Null)?;
            }
            (Some("textDocument/definition"), Some(id)) => {
                let result = message
                    .get("params")
                    .and_then(|params| self.definition(params));
                write_result(writer, &id, &result.unwrap_or(Value::Null))?;
            }
            (Some("textDocument/didOpen"), None) => self.did_open(message),
            (Some("textDocument/didChange"), None) => self.did_change(message),
            (Some("textDocument/didClose"), None) => self.did_close(message),
            (Some("exit"), None) => return Ok(true),
            (Some(_), Some(id)) => write_error(writer, &id, -32_601, "method not found")?,
            _ => {}
        }
        Ok(false)
    }

    fn did_open(&mut self, message: &Value) {
        let Some(document) = message
            .pointer("/params/textDocument")
            .and_then(Value::as_object)
        else {
            return;
        };
        let (Some(uri), Some(text)) = (
            document.get("uri").and_then(Value::as_str),
            document.get("text").and_then(Value::as_str),
        ) else {
            return;
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.insert(normalize_path(path), text.to_owned());
        }
    }

    fn did_change(&mut self, message: &Value) {
        let Some(uri) = message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)
        else {
            return;
        };
        let Some(text) = message
            .pointer("/params/contentChanges/0/text")
            .and_then(Value::as_str)
        else {
            return;
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.insert(normalize_path(path), text.to_owned());
        }
    }

    fn did_close(&mut self, message: &Value) {
        let Some(uri) = message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)
        else {
            return;
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.remove(&normalize_path(path));
        }
    }

    fn definition(&self, params: &Value) -> Option<Value> {
        let uri = params.pointer("/textDocument/uri")?.as_str()?;
        let path = normalize_path(file_uri_to_path(uri)?);
        let line = usize::try_from(params.pointer("/position/line")?.as_u64()?).ok()?;
        let character = usize::try_from(params.pointer("/position/character")?.as_u64()?).ok()?;
        let project = load_project_with_overlays(&path, &self.documents).ok()?;
        let source = project
            .sources
            .iter()
            .position(|source| normalize_path(source.path.clone()) == path)?;
        let byte = byte_offset(&project.sources[source].text, line, character)?;
        let target = find_definition(&project.program, source, byte)?;
        let target_source = project.sources.get(target.source)?;
        Some(json!({
            "uri": path_to_file_uri(&target_source.path),
            "range": span_range(&target_source.text, target)
        }))
    }
}

fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.trim().strip_prefix("Content-Length:").map(str::trim) {
            content_length = value.parse::<usize>().ok();
        }
    }
    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut content = vec![0; length];
    reader.read_exact(&mut content)?;
    serde_json::from_slice(&content)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_result(writer: &mut impl Write, id: &Value, result: &Value) -> io::Result<()> {
    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn write_error(writer: &mut impl Write, id: &Value, code: i32, message: &str) -> io::Result<()> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    )
}

fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let content = serde_json::to_vec(message).expect("JSON-RPC responses are serializable");
    write!(writer, "Content-Length: {}\r\n\r\n", content.len())?;
    writer.write_all(&content)?;
    writer.flush()
}

fn byte_offset(source: &str, target_line: usize, target_character: usize) -> Option<usize> {
    let mut line_start = 0;
    for _ in 0..target_line {
        let newline = source[line_start..].find('\n')?;
        line_start += newline + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |offset| line_start + offset);
    let line = &source[line_start..line_end];
    let mut utf16 = 0;
    for (offset, character) in line.char_indices() {
        if utf16 >= target_character {
            return Some(line_start + offset);
        }
        utf16 += character.len_utf16();
    }
    (utf16 == target_character).then_some(line_end)
}

fn span_range(source: &str, span: Span) -> Value {
    let (start_line, start_character) = position(source, span.start);
    let (end_line, end_character) = position(source, span.end);
    json!({
        "start": { "line": start_line, "character": start_character },
        "end": { "line": end_line, "character": end_character }
    })
}

fn position(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let line_start = source[..byte].rfind('\n').map_or(0, |offset| offset + 1);
    let line = source[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let character = source[line_start..byte].encode_utf16().count();
    (line, character)
}

fn normalize_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%' {
            let high = hex(*bytes.get(cursor + 1)?)?;
            let low = hex(*bytes.get(cursor + 2)?)?;
            decoded.push((high << 4) | low);
            cursor += 3;
        } else {
            decoded.push(bytes[cursor]);
            cursor += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(decoded).ok()?))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn path_to_file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(uri, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::{Server, byte_offset, file_uri_to_path, path_to_file_uri, position};
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn project_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("flow-lsp-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).expect("test project should be creatable");
        path
    }

    #[test]
    fn converts_lsp_utf16_positions() {
        let source = "first\n💡value\n";
        assert_eq!(byte_offset(source, 1, 2), Some(10));
        assert_eq!(position(source, 10), (1, 2));
    }

    #[test]
    fn round_trips_file_uris() {
        let path = Path::new("/tmp/flow project/main.flow");
        let uri = path_to_file_uri(path);
        assert_eq!(uri, "file:///tmp/flow%20project/main.flow");
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn navigates_unsaved_cross_file_flow_calls() {
        let directory = project_directory();
        fs::write(directory.join("flow.toml"), "name = \"lsp-test\"\n")
            .expect("manifest should be writable");
        let declaration = directory.join("shared.flow");
        fs::write(&declaration, "namespace shared\nflow helper() = true\n")
            .expect("declaration should be writable");
        let entry = directory.join("main.flow");
        fs::write(&entry, "use namespace shared\nflow main() = false\n")
            .expect("entry should be writable");

        let unsaved = "use namespace shared\nflow main() = helper()\n";
        let mut server = Server::default();
        server.documents.insert(entry.clone(), unsaved.to_owned());
        let result = server
            .definition(&json!({
                "textDocument": { "uri": path_to_file_uri(&entry) },
                "position": { "line": 1, "character": 16 }
            }))
            .expect("definition should resolve");

        assert_eq!(
            result["uri"],
            json!(path_to_file_uri(
                &declaration.canonicalize().expect("path should exist")
            ))
        );
        assert_eq!(
            result["range"]["start"],
            json!({ "line": 1, "character": 5 })
        );
        fs::remove_dir_all(directory).expect("test project should be removable");
    }
}
