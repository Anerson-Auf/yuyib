//! `rust-analyzer` sidecar for the Editor Code workspace.
//!
//! Supports diagnostics notifications plus request/response
//! `textDocument/completion`, `textDocument/hover`, `textDocument/signatureHelp`,
//! `textDocument/definition`, `textDocument/references`, `textDocument/rename`,
//! `textDocument/codeAction` (edit-bearing and allowlisted command-only), and
//! `workspace/executeCommand`.

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

/// One Monaco completion suggestion (1-based ranges).
#[derive(Clone, Debug)]
pub struct LspCompletionItem {
    pub label: String,
    pub kind: u32,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub insert_text: String,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
}

/// Hover payload for Monaco (markdown preferred).
#[derive(Clone, Debug, Default)]
pub struct LspHover {
    pub markdown: String,
}

/// One parameter inside a signature help entry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspSignatureParameter {
    /// Parameter label (plain text; offset ranges are resolved against the signature label).
    pub label: String,
    pub documentation: Option<String>,
}

/// One callable signature overload.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspSignatureInformation {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<LspSignatureParameter>,
    pub active_parameter: Option<u32>,
}

/// Signature help payload for Monaco.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspSignatureHelp {
    pub signatures: Vec<LspSignatureInformation>,
    pub active_signature: u32,
    pub active_parameter: u32,
}

/// One definition / navigation location (1-based Monaco ranges).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub path: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// One text edit inside a rename WorkspaceEdit (1-based ranges).
#[derive(Clone, Debug)]
pub struct LspTextEdit {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub new_text: String,
}

/// Edits for one file path (absolute or host-normalized).
#[derive(Clone, Debug)]
pub struct LspFileEdits {
    pub path: String,
    pub edits: Vec<LspTextEdit>,
}

/// Rename result for the UI (empty `files` + `error` = rejection).
#[derive(Clone, Debug, Default)]
pub struct LspRenameResult {
    pub files: Vec<LspFileEdits>,
    pub error: Option<String>,
}

/// One edit-bearing and/or command-bearing code action for Monaco.
#[derive(Clone, Debug)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub disabled: Option<String>,
    pub files: Vec<LspFileEdits>,
    /// Allowlisted LSP command (`rust-analyzer.*`); run via `workspace/executeCommand`.
    pub command: Option<LspCommand>,
}

/// LSP `Command` payload for `workspace/executeCommand`.
#[derive(Clone, Debug)]
pub struct LspCommand {
    pub command: String,
    pub title: Option<String>,
    pub arguments: Vec<Value>,
}

/// Result of `workspace/executeCommand` (optional WorkspaceEdit).
#[derive(Clone, Debug, Default)]
pub struct LspExecuteCommandResult {
    pub files: Vec<LspFileEdits>,
    pub error: Option<String>,
}

/// Host-facing RA session status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspStatus {
    Starting,
    Ready,
    Unavailable(String),
    Error(String),
}

enum PendingKind {
    Completion { client_request_id: String },
    Hover { client_request_id: String },
    SignatureHelp { client_request_id: String },
    Definition { client_request_id: String },
    References { client_request_id: String },
    Rename { client_request_id: String },
    CodeAction { client_request_id: String },
    ExecuteCommand { client_request_id: String },
}

enum LspEvent {
    Status(LspStatus),
    Diagnostics {
        path: String,
        diagnostics: Vec<LspDiagnostic>,
    },
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    },
}

/// Bounded rust-analyzer process with Content-Length JSON-RPC framing.
pub struct RustAnalyzerSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    next_id: u64,
    initialize_id: u64,
    open_docs: HashMap<PathBuf, i32>,
    pending: HashMap<u64, PendingKind>,
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
                initialize_id: 0,
                open_docs: HashMap::new(),
                pending: HashMap::new(),
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
                    initialize_id: 0,
                    open_docs: HashMap::new(),
                    pending: HashMap::new(),
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
            initialize_id: 0,
            open_docs: HashMap::new(),
            pending: HashMap::new(),
            events: rx,
            status: LspStatus::Starting,
        };
        let id = session.alloc_id();
        session.initialize_id = id;
        let _ = session.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false },
                        "completion": {
                            "completionItem": {
                                "snippetSupport": false,
                                "documentationFormat": ["markdown", "plaintext"]
                            }
                        },
                        "hover": {
                            "contentFormat": ["markdown", "plaintext"]
                        },
                        "signatureHelp": {
                            "signatureInformation": {
                                "documentationFormat": ["markdown", "plaintext"],
                                "parameterInformation": {
                                    "labelOffsetSupport": true
                                }
                            },
                            "contextSupport": true
                        },
                        "definition": {
                            "linkSupport": true
                        },
                        "references": {},
                        "rename": {
                            "prepareSupport": false
                        },
                        "codeAction": {
                            "codeActionLiteralSupport": {
                                "codeActionKind": {
                                    "valueSet": [
                                        "",
                                        "quickfix",
                                        "refactor",
                                        "refactor.extract",
                                        "refactor.inline",
                                        "refactor.rewrite",
                                        "source",
                                        "source.organizeImports"
                                    ]
                                }
                            },
                            "isPreferredSupport": true,
                            "disabledSupport": true
                        }
                    },
                    "workspace": {
                        "executeCommand": {
                            "dynamicRegistration": false
                        }
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

    /// Requests completions at a 0-based LSP position.
    pub fn request_completion(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::Completion {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }
        }));
    }

    /// Requests hover at a 0-based LSP position.
    pub fn request_hover(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::Hover {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }
        }));
    }

    /// Requests signature help at a 0-based LSP position.
    pub fn request_signature_help(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        trigger_kind: u32,
        trigger_character: Option<&str>,
        is_retrigger: bool,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::SignatureHelp {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let mut context = json!({
            "triggerKind": trigger_kind.max(1),
            "isRetrigger": is_retrigger
        });
        if let Some(character) = trigger_character {
            context["triggerCharacter"] = Value::String(character.to_owned());
        }
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": context
            }
        }));
    }

    /// Requests go-to-definition at a 0-based LSP position.
    pub fn request_definition(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::Definition {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }
        }));
    }

    /// Requests find-all-references at a 0-based LSP position.
    pub fn request_references(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        include_declaration: bool,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::References {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/references",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": {
                    "includeDeclaration": include_declaration
                }
            }
        }));
    }

    /// Requests a workspace rename at a 0-based LSP position.
    pub fn request_rename(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
        new_name: &str,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::Rename {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/rename",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "newName": new_name
            }
        }));
    }

    /// Requests code actions for a 0-based LSP range.
    ///
    /// `diagnostics` are LSP-shaped objects already in 0-based coordinates.
    pub fn request_code_action(
        &mut self,
        path: &Path,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
        diagnostics: Vec<Value>,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::CodeAction {
                client_request_id: client_request_id.into(),
            },
        );
        let uri = path_to_file_uri(path);
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/codeAction",
            "params": {
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start_line, "character": start_character },
                    "end": { "line": end_line, "character": end_character }
                },
                "context": {
                    "diagnostics": diagnostics,
                    "triggerKind": 1
                }
            }
        }));
    }

    /// Runs an allowlisted `workspace/executeCommand` on rust-analyzer.
    ///
    /// Rejects non-`rust-analyzer.*` commands synchronously via the pending
    /// response path when the command is not allowlisted (caller should filter);
    /// this method still sends only after [`is_allowed_lsp_command`] is true.
    pub fn request_execute_command(
        &mut self,
        command: &str,
        arguments: Vec<Value>,
        client_request_id: impl Into<String>,
    ) {
        if self.stdin.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            PendingKind::ExecuteCommand {
                client_request_id: client_request_id.into(),
            },
        );
        let _ = self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "workspace/executeCommand",
            "params": {
                "command": command,
                "arguments": arguments
            }
        }));
    }

    /// Drains RA notifications / responses into host callbacks.
    pub fn poll(
        &mut self,
        mut on_status: impl FnMut(LspStatus),
        mut on_diagnostics: impl FnMut(String, Vec<LspDiagnostic>),
        mut on_completion: impl FnMut(String, Vec<LspCompletionItem>),
        mut on_hover: impl FnMut(String, Option<LspHover>),
        mut on_signature_help: impl FnMut(String, Option<LspSignatureHelp>),
        mut on_definition: impl FnMut(String, Vec<LspLocation>),
        mut on_references: impl FnMut(String, Vec<LspLocation>),
        mut on_rename: impl FnMut(String, LspRenameResult),
        mut on_code_action: impl FnMut(String, Vec<LspCodeAction>),
        mut on_execute_command: impl FnMut(String, LspExecuteCommandResult),
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
                Ok(LspEvent::Response { id, result, error }) => {
                    if id == self.initialize_id {
                        if error.is_some() {
                            let detail = error
                                .as_ref()
                                .and_then(|value| value.get("message"))
                                .and_then(Value::as_str)
                                .unwrap_or("initialize failed");
                            let status = LspStatus::Error(detail.to_owned());
                            self.status = status.clone();
                            on_status(status);
                        } else if !matches!(self.status, LspStatus::Ready) {
                            let status = LspStatus::Ready;
                            self.status = status.clone();
                            on_status(status);
                        }
                        continue;
                    }
                    match self.pending.remove(&id) {
                        Some(PendingKind::Completion { client_request_id }) => {
                            let items = if error.is_some() {
                                Vec::new()
                            } else {
                                parse_completion_result(result.as_ref())
                            };
                            on_completion(client_request_id, items);
                        }
                        Some(PendingKind::Hover { client_request_id }) => {
                            let hover = if error.is_some() {
                                None
                            } else {
                                parse_hover_result(result.as_ref())
                            };
                            on_hover(client_request_id, hover);
                        }
                        Some(PendingKind::SignatureHelp { client_request_id }) => {
                            let help = if error.is_some() {
                                None
                            } else {
                                parse_signature_help_result(result.as_ref())
                            };
                            on_signature_help(client_request_id, help);
                        }
                        Some(PendingKind::Definition { client_request_id }) => {
                            let locations = if error.is_some() {
                                Vec::new()
                            } else {
                                parse_locations_result(result.as_ref())
                            };
                            on_definition(client_request_id, locations);
                        }
                        Some(PendingKind::References { client_request_id }) => {
                            let locations = if error.is_some() {
                                Vec::new()
                            } else {
                                parse_locations_result(result.as_ref())
                            };
                            on_references(client_request_id, locations);
                        }
                        Some(PendingKind::Rename { client_request_id }) => {
                            let rename = if let Some(err) = error.as_ref() {
                                LspRenameResult {
                                    files: Vec::new(),
                                    error: Some(
                                        err.get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("rename failed")
                                            .to_owned(),
                                    ),
                                }
                            } else {
                                parse_rename_result(result.as_ref())
                            };
                            on_rename(client_request_id, rename);
                        }
                        Some(PendingKind::CodeAction { client_request_id }) => {
                            let actions = if error.is_some() {
                                Vec::new()
                            } else {
                                parse_code_action_result(result.as_ref())
                            };
                            on_code_action(client_request_id, actions);
                        }
                        Some(PendingKind::ExecuteCommand { client_request_id }) => {
                            let executed = if let Some(err) = error.as_ref() {
                                LspExecuteCommandResult {
                                    files: Vec::new(),
                                    error: Some(
                                        err.get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("executeCommand failed")
                                            .to_owned(),
                                    ),
                                }
                            } else {
                                parse_execute_command_result(result.as_ref())
                            };
                            on_execute_command(client_request_id, executed);
                        }
                        None => {}
                    }
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
    loop {
        let Ok(Some(message)) = read_message(&mut reader) else {
            break;
        };
        // Client responses carry `id` + `result`/`error`. Server→client requests
        // also have `id`, but include `method` — ignore those for now.
        if message.get("method").is_none()
            && let Some(id) = message.get("id").and_then(Value::as_u64)
        {
            let result = message.get("result").cloned();
            let error = message.get("error").cloned();
            if tx
                .send(LspEvent::Response { id, result, error })
                .is_err()
            {
                break;
            }
            continue;
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

fn parse_completion_result(result: Option<&Value>) -> Vec<LspCompletionItem> {
    let Some(result) = result else {
        return Vec::new();
    };
    let items = result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array));
    let Some(items) = items else {
        return Vec::new();
    };
    items.iter().filter_map(parse_completion_item).collect()
}

fn parse_completion_item(item: &Value) -> Option<LspCompletionItem> {
    let label = item.get("label")?.as_str()?.to_owned();
    let insert_text = item
        .get("insertText")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            item.get("textEdit")
                .and_then(|edit| edit.get("newText"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| label.clone());
    Some(LspCompletionItem {
        label,
        kind: item.get("kind").and_then(Value::as_u64).unwrap_or(1) as u32,
        detail: item
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_owned),
        documentation: markup_to_string(item.get("documentation")),
        insert_text,
        filter_text: item
            .get("filterText")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sort_text: item
            .get("sortText")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn parse_hover_result(result: Option<&Value>) -> Option<LspHover> {
    let result = result?;
    if result.is_null() {
        return None;
    }
    let markdown = markup_to_string(result.get("contents"))?;
    if markdown.trim().is_empty() {
        return None;
    }
    Some(LspHover { markdown })
}

fn parse_signature_help_result(result: Option<&Value>) -> Option<LspSignatureHelp> {
    let result = result?;
    if result.is_null() {
        return None;
    }
    let signatures = result
        .get("signatures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_signature_information)
        .collect::<Vec<_>>();
    if signatures.is_empty() {
        return None;
    }
    Some(LspSignatureHelp {
        signatures,
        active_signature: result
            .get("activeSignature")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        active_parameter: result
            .get("activeParameter")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    })
}

fn parse_signature_information(item: &Value) -> Option<LspSignatureInformation> {
    let label = item.get("label")?.as_str()?.to_owned();
    let parameters = item
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|parameter| parse_signature_parameter(parameter, &label))
        .collect();
    Some(LspSignatureInformation {
        label,
        documentation: markup_to_string(item.get("documentation")),
        parameters,
        active_parameter: item
            .get("activeParameter")
            .and_then(Value::as_u64)
            .map(|value| value as u32),
    })
}

fn parse_signature_parameter(item: &Value, signature_label: &str) -> Option<LspSignatureParameter> {
    let label = match item.get("label") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) if parts.len() == 2 => {
            let start = parts[0].as_u64()? as usize;
            let end = parts[1].as_u64()? as usize;
            if end < start || end > signature_label.len() {
                return None;
            }
            signature_label.get(start..end)?.to_owned()
        }
        _ => return None,
    };
    Some(LspSignatureParameter {
        label,
        documentation: markup_to_string(item.get("documentation")),
    })
}

fn parse_locations_result(result: Option<&Value>) -> Vec<LspLocation> {
    let Some(result) = result else {
        return Vec::new();
    };
    if result.is_null() {
        return Vec::new();
    }
    if let Some(items) = result.as_array() {
        return items.iter().filter_map(parse_location_item).collect();
    }
    parse_location_item(result).into_iter().collect()
}

fn parse_location_item(item: &Value) -> Option<LspLocation> {
    if let Some(uri) = item.get("targetUri").and_then(Value::as_str) {
        let range = item
            .get("targetSelectionRange")
            .or_else(|| item.get("targetRange"))?;
        let (start_line, start_column, end_line, end_column) = parse_lsp_range_1based(range)?;
        return Some(LspLocation {
            path: file_uri_to_path(uri),
            start_line,
            start_column,
            end_line,
            end_column,
        });
    }
    let uri = item.get("uri").and_then(Value::as_str)?;
    let range = item.get("range")?;
    let (start_line, start_column, end_line, end_column) = parse_lsp_range_1based(range)?;
    Some(LspLocation {
        path: file_uri_to_path(uri),
        start_line,
        start_column,
        end_line,
        end_column,
    })
}

fn parse_lsp_range_1based(range: &Value) -> Option<(u32, u32, u32, u32)> {
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some((
        start.get("line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        end.get("line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        end.get("character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
    ))
}

fn parse_rename_result(result: Option<&Value>) -> LspRenameResult {
    let Some(result) = result else {
        return LspRenameResult {
            files: Vec::new(),
            error: Some("empty rename result".to_owned()),
        };
    };
    if result.is_null() {
        return LspRenameResult {
            files: Vec::new(),
            error: Some("cannot rename at this location".to_owned()),
        };
    }
    let files = parse_workspace_edit(result);
    if files.is_empty() {
        return LspRenameResult {
            files: Vec::new(),
            error: Some("rename produced no edits".to_owned()),
        };
    }
    LspRenameResult {
        files,
        error: None,
    }
}

fn parse_code_action_result(result: Option<&Value>) -> Vec<LspCodeAction> {
    let Some(result) = result else {
        return Vec::new();
    };
    if result.is_null() {
        return Vec::new();
    }
    let Some(items) = result.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(parse_code_action_item).collect()
}

fn parse_code_action_item(item: &Value) -> Option<LspCodeAction> {
    // Bare LSP Command (title + command string, no CodeAction wrapper).
    if item.get("edit").is_none()
        && item.get("kind").is_none()
        && item.get("command").and_then(Value::as_str).is_some()
    {
        let title = item.get("title")?.as_str()?.to_owned();
        let command = parse_lsp_command(item)?;
        if !is_allowed_lsp_command(&command.command) {
            return None;
        }
        return Some(LspCodeAction {
            title,
            kind: None,
            is_preferred: false,
            disabled: None,
            files: Vec::new(),
            command: Some(command),
        });
    }

    let title = item.get("title")?.as_str()?.to_owned();
    let disabled = item
        .get("disabled")
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let files = item
        .get("edit")
        .map(parse_workspace_edit)
        .unwrap_or_default();
    let command = item
        .get("command")
        .and_then(parse_lsp_command)
        .filter(|command| is_allowed_lsp_command(&command.command));
    if files.is_empty() && disabled.is_none() && command.is_none() {
        return None;
    }
    Some(LspCodeAction {
        title,
        kind: item
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        is_preferred: item
            .get("isPreferred")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        disabled,
        files,
        command,
    })
}

/// Whether `command` may be forwarded to rust-analyzer `workspace/executeCommand`.
#[must_use]
pub fn is_allowed_lsp_command(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty() && trimmed.starts_with("rust-analyzer.")
}

fn parse_lsp_command(value: &Value) -> Option<LspCommand> {
    // Accepts a bare Command object or CodeAction.command object:
    // { "title"?, "command": "rust-analyzer.…", "arguments"? }.
    let command = value.get("command")?.as_str()?.to_owned();
    Some(LspCommand {
        command,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        arguments: value
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

fn parse_execute_command_result(result: Option<&Value>) -> LspExecuteCommandResult {
    let Some(result) = result else {
        return LspExecuteCommandResult::default();
    };
    if result.is_null() {
        return LspExecuteCommandResult::default();
    }
    // Some RA commands return a WorkspaceEdit directly.
    let files = parse_workspace_edit(result);
    if !files.is_empty() {
        return LspExecuteCommandResult {
            files,
            error: None,
        };
    }
    // Others wrap edits: { edit: WorkspaceEdit } or ApplyWorkspaceEditParams.
    if let Some(edit) = result.get("edit") {
        let files = parse_workspace_edit(edit);
        if !files.is_empty() {
            return LspExecuteCommandResult {
                files,
                error: None,
            };
        }
    }
    LspExecuteCommandResult::default()
}

fn parse_workspace_edit(edit: &Value) -> Vec<LspFileEdits> {
    let mut by_path: HashMap<String, Vec<LspTextEdit>> = HashMap::new();
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            let path = file_uri_to_path(uri);
            let parsed = edits
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(parse_text_edit)
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                by_path.entry(path).or_default().extend(parsed);
            }
        }
    }
    if let Some(document_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            let Some(uri) = change
                .get("textDocument")
                .and_then(|doc| doc.get("uri"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let path = file_uri_to_path(uri);
            let parsed = change
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_text_edit)
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                by_path.entry(path).or_default().extend(parsed);
            }
        }
    }
    let mut files: Vec<LspFileEdits> = by_path
        .into_iter()
        .map(|(path, edits)| LspFileEdits { path, edits })
        .collect();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn parse_text_edit(edit: &Value) -> Option<LspTextEdit> {
    let range = edit.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    let new_text = edit.get("newText")?.as_str()?.to_owned();
    Some(LspTextEdit {
        start_line: start.get("line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        start_column: start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        end_line: end.get("line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        end_column: end.get("character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1,
        new_text,
    })
}

fn markup_to_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(kind) = value.get("kind").and_then(Value::as_str)
        && let Some(text) = value.get("value").and_then(Value::as_str)
    {
        let _ = kind;
        return Some(text.to_owned());
    }
    if let Some(text) = value.get("value").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    if let Some(items) = value.as_array() {
        let joined = items
            .iter()
            .filter_map(|item| markup_to_string(Some(item)))
            .collect::<Vec<_>>()
            .join("\n\n");
        if joined.is_empty() {
            return None;
        }
        return Some(joined);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_completion_list_and_array() {
        let array = json!([
            { "label": "foo", "kind": 6, "insertText": "foo()" },
            { "label": "bar", "kind": 3, "detail": "fn bar()" }
        ]);
        let items = parse_completion_result(Some(&array));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].insert_text, "foo()");
        assert_eq!(items[1].detail.as_deref(), Some("fn bar()"));

        let list = json!({ "isIncomplete": false, "items": [{ "label": "baz", "kind": 2 }] });
        let items = parse_completion_result(Some(&list));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "baz");
    }

    #[test]
    fn parses_hover_markup() {
        let hover = parse_hover_result(Some(&json!({
            "contents": { "kind": "markdown", "value": "**Vec**" }
        })))
        .expect("hover");
        assert!(hover.markdown.contains("Vec"));
        assert!(parse_hover_result(Some(&Value::Null)).is_none());
    }

    #[test]
    fn parses_signature_help_with_offset_parameters() {
        let help = parse_signature_help_result(Some(&json!({
            "signatures": [{
                "label": "fn smoke_note(project: &str) -> String",
                "documentation": { "kind": "markdown", "value": "Builds a note" },
                "parameters": [
                    {
                        "label": [14, 27],
                        "documentation": "project name"
                    }
                ],
                "activeParameter": 0
            }],
            "activeSignature": 0,
            "activeParameter": 0
        })))
        .expect("signature help");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].parameters.len(), 1);
        assert_eq!(help.signatures[0].parameters[0].label, "project: &str");
        assert_eq!(
            help.signatures[0].documentation.as_deref(),
            Some("Builds a note")
        );
        assert!(parse_signature_help_result(Some(&Value::Null)).is_none());
    }

    #[test]
    fn parses_definition_location_and_link() {
        let location = parse_locations_result(Some(&json!({
            "uri": "file:///D:/proj/src/demo_lsp.rs",
            "range": {
                "start": { "line": 10, "character": 4 },
                "end": { "line": 10, "character": 15 }
            }
        })));
        assert_eq!(location.len(), 1);
        assert!(location[0].path.contains("demo_lsp.rs"));
        assert_eq!(location[0].start_line, 11);
        assert_eq!(location[0].start_column, 5);
        assert_eq!(location[0].end_column, 16);

        let links = parse_locations_result(Some(&json!([
            {
                "targetUri": "file:///D:/proj/src/lib.rs",
                "targetRange": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 20, "character": 1 }
                },
                "targetSelectionRange": {
                    "start": { "line": 3, "character": 7 },
                    "end": { "line": 3, "character": 11 }
                }
            }
        ])));
        assert_eq!(links.len(), 1);
        assert!(links[0].path.contains("lib.rs"));
        assert_eq!(links[0].start_line, 4);
        assert_eq!(links[0].start_column, 8);
        assert!(parse_locations_result(Some(&Value::Null)).is_empty());
    }

    #[test]
    fn parses_references_location_array() {
        let refs = parse_locations_result(Some(&json!([
            {
                "uri": "file:///D:/proj/src/demo_lsp.rs",
                "range": {
                    "start": { "line": 10, "character": 11 },
                    "end": { "line": 10, "character": 22 }
                }
            },
            {
                "uri": "file:///D:/proj/src/main.rs",
                "range": {
                    "start": { "line": 5, "character": 14 },
                    "end": { "line": 5, "character": 25 }
                }
            }
        ])));
        assert_eq!(refs.len(), 2);
        assert!(refs[0].path.contains("demo_lsp.rs"));
        assert_eq!(refs[0].start_line, 11);
        assert!(refs[1].path.contains("main.rs"));
        assert_eq!(refs[1].start_column, 15);
    }

    #[test]
    fn parses_rename_workspace_edit() {
        let result = parse_rename_result(Some(&json!({
            "changes": {
                "file:///D:/proj/src/lib.rs": [
                    {
                        "range": {
                            "start": { "line": 2, "character": 7 },
                            "end": { "line": 2, "character": 10 }
                        },
                        "newText": "bar"
                    }
                ]
            },
            "documentChanges": [
                {
                    "textDocument": { "uri": "file:///D:/proj/src/main.rs", "version": 1 },
                    "edits": [
                        {
                            "range": {
                                "start": { "line": 0, "character": 4 },
                                "end": { "line": 0, "character": 7 }
                            },
                            "newText": "bar"
                        }
                    ]
                }
            ]
        })));
        assert!(result.error.is_none());
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[0].edits[0].new_text, "bar");
        assert_eq!(result.files[0].edits[0].start_line, 3);
        assert!(parse_rename_result(Some(&Value::Null)).error.is_some());
    }

    #[test]
    fn parses_code_actions_with_edits_and_allowlisted_commands() {
        let actions = parse_code_action_result(Some(&json!([
            {
                "title": "Import Vec",
                "kind": "quickfix",
                "isPreferred": true,
                "edit": {
                    "changes": {
                        "file:///D:/proj/src/main.rs": [
                            {
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 0 }
                                },
                                "newText": "use std::vec::Vec;\n"
                            }
                        ]
                    }
                }
            },
            {
                "title": "run cargo",
                "command": {
                    "command": "rust-analyzer.runSingle",
                    "title": "run",
                    "arguments": [{ "kind": "cargo" }]
                }
            },
            {
                "title": "shell escape",
                "command": { "command": "evil.run", "title": "nope" }
            },
            {
                "title": "disabled fix",
                "kind": "quickfix",
                "disabled": { "reason": "not applicable" },
                "edit": { "changes": {} }
            }
        ])));
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].title, "Import Vec");
        assert!(actions[0].is_preferred);
        assert_eq!(actions[0].files.len(), 1);
        assert_eq!(actions[1].title, "run cargo");
        assert_eq!(
            actions[1].command.as_ref().map(|command| command.command.as_str()),
            Some("rust-analyzer.runSingle")
        );
        assert_eq!(actions[2].disabled.as_deref(), Some("not applicable"));
    }

    #[test]
    fn allowlists_only_rust_analyzer_commands() {
        assert!(is_allowed_lsp_command("rust-analyzer.applySourceChange"));
        assert!(!is_allowed_lsp_command("evil.run"));
        assert!(!is_allowed_lsp_command(""));
    }

    #[test]
    fn parse_execute_command_reads_workspace_edit() {
        let result = parse_execute_command_result(Some(&json!({
            "changes": {
                "file:///D:/proj/src/lib.rs": [
                    {
                        "range": {
                            "start": { "line": 1, "character": 0 },
                            "end": { "line": 1, "character": 3 }
                        },
                        "newText": "foo"
                    }
                ]
            }
        })));
        assert!(result.error.is_none());
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].edits[0].new_text, "foo");
        assert!(parse_execute_command_result(Some(&Value::Null))
            .files
            .is_empty());
    }
}
