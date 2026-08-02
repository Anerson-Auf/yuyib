//! Diagnostics-only `rust-analyzer` sidecar for the Editor Code workspace.
//!
//! Completion, hover, rename and code actions are intentionally out of scope.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use serde_json::{Value, json};

/// One Monaco-compatible diagnostic marker.
#[derive(Clone, Debug)]
pub struct LspDiagnostic {
    pub path: String,
    pub severity: u8,
    pub message: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub source: String,
}

/// Host-facing RA session status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspStatus {
    Starting,
    Ready,
    Unavailable(String),
    Error(String),
}

enum LspEvent {
    Status(LspStatus),
    Diagnostics {
        path: String,
        diagnostics: Vec<LspDiagnostic>,
    },
}

/// Bounded rust-analyzer process with Content-Length JSON-RPC framing.
pub struct RustAnalyzerSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    next_id: u64,
    open_docs: HashMap<PathBuf, i32>,
    events: Receiver<LspEvent>,
    status: LspStatus,
}

impl RustAnalyzerSession {
    /// Spawns `rust-analyzer` for `project_root`, or returns Unavailable.
    pub fn start(project_root: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let binary = which_rust_analyzer();
        let Some(binary) = binary else {
            let _ = tx.send(LspEvent::Status(LspStatus::Unavailable(
                "rust-analyzer not found on PATH".to_owned(),
            )));
            return Self {
                child: None,
                stdin: None,
                next_id: 1,
                open_docs: HashMap::new(),
                events: rx,
                status: LspStatus::Unavailable("rust-analyzer not found on PATH".to_owned()),
            };
        };
        let mut child = match Command::new(&binary)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let message = format!("failed to spawn rust-analyzer: {error}");
                let _ = tx.send(LspEvent::Status(LspStatus::Unavailable(message.clone())));
                return Self {
                    child: None,
                    stdin: None,
                    next_id: 1,
                    open_docs: HashMap::new(),
                    events: rx,
                    status: LspStatus::Unavailable(message),
                };
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let root_uri = path_to_file_uri(project_root);
        thread::spawn(move || reader_loop(stdout, tx));
        let mut session = Self {
            child: Some(child),
            stdin: Some(stdin),
            next_id: 1,
            open_docs: HashMap::new(),
            events: rx,
            status: LspStatus::Starting,
        };
        let id = session.alloc_id();
        let _ = session.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false }
                    }
                }
            }
        }));
        let _ = session.write_message(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));
        session
    }

    /// Current status snapshot.
    #[must_use]
    pub fn status(&self) -> &LspStatus {
        &self.status
    }

    /// Opens or replaces one Rust source document.
    pub fn did_open(&mut self, path: &Path, text: &str) {
        if self.stdin.is_none() {
            return;
        }
        let uri = path_to_file_uri(path);
        self.open_docs.insert(path.to_path_buf(), 1);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": text
                }
            }
        }));
    }

    /// Notifies RA that document bytes changed.
    pub fn did_change(&mut self, path: &Path, text: &str) {
        if self.stdin.is_none() {
            return;
        }
        let version = {
            let entry = self.open_docs.entry(path.to_path_buf()).or_insert(1);
            *entry += 1;
            *entry
        };
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }));
    }

    /// Closes one document.
    pub fn did_close(&mut self, path: &Path) {
        if self.stdin.is_none() {
            return;
        }
        self.open_docs.remove(path);
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        }));
    }

    /// Drains RA notifications into host callbacks.
    pub fn poll(
        &mut self,
        mut on_status: impl FnMut(LspStatus),
        mut on_diagnostics: impl FnMut(String, Vec<LspDiagnostic>),
    ) {
        loop {
            match self.events.try_recv() {
                Ok(LspEvent::Status(status)) => {
                    self.status = status.clone();
                    on_status(status);
                }
                Ok(LspEvent::Diagnostics { path, diagnostics }) => {
                    on_diagnostics(path, diagnostics);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !matches!(self.status, LspStatus::Unavailable(_)) {
                        let status = LspStatus::Error("rust-analyzer exited".to_owned());
                        self.status = status.clone();
                        on_status(status);
                    }
                    break;
                }
            }
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write_message(&mut self, value: &Value) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("rust-analyzer stdin closed".to_owned());
        };
        let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).map_err(|error| error.to_string())?;
        stdin.write_all(&body).map_err(|error| error.to_string())?;
        stdin.flush().map_err(|error| error.to_string())
    }
}

impl Drop for RustAnalyzerSession {
    fn drop(&mut self) {
        if let Some(stdin) = self.stdin.as_mut() {
            let shutdown = json!({
                "jsonrpc": "2.0",
                "id": 999_999_u64,
                "method": "shutdown",
                "params": null
            });
            if let Ok(body) = serde_json::to_vec(&shutdown) {
                let _ = write!(stdin, "Content-Length: {}\r\n\r\n", body.len());
                let _ = stdin.write_all(&body);
                let _ = write!(
                    stdin,
                    "Content-Length: 48\r\n\r\n{{\"jsonrpc\":\"2.0\",\"method\":\"exit\",\"params\":null}}"
                );
                let _ = stdin.flush();
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn which_rust_analyzer() -> Option<PathBuf> {
    let ok = Command::new("rust-analyzer")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from("rust-analyzer"))
}

fn path_to_file_uri(path: &Path) -> String {
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let mut text = absolute.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        text = format!("/{text}");
    }
    format!("file://{text}")
}

fn reader_loop(stdout: impl Read + Send + 'static, tx: Sender<LspEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut ready_sent = false;
    loop {
        let Ok(Some(message)) = read_message(&mut reader) else {
            break;
        };
        if !ready_sent
            && message.get("id").is_some()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            ready_sent = true;
            if message.get("error").is_some() {
                let detail = message
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("initialize failed");
                let _ = tx.send(LspEvent::Status(LspStatus::Error(detail.to_owned())));
            } else if tx.send(LspEvent::Status(LspStatus::Ready)).is_err() {
                break;
            }
        }
        let method = message.get("method").and_then(Value::as_str);
        if method == Some("textDocument/publishDiagnostics") {
            let Some(params) = message.get("params") else {
                continue;
            };
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let path = file_uri_to_path(uri);
            let diagnostics = params
                .get("diagnostics")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let range = item.get("range")?;
                            let start = range.get("start")?;
                            let end = range.get("end")?;
                            Some(LspDiagnostic {
                                path: path.clone(),
                                severity: item
                                    .get("severity")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(2) as u8,
                                message: item
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("diagnostic")
                                    .to_owned(),
                                start_line: start
                                    .get("line")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0) as u32
                                    + 1,
                                start_column: start
                                    .get("character")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0) as u32
                                    + 1,
                                end_line: end.get("line").and_then(Value::as_u64).unwrap_or(0)
                                    as u32
                                    + 1,
                                end_column: end
                                    .get("character")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0) as u32
                                    + 1,
                                source: item
                                    .get("source")
                                    .and_then(Value::as_str)
                                    .unwrap_or("rust-analyzer")
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if tx
                .send(LspEvent::Diagnostics { path, diagnostics })
                .is_err()
            {
                break;
            }
        }
    }
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    let Some(length) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| error.to_string())?;
    let value = serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    Ok(Some(value))
}

fn file_uri_to_path(uri: &str) -> String {
    let stripped = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    let mut path = stripped.replace('/', "\\");
    if path.starts_with('\\') && path.chars().nth(2) == Some(':') {
        path = path.trim_start_matches('\\').to_owned();
    }
    path
}
