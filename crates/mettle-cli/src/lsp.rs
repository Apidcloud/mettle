use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use mettle_compiler::{compile_with_capabilities, find_definition};
use mettle_syntax::{Span, parse};
use serde_json::{Value, json};

use super::{
    CAPABILITIES, CliError, SourceDocument, combine_parsed_sources, find_project_root,
    load_project_documents_with_overlays, load_project_with_overlays,
};

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
    eprintln!("error: Mettle language server I/O failed: {error}");
    CliError::Failure
}

#[derive(Default)]
struct Server {
    documents: HashMap<PathBuf, String>,
    published: HashMap<PathBuf, BTreeSet<PathBuf>>,
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
                        "implementationProvider": true,
                        "textDocumentSync": {
                            "openClose": true,
                            "change": 1,
                            "save": true
                        }
                    },
                    "serverInfo": {
                        "name": "mettle",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )?,
            (Some("shutdown"), Some(id)) => {
                self.shutdown = true;
                write_result(writer, &id, &Value::Null)?;
            }
            (Some("textDocument/definition" | "textDocument/implementation"), Some(id)) => {
                let result = message
                    .get("params")
                    .and_then(|params| self.definition(params));
                write_result(writer, &id, &result.unwrap_or(Value::Null))?;
            }
            (Some("textDocument/didOpen"), None) => self.did_open(message, writer)?,
            (Some("textDocument/didChange"), None) => self.did_change(message, writer)?,
            (Some("textDocument/didClose"), None) => self.did_close(message, writer)?,
            (Some("textDocument/didSave" | "mettle/revalidate"), None) => {
                self.revalidate_all(writer)?;
            }
            (Some("exit"), None) => return Ok(true),
            (Some(_), Some(id)) => write_error(writer, &id, -32_601, "method not found")?,
            _ => {}
        }
        Ok(false)
    }

    fn did_open(&mut self, message: &Value, writer: &mut impl Write) -> io::Result<()> {
        let Some(document) = message
            .pointer("/params/textDocument")
            .and_then(Value::as_object)
        else {
            return Ok(());
        };
        let (Some(uri), Some(text)) = (
            document.get("uri").and_then(Value::as_str),
            document.get("text").and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.insert(normalize_path(path), text.to_owned());
            self.revalidate_all(writer)?;
        }
        Ok(())
    }

    fn did_change(&mut self, message: &Value, writer: &mut impl Write) -> io::Result<()> {
        let Some(uri) = message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let Some(text) = message
            .pointer("/params/contentChanges/0/text")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.insert(normalize_path(path), text.to_owned());
            self.revalidate_all(writer)?;
        }
        Ok(())
    }

    fn did_close(&mut self, message: &Value, writer: &mut impl Write) -> io::Result<()> {
        let Some(uri) = message
            .pointer("/params/textDocument/uri")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        if let Some(path) = file_uri_to_path(uri) {
            self.documents.remove(&normalize_path(path));
            self.revalidate_all(writer)?;
        }
        Ok(())
    }

    fn revalidate_all(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let mut entries = BTreeMap::new();
        for path in self.documents.keys() {
            entries
                .entry(project_scope(path))
                .or_insert_with(|| path.clone());
        }

        let stale_scopes = self
            .published
            .keys()
            .filter(|scope| !entries.contains_key(*scope))
            .cloned()
            .collect::<Vec<_>>();
        for scope in stale_scopes {
            if let Some(paths) = self.published.remove(&scope) {
                for path in paths {
                    publish_diagnostics(writer, &path, &[])?;
                }
            }
        }

        for (scope, entry) in entries {
            let diagnostics = self.project_diagnostics(&entry).unwrap_or_default();
            let paths = diagnostics.keys().cloned().collect::<BTreeSet<_>>();
            let previous = self.published.remove(&scope).unwrap_or_default();
            for path in previous.difference(&paths) {
                publish_diagnostics(writer, path, &[])?;
            }
            for (path, diagnostics) in diagnostics {
                publish_diagnostics(writer, &path, &diagnostics)?;
            }
            self.published.insert(scope, paths);
        }
        Ok(())
    }

    fn project_diagnostics(&self, entry: &Path) -> Option<BTreeMap<PathBuf, Vec<Value>>> {
        let (sources, entry_source) =
            load_project_documents_with_overlays(entry, &self.documents).ok()?;
        let mut diagnostics = sources
            .iter()
            .map(|source| (source.path.clone(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut parsed = Vec::with_capacity(sources.len());
        let mut syntax_failed = false;

        for (source_id, source) in sources.iter().enumerate() {
            match parse(&source.text) {
                Ok(mut program) => {
                    program.set_source(source_id);
                    parsed.push(program);
                }
                Err(error) => {
                    syntax_failed = true;
                    diagnostics.get_mut(&source.path)?.push(diagnostic(
                        &source.text,
                        error.span,
                        &error.message,
                        "syntax",
                    ));
                }
            }
        }
        if syntax_failed {
            return Some(diagnostics);
        }

        let project = combine_parsed_sources(sources, parsed, entry_source);
        if let Err(errors) = compile_with_capabilities(&project.program, CAPABILITIES) {
            for error in errors {
                let source: &SourceDocument = project
                    .sources
                    .get(error.span.source)
                    .unwrap_or(&project.sources[entry_source]);
                diagnostics.get_mut(&source.path)?.push(diagnostic(
                    &source.text,
                    error.span,
                    &error.message,
                    "compile",
                ));
            }
        }
        Some(diagnostics)
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

fn project_scope(path: &Path) -> PathBuf {
    path.parent()
        .and_then(find_project_root)
        .unwrap_or_else(|| path.to_path_buf())
}

fn diagnostic(source: &str, span: Span, message: &str, code: &str) -> Value {
    json!({
        "range": span_range(source, span),
        "severity": 1,
        "source": "mettle",
        "code": code,
        "message": message,
    })
}

fn publish_diagnostics(
    writer: &mut impl Write,
    path: &Path,
    diagnostics: &[Value],
) -> io::Result<()> {
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": path_to_file_uri(path),
                "diagnostics": diagnostics,
            }
        }),
    )
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
    let decoded = String::from_utf8(decoded).ok()?;
    #[cfg(windows)]
    let decoded = if decoded.starts_with('/')
        && decoded.as_bytes().get(2) == Some(&b':')
        && decoded
            .as_bytes()
            .get(1)
            .is_some_and(u8::is_ascii_alphabetic)
    {
        decoded[1..].to_owned()
    } else if !decoded.starts_with('/') {
        format!(r"\\{}", decoded.replace('/', r"\"))
    } else {
        decoded
    };
    Some(PathBuf::from(decoded))
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
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = path.strip_prefix(r"\\?\UNC\").map_or_else(
        || path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned(),
        |path| format!(r"\\{path}"),
    );
    let path = path.replace('\\', "/");
    let mut uri = if path.as_bytes().get(1) == Some(&b':') {
        String::from("file:///")
    } else if path.starts_with("//") {
        String::from("file:")
    } else {
        String::from("file://")
    };
    for byte in path.bytes() {
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
    use super::{
        Server, byte_offset, file_uri_to_path, normalize_path, path_to_file_uri, position,
        read_message,
    };
    use serde_json::{Value, json};
    use std::fs;
    use std::io::BufReader;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMPORARY_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn project_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let counter = TEMPORARY_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flow-lsp-test-{}-{unique}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("test project should be creatable");
        path
    }

    fn messages(bytes: &[u8]) -> Vec<Value> {
        let mut reader = BufReader::new(bytes);
        let mut messages = Vec::new();
        while let Some(message) = read_message(&mut reader).expect("LSP message should decode") {
            messages.push(message);
        }
        messages
    }

    fn diagnostics_for(messages: &[Value], path: &Path) -> Vec<Value> {
        let uri = path_to_file_uri(&normalize_path(path.to_path_buf()));
        messages
            .iter()
            .find(|message| {
                message["method"] == "textDocument/publishDiagnostics"
                    && message["params"]["uri"] == uri
            })
            .expect("diagnostics should be published for the file")["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array")
            .clone()
    }

    #[test]
    fn publishes_and_clears_unsaved_syntax_and_compile_diagnostics() {
        let directory = project_directory();
        let entry = directory.join("main.mettle");
        fs::write(&entry, "flow main() = true\n").expect("entry should be writable");
        let uri = path_to_file_uri(&entry);
        let mut server = Server::default();

        let mut output = Vec::new();
        server
            .handle(
                &json!({
                    "method": "textDocument/didOpen",
                    "params": { "textDocument": { "uri": uri, "text": "flow main() = (" } }
                }),
                &mut output,
            )
            .expect("open should publish diagnostics");
        let diagnostics = diagnostics_for(&messages(&output), &entry);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "syntax");
        assert_eq!(diagnostics[0]["severity"], 1);

        output.clear();
        server
            .handle(
                &json!({
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": { "uri": uri },
                        "contentChanges": [{ "text": "flow main() = missing\n" }]
                    }
                }),
                &mut output,
            )
            .expect("change should publish diagnostics");
        let diagnostics = diagnostics_for(&messages(&output), &entry);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "compile");
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("missing")
        );

        output.clear();
        server
            .handle(
                &json!({
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": { "uri": uri },
                        "contentChanges": [{ "text": "flow main() = true\n" }]
                    }
                }),
                &mut output,
            )
            .expect("fix should clear diagnostics");
        assert!(diagnostics_for(&messages(&output), &entry).is_empty());

        output.clear();
        server
            .handle(
                &json!({
                    "method": "textDocument/didClose",
                    "params": { "textDocument": { "uri": uri } }
                }),
                &mut output,
            )
            .expect("close should clear diagnostics");
        assert!(diagnostics_for(&messages(&output), &entry).is_empty());
        fs::remove_dir_all(directory).expect("test project should be removable");
    }

    #[test]
    fn includes_unsaved_new_project_files_in_diagnostics() {
        let directory = project_directory();
        fs::write(directory.join("mettle.toml"), "name = \"lsp-test\"\n")
            .expect("manifest should be writable");
        let entry = directory.join("main.mettle");
        let unsaved = directory.join("new.mettle");
        fs::write(&entry, "flow main() = true\n").expect("entry should be writable");
        let mut server = Server::default();
        let mut output = Vec::new();

        server
            .handle(
                &json!({
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": path_to_file_uri(&entry),
                            "text": "flow main() = true\n"
                        }
                    }
                }),
                &mut output,
            )
            .expect("entry should open");
        output.clear();
        server
            .handle(
                &json!({
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": path_to_file_uri(&unsaved),
                            "text": "flow extra() = missing\n"
                        }
                    }
                }),
                &mut output,
            )
            .expect("unsaved project file should be validated");
        let diagnostics = diagnostics_for(&messages(&output), &unsaved);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "compile");
        fs::remove_dir_all(directory).expect("test project should be removable");
    }

    #[test]
    fn converts_lsp_utf16_positions() {
        let source = "first\n💡value\n";
        assert_eq!(byte_offset(source, 1, 2), Some(10));
        assert_eq!(position(source, 10), (1, 2));
    }

    #[test]
    fn round_trips_file_uris() {
        let path = if cfg!(windows) {
            Path::new("C:\\flow project\\main.mettle")
        } else {
            Path::new("/tmp/flow project/main.mettle")
        };
        let uri = path_to_file_uri(path);
        if cfg!(windows) {
            assert_eq!(uri, "file:///C:/flow%20project/main.mettle");
        } else {
            assert_eq!(uri, "file:///tmp/flow%20project/main.mettle");
        }
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(path));
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_windows_verbatim_and_unc_paths() {
        let verbatim = Path::new(r"\\?\C:\flow project\main.mettle");
        assert_eq!(
            path_to_file_uri(verbatim),
            "file:///C:/flow%20project/main.mettle"
        );

        let unc = Path::new(r"\\server\share\main.mettle");
        let uri = path_to_file_uri(unc);
        assert_eq!(uri, "file://server/share/main.mettle");
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(unc));
    }

    #[test]
    fn navigates_unsaved_cross_file_flow_calls() {
        let directory = project_directory();
        fs::write(directory.join("mettle.toml"), "name = \"lsp-test\"\n")
            .expect("manifest should be writable");
        let declaration = directory.join("shared.mettle");
        fs::write(&declaration, "namespace shared\nflow helper() = true\n")
            .expect("declaration should be writable");
        let entry = directory.join("main.mettle");
        fs::write(&entry, "use namespace shared\nflow main() = false\n")
            .expect("entry should be writable");

        let unsaved = "use namespace shared\nflow main() = helper()\n";
        let mut server = Server::default();
        server
            .documents
            .insert(normalize_path(entry.clone()), unsaved.to_owned());
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
