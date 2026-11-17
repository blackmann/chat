//! A running language server as the app sees it: whether it's ready, what it
//! has been told about open files, and what it's busy with.
use super::{Client, ServerSpec, registry};
use lsp_types::{
    ClientCapabilities, ClientInfo, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentHighlightClientCapabilities, GeneralClientCapabilities, GotoCapability, HoverClientCapabilities, InitializeParams,
    InitializeResult, MarkupKind, NumberOrString, PositionEncodingKind, ProgressParams, ProgressParamsValue, ReferenceClientCapabilities,
    ServerCapabilities, TextDocumentClientCapabilities, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentSyncClientCapabilities, VersionedTextDocumentIdentifier, WindowClientCapabilities, WorkDoneProgress, WorkspaceFolder,
    notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Initialized},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Chat is installing it, on first use.
    Installing,
    Starting,
    Ready,
    /// The command isn't on PATH.
    NotFound,
    /// It wouldn't start, or it crashed.
    Failed(String),
    /// Stopped on purpose: it stays stopped until restarted.
    Stopped,
}

/// How positions count columns: UTF-8 bytes when the server agrees, else
/// UTF-16 code units, which every server takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

struct Doc {
    language: &'static str,
    version: i32,
    text: String,
}

pub struct Server {
    pub spec: ServerSpec,
    pub root: PathBuf,
    pub status: Status,
    pub client: Option<Arc<Client>>,
    pub capabilities: Option<ServerCapabilities>,
    pub encoding: Encoding,
    docs: HashMap<PathBuf, Doc>,
    /// Work the server reported begun and not yet ended, by token: its title,
    /// latest message and percentage.
    progress: Vec<(NumberOrString, String, Option<String>, Option<u32>)>,
    /// rust-analyzer says when it has settled (`experimental/serverStatus`),
    /// which progress reports alone don't: there are gaps between them.
    quiescent: Option<bool>,
    /// Resident memory of the server and the processes it started, in bytes,
    /// as last sampled.
    pub memory: Option<u64>,
    /// Why it's missing or failed, when there's more to say than the status.
    pub note: Option<String>,
}

impl Server {
    pub fn new(spec: ServerSpec, root: PathBuf, status: Status, client: Option<Arc<Client>>) -> Self {
        Self { spec, root, status, client, capabilities: None, encoding: Encoding::Utf16, docs: HashMap::new(), progress: Vec::new(), quiescent: None, memory: None, note: None }
    }

    fn ready_client(&self) -> Option<&Arc<Client>> {
        self.client.as_ref().filter(|_| self.status == Status::Ready)
    }

    pub fn has(&self, path: &Path) -> bool {
        self.docs.contains_key(path)
    }

    /// The text the server was last sent for `path`.
    pub fn text(&self, path: &Path) -> Option<&str> {
        self.docs.get(path).map(|d| d.text.as_str())
    }

    pub fn is_ready(&self) -> bool {
        self.ready_client().is_some()
    }

    pub fn documents(&self) -> impl Iterator<Item = &PathBuf> {
        self.docs.keys()
    }

    /// The server is answering: note what it can do and tell it about the
    /// files opened while it started.
    pub fn ready(&mut self, result: InitializeResult) {
        let utf8 = result.capabilities.position_encoding.as_ref().is_some_and(|e| *e == PositionEncodingKind::UTF8);
        self.encoding = if utf8 { Encoding::Utf8 } else { Encoding::Utf16 };
        // rust-analyzer sends the status it was asked for, but doesn't say it
        // will: until the first one comes, it's still loading.
        if result.server_info.as_ref().is_some_and(|i| i.name == "rust-analyzer") {
            self.quiescent = Some(false);
        }
        self.capabilities = Some(result.capabilities);
        self.status = Status::Ready;
        if let Some(client) = &self.client {
            client.notify::<Initialized>(lsp_types::InitializedParams {});
            for (path, doc) in &self.docs {
                send_open(client, path, doc);
            }
        }
    }

    pub fn failed(&mut self, why: String) {
        // A server stopped on purpose exiting is no failure.
        if self.status == Status::Stopped {
            return;
        }
        self.status = Status::Failed(why);
        self.wind_down();
    }

    /// Stop the server, and keep it stopped: files opened later wait for a restart.
    pub fn stop(&mut self) {
        self.status = Status::Stopped;
        self.wind_down();
    }

    fn wind_down(&mut self) {
        self.progress.clear();
        self.quiescent = None;
        self.memory = None;
        if let Some(client) = self.client.take() {
            client.shutdown();
        }
    }

    pub fn open(&mut self, path: &Path, language: &'static str, text: String) {
        if self.docs.contains_key(path) {
            return self.update(path, text);
        }
        let doc = Doc { language, version: 0, text };
        if let Some(client) = self.ready_client() {
            send_open(client, path, &doc);
        }
        self.docs.insert(path.to_path_buf(), doc);
    }

    pub fn close(&mut self, path: &Path) {
        if self.docs.remove(path).is_none() {
            return;
        }
        if let (Some(client), Some(uri)) = (self.ready_client(), registry::uri(path)) {
            client.notify::<DidCloseTextDocument>(DidCloseTextDocumentParams { text_document: TextDocumentIdentifier { uri } });
        }
    }

    /// The file's text now; the server hears about it only if it changed.
    pub fn update(&mut self, path: &Path, text: String) {
        let ready = self.ready_client().cloned();
        let Some(doc) = self.docs.get_mut(path).filter(|d| d.text != text) else { return };
        doc.version += 1;
        doc.text = text;
        if let (Some(client), Some(uri)) = (ready, registry::uri(path)) {
            client.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier { uri, version: doc.version },
                content_changes: vec![TextDocumentContentChangeEvent { range: None, range_length: None, text: doc.text.clone() }],
            });
        }
    }

    /// Take in a notification; whether anything shown changed.
    pub fn notified(&mut self, method: &str, params: serde_json::Value) -> bool {
        if method == "experimental/serverStatus" {
            let quiescent = params.get("quiescent").and_then(serde_json::Value::as_bool);
            let changed = quiescent != self.quiescent;
            self.quiescent = quiescent;
            return changed;
        }
        if method != "$/progress" {
            return false;
        }
        let Ok(ProgressParams { token, value: ProgressParamsValue::WorkDone(work) }) = serde_json::from_value(params) else {
            return false;
        };
        match work {
            WorkDoneProgress::Begin(b) => {
                self.progress.retain(|(t, ..)| *t != token);
                self.progress.push((token, b.title, b.message, b.percentage));
            }
            WorkDoneProgress::Report(r) => {
                if let Some(p) = self.progress.iter_mut().find(|(t, ..)| *t == token) {
                    if r.message.is_some() {
                        p.2 = r.message;
                    }
                    if r.percentage.is_some() {
                        p.3 = r.percentage;
                    }
                }
            }
            WorkDoneProgress::End(_) => self.progress.retain(|(t, ..)| *t != token),
        }
        true
    }

    /// What the server is busy with, as a short phrase, if anything.
    pub fn busy(&self) -> Option<String> {
        let Some((_, title, _, pct)) = self.progress.last() else {
            return (self.quiescent == Some(false)).then(|| "Loading…".to_string());
        };
        Some(match pct {
            Some(p) => format!("{title} {p}%"),
            None => format!("{title}…"),
        })
    }
}

fn send_open(client: &Client, path: &Path, doc: &Doc) {
    let Some(uri) = registry::uri(path) else { return };
    client.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem { uri, language_id: doc.language.into(), version: doc.version, text: doc.text.clone() },
    });
}

/// What the app tells a server it can do: read-only navigation, so no edits,
/// completions or diagnostics are asked for.
#[allow(deprecated)] // `root_uri`: older servers still read it.
pub fn initialize_params(spec: &ServerSpec, root: &Path) -> InitializeParams {
    let uri = registry::uri(root);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: uri.clone(),
        initialization_options: spec.options.clone(),
        workspace_folders: uri.map(|uri| vec![WorkspaceFolder { uri, name }]),
        client_info: Some(ClientInfo { name: "chat".into(), version: Some(env!("CARGO_PKG_VERSION").into()) }),
        capabilities: ClientCapabilities {
            general: Some(GeneralClientCapabilities {
                position_encodings: Some(vec![PositionEncodingKind::UTF8, PositionEncodingKind::UTF16]),
                ..Default::default()
            }),
            window: Some(WindowClientCapabilities { work_done_progress: Some(true), ..Default::default() }),
            text_document: Some(TextDocumentClientCapabilities {
                synchronization: Some(TextDocumentSyncClientCapabilities { dynamic_registration: Some(false), ..Default::default() }),
                definition: Some(GotoCapability { dynamic_registration: Some(false), link_support: Some(true) }),
                type_definition: Some(GotoCapability { dynamic_registration: Some(false), link_support: Some(true) }),
                implementation: Some(GotoCapability { dynamic_registration: Some(false), link_support: Some(true) }),
                references: Some(ReferenceClientCapabilities { dynamic_registration: Some(false) }),
                document_highlight: Some(DocumentHighlightClientCapabilities { dynamic_registration: Some(false) }),
                hover: Some(HoverClientCapabilities {
                    dynamic_registration: Some(false),
                    content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                }),
                ..Default::default()
            }),
            // rust-analyzer's word for when it has finished loading.
            experimental: Some(serde_json::json!({ "serverStatusNotification": true })),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A short hint at how to install a default server.
pub fn install_hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "rust-analyzer" => "rustup component add rust-analyzer",
        "typescript-language-server" => "npm i -g typescript-language-server typescript",
        "vscode-json-language-server" | "vscode-css-language-server" | "vscode-html-language-server" => {
            "npm i -g vscode-langservers-extracted"
        }
        "gopls" => "go install golang.org/x/tools/gopls@latest",
        "basedpyright" => "pip install basedpyright",
        "pyright" => "npm i -g pyright",
        "clangd" => "xcode-select --install, or brew install llvm",
        "yaml-language-server" => "npm i -g yaml-language-server",
        "taplo" => "cargo install taplo-cli --features lsp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server() -> Server {
        Server::new(ServerSpec::defaults().remove(0), PathBuf::from("/tmp"), Status::Starting, None)
    }

    #[test]
    fn tracks_progress_until_it_ends() {
        let mut s = server();
        let begin = json!({ "token": "idx", "value": { "kind": "begin", "title": "Indexing" } });
        assert!(s.notified("$/progress", begin));
        assert_eq!(s.busy().as_deref(), Some("Indexing…"));
        s.notified("$/progress", json!({ "token": "idx", "value": { "kind": "report", "percentage": 40 } }));
        assert_eq!(s.busy().as_deref(), Some("Indexing 40%"));
        s.notified("$/progress", json!({ "token": "idx", "value": { "kind": "end" } }));
        assert_eq!(s.busy(), None);
        assert!(!s.notified("textDocument/publishDiagnostics", json!({})));
    }

    #[test]
    fn stays_busy_until_rust_analyzer_settles() {
        let mut s = server();
        assert!(s.notified("experimental/serverStatus", json!({ "health": "ok", "quiescent": false })));
        assert_eq!(s.busy().as_deref(), Some("Loading…"));
        // Progress, when there is some, says more.
        s.notified("$/progress", json!({ "token": "t", "value": { "kind": "begin", "title": "Indexing" } }));
        assert_eq!(s.busy().as_deref(), Some("Indexing…"));
        s.notified("$/progress", json!({ "token": "t", "value": { "kind": "end" } }));
        assert_eq!(s.busy().as_deref(), Some("Loading…"));
        s.notified("experimental/serverStatus", json!({ "health": "ok", "quiescent": true }));
        assert_eq!(s.busy(), None);
    }

    #[test]
    fn versions_go_up_only_when_text_changes() {
        let mut s = server();
        let p = Path::new("/tmp/a.rs");
        s.open(p, "rust", "fn a() {}".into());
        s.update(p, "fn a() {}".into());
        assert_eq!(s.docs[p].version, 0);
        s.update(p, "fn b() {}".into());
        assert_eq!(s.docs[p].version, 1);
        s.close(p);
        assert!(!s.has(p));
    }
}
