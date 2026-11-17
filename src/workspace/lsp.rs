//! The workspace's language servers: one per server and project root, started
//! the first time a file it serves opens in a tab and kept while the app runs.
//! Open files are sent as they are on disk and sent again when they change there.
use super::panes::{LineKind, LineSource, diff_lines};
use super::symbols::{ServerMarks, SymbolMark, word_at};
use super::*;
use crate::lsp::{self, Client, Encoding, Incoming, Server, ServerSpec, Status, position};
use crate::menu::{Entry, Item};
use futures::{FutureExt, future::LocalBoxFuture};
use lsp_types::{
    DocumentHighlightKind, DocumentHighlightParams, GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, MarkedString,
    MarkupKind, OneOf, Position, Range as LspRange, ReferenceContext, ReferenceParams, TextDocumentIdentifier, TextDocumentPositionParams,
    WorkspaceSymbolParams, WorkspaceSymbolResponse,
    request::{DocumentHighlightRequest, GotoDefinition, HoverRequest, Initialize, References, WorkspaceSymbolRequest},
};
use std::{ops::Range, sync::Arc};

/// The anchor the definitions and references list opens at.
pub(super) const LOCATIONS: &str = "lsp-locations";
/// How long the pointer rests on a word before asking what it is.
pub(super) const HOVER_DELAY: Duration = Duration::from_millis(450);
/// How long a server with no open files is kept, in case one opens again.
pub(super) const IDLE: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub(super) struct Servers {
    next: u64,
    pub(super) all: HashMap<u64, Server>,
    /// Files reached by going to a definition, and the server that found them:
    /// a crate's sources or `node_modules` are read by the project's server, not one of their own.
    pub(super) adopted: HashMap<PathBuf, u64>,
    /// The word under the pointer while ⌘ is held.
    pub link: Option<Link>,
    pub(super) hover: Option<HoverCard>,
    pub(super) notice: Option<(SharedString, Point<Pixels>, u64)>,
    /// Places jumped from and, after going back, the places to go forward to.
    pub(super) back: Vec<Spot>,
    pub(super) forward: Vec<Spot>,
    /// Where the last jump landed.
    pub(super) at: Option<Spot>,
    /// Counts picks and notices, so late replies and timers can tell they're stale.
    ticket: u64,
    /// Each server's log, kept across restarts.
    pub(super) logs: HashMap<super::servers::Key, std::collections::VecDeque<SharedString>>,
    pub(super) log_scroll: HashMap<super::servers::Key, UniformListScrollHandle>,
    /// Memory is being read every so often.
    pub(super) sampling: bool,
}

/// A word drawn as a link to its definition.
#[derive(Clone, PartialEq)]
pub(super) struct Link {
    pub source: LineSource,
    pub row: usize,
    pub range: Range<usize>,
}

/// A place to go back to: a tab and, in a file, its row.
#[derive(Clone, PartialEq)]
pub(super) struct Spot {
    pub(super) tab: TabKind,
    pub(super) row: Option<usize>,
}

pub(super) struct HoverCard {
    pub(super) key: (LineSource, usize, Range<usize>),
    point: Point<Pixels>,
    pub(super) text: Option<SharedString>,
    _ask: Task<()>,
}

/// A place in a pane as its server knows it.
struct Target {
    id: u64,
    client: Arc<Client>,
    path: PathBuf,
    position: Position,
}
impl Target {
    fn at(&self) -> TextDocumentPositionParams {
        TextDocumentPositionParams { text_document: TextDocumentIdentifier { uri: lsp::uri(&self.path).expect("an absolute path") }, position: self.position }
    }
}

type Locations = Vec<(PathBuf, LspRange)>;

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Definition,
    References,
}
impl Look {
    fn none(self) -> &'static str {
        match self {
            Self::Definition => "No definition found",
            Self::References => "No references found",
        }
    }
}

fn source_path(source: &LineSource) -> &Path {
    match source {
        LineSource::File(path) | LineSource::Diff(path) | LineSource::GitDiff(path, _) => path,
    }
}

fn tab_of(source: &LineSource) -> TabKind {
    match source {
        LineSource::File(path) => TabKind::File(path.clone()),
        LineSource::Diff(path) => TabKind::Diff(path.clone()),
        LineSource::GitDiff(path, staged) => TabKind::GitDiff(path.clone(), *staged),
    }
}

fn definitions(found: Option<GotoDefinitionResponse>) -> Locations {
    let one = |l: lsp_types::Location| lsp::path(&l.uri).map(|p| (p, l.range));
    match found {
        None => Vec::new(),
        Some(GotoDefinitionResponse::Scalar(l)) => one(l).into_iter().collect(),
        Some(GotoDefinitionResponse::Array(ls)) => ls.into_iter().filter_map(one).collect(),
        Some(GotoDefinitionResponse::Link(ls)) => ls.into_iter().filter_map(|l| lsp::path(&l.target_uri).map(|p| (p, l.target_selection_range))).collect(),
    }
}

/// Symbols named exactly `word`, from a fuzzy workspace search.
fn named(found: Option<WorkspaceSymbolResponse>, word: &str) -> Locations {
    match found {
        None => Vec::new(),
        Some(WorkspaceSymbolResponse::Flat(symbols)) => {
            symbols.into_iter().filter(|s| s.name == word).filter_map(|s| lsp::path(&s.location.uri).map(|p| (p, s.location.range))).collect()
        }
        Some(WorkspaceSymbolResponse::Nested(symbols)) => symbols
            .into_iter()
            .filter(|s| s.name == word)
            .filter_map(|s| match s.location {
                OneOf::Left(l) => lsp::path(&l.uri).map(|p| (p, l.range)),
                OneOf::Right(_) => None,
            })
            .collect(),
    }
}

/// A piece of a hover card: code, highlighted in its language, or prose.
#[derive(Debug, PartialEq)]
pub(super) enum HoverPart {
    Code { lang: String, text: String },
    Text(String),
}

/// A hover's Markdown cut into the pieces its card stacks: each fenced block
/// on its own, the prose between them, and a rule (`---`) as a break.
pub(super) fn hover_parts(markdown: &str) -> Vec<HoverPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, parts: &mut Vec<HoverPart>| {
        let t = text.trim();
        if !t.is_empty() {
            parts.push(HoverPart::Text(t.to_string()));
        }
        text.clear();
    };
    let mut lines = markdown.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if let Some(lang) = trimmed.strip_prefix("```") {
            flush(&mut text, &mut parts);
            let code: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
            let code = code.join("\n");
            if !code.trim().is_empty() {
                parts.push(HoverPart::Code { lang: lang.trim().to_string(), text: code });
            }
        } else if matches!(trimmed, "---" | "***" | "___") {
            flush(&mut text, &mut parts);
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    flush(&mut text, &mut parts);
    parts
}

/// A hover's contents as Markdown, code in fences.
fn hover_markdown(contents: HoverContents) -> Option<String> {
    let part = |m: MarkedString| match m {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(l) => format!("```{}\n{}\n```", l.language, l.value),
    };
    let text = match contents {
        HoverContents::Scalar(m) => part(m),
        HoverContents::Array(parts) => parts.into_iter().map(part).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(m) => match m.kind {
            MarkupKind::Markdown => m.value,
            MarkupKind::PlainText => format!("```\n{}\n```", m.value),
        },
    };
    (!text.trim().is_empty()).then_some(text)
}

/// How a server's state reads in a pane header.
pub(super) struct Badge {
    pub text: SharedString,
    /// Something's wrong: draw it as a warning.
    pub warn: bool,
    pub tooltip: Option<SharedString>,
}

fn read_text(path: &Path) -> Option<String> {
    let size = std::fs::metadata(path).ok()?.len();
    (size <= MAX_FILE).then(|| std::fs::read_to_string(path).ok()).flatten()
}

/// Open on `server` the files a server it replaces had open.
fn adopt(server: &mut Server, paths: Vec<PathBuf>) {
    for path in paths {
        if let (Some(language), Some(text)) = (lsp::language_id(&path), read_text(&path)) {
            server.open(&path, language, text);
        }
    }
}

impl Workspace {
    /// The server for `path`, starting one if none that serves it is running.
    fn lsp_server_for(&mut self, path: &Path, cx: &mut Context<Self>) -> Option<u64> {
        let language = lsp::language_id(path)?;
        let specs: Vec<ServerSpec> =
            ServerSpec::merged(&self.settings.language_servers).into_iter().filter(|s| s.languages.iter().any(|l| l == language)).collect();
        if specs.is_empty() {
            return None;
        }
        if let Some(&id) = self.servers.adopted.get(path)
            && self.servers.all.get(&id).is_some_and(|s| matches!(s.status, Status::Starting | Status::Ready | Status::Stopped))
        {
            return Some(id);
        }
        let serves = |s: &Server| specs.iter().any(|spec| spec.name == s.spec.name) && path.starts_with(&s.root);
        let existing = self.servers.all.iter().filter(|(_, s)| serves(s)).max_by_key(|(_, s)| s.root.as_os_str().len()).map(|(id, s)| (*id, s.status.clone()));
        // Missing or crashed: look again, in case it's been installed since or
        // crashed by chance, and hand the new one the files the old one had.
        // One stopped on purpose stays stopped.
        let orphans: Vec<PathBuf> = match existing {
            Some((id, Status::Starting | Status::Ready | Status::Stopped)) => return Some(id),
            Some((id, _)) => self.servers.all.remove(&id).map(|s| s.documents().cloned().collect()).unwrap_or_default(),
            None => Vec::new(),
        };

        // One that's there already, on PATH or installed by Chat; else the
        // first Chat can install; else the first, to say it's missing.
        let env = crate::agent::shell_path();
        let ready = specs.iter().find(|s| lsp::install::locate(s, env.as_deref()).is_some());
        let installable = specs.iter().find(|s| lsp::install::missing(s, env.as_deref()).is_none());
        let spec = ready.or(installable).unwrap_or(&specs[0]).clone();
        let root = lsp::root_for(path, &spec.roots, &self.current_project().roots());
        Some(self.start_server(spec, root, orphans, cx))
    }

    /// Start `spec` at `root` and open `files` on it. A command that isn't
    /// installed, or won't start, leaves a server that says so.
    pub(super) fn start_server(&mut self, spec: ServerSpec, root: PathBuf, files: Vec<PathBuf>, cx: &mut Context<Self>) -> u64 {
        let env = crate::agent::shell_path();
        let id = self.servers.next;
        self.servers.next += 1;
        let command = lsp::install::locate(&spec, env.as_deref()).filter(|_| !lsp::install::needs_install(&spec, &root, env.as_deref()));
        let key = (spec.name.clone(), root.clone());
        // Started with what it needs to know about this project (where
        // TypeScript is); kept apart from the spec, which restarts reuse.
        let mut started = spec.clone();
        started.options = lsp::install::options(&spec, &root);
        let mut server = match command {
            None => match lsp::install::missing(&spec, env.as_deref()) {
                // Not there, but Chat can install it: do, then start it.
                None => {
                    self.log(&key, format!("{} isn't installed; installing it", spec.name), cx);
                    self.install_server(id, spec.clone(), false, cx);
                    Server::new(spec, root, Status::Installing, None)
                }
                Some(why) => {
                    self.log(&key, format!("{} isn't installed: {why}", spec.name), cx);
                    let mut server = Server::new(spec, root, Status::NotFound, None);
                    server.note = Some(why);
                    server
                }
            },
            Some(command) => match Client::spawn(&command.to_string_lossy(), &spec.args, &root, env.as_deref()) {
                Ok((client, incoming)) => {
                    self.log(&key, format!("started {} in {}", command.display(), root.display()), cx);
                    if let Some(options) = &started.options {
                        self.log(&key, format!("with options {options}"), cx);
                    }
                    self.lsp_listen(id, &started, &root, &client, incoming, cx);
                    Server::new(spec, root, Status::Starting, Some(client))
                }
                Err(e) => {
                    self.log(&key, format!("{e:#}"), cx);
                    Server::new(spec, root, Status::Failed(format!("{e:#}")), None)
                }
            },
        };
        adopt(&mut server, files);
        self.servers.all.insert(id, server);
        self.sample_memory(cx);
        id
    }

    /// Install a server off the UI thread, logging what the installer says,
    /// then start it on the files opened in the meantime.
    pub(super) fn install_server(&mut self, id: u64, spec: ServerSpec, latest: bool, cx: &mut Context<Self>) {
        let env = crate::agent::shell_path();
        cx.spawn(async move |this, cx| {
            let job = spec.clone();
            let outcome = cx.background_executor().spawn(async move { lsp::install::install(&job, env.as_deref(), latest) }).await;
            this.update(cx, |s, cx| {
                let Some(server) = s.servers.all.get_mut(&id).filter(|srv| srv.status == Status::Installing) else {
                    // Closed or restarted meanwhile; the install is kept for next time.
                    return;
                };
                let key = (server.spec.name.clone(), server.root.clone());
                match outcome.result {
                    Ok(_) => {
                        let (root, files): (PathBuf, Vec<PathBuf>) = (server.root.clone(), server.documents().cloned().collect());
                        s.servers.all.remove(&id);
                        for line in outcome.output {
                            s.log(&key, line, cx);
                        }
                        let new = s.start_server(spec, root, files, cx);
                        for owner in s.servers.adopted.values_mut() {
                            if *owner == id {
                                *owner = new;
                            }
                        }
                    }
                    Err(why) => {
                        server.status = Status::Failed(format!("Couldn't install it: {why}"));
                        for line in outcome.output {
                            s.log(&key, line, cx);
                        }
                        s.log(&key, format!("couldn't install: {why}"), cx);
                    }
                }
                s.refresh_servers_menu(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Initialize a server that just started, and keep up with what it says.
    fn lsp_listen(&self, id: u64, spec: &ServerSpec, root: &Path, client: &Arc<Client>, mut incoming: futures::channel::mpsc::UnboundedReceiver<Incoming>, cx: &mut Context<Self>) {
        let init = client.call::<Initialize>(lsp::initialize_params(spec, root));
        cx.spawn(async move |this, cx| {
            let result = init.await;
            this.update(cx, |s, cx| {
                let Some(server) = s.servers.all.get_mut(&id) else { return };
                match result {
                    Ok(result) => server.ready(result),
                    Err(e) => server.failed(format!("{e:#}")),
                }
                s.refresh_servers_menu(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(event) = incoming.next().await {
                let done = this.update(cx, |s, cx| {
                    let Some(server) = s.servers.all.get_mut(&id) else { return true };
                    match event {
                        Incoming::Log(line) => {
                            let key = (server.spec.name.clone(), server.root.clone());
                            s.log(&key, line, cx);
                            false
                        }
                        Incoming::Notification { method, params } => {
                            if server.notified(&method, params) {
                                s.refresh_servers_menu(cx);
                                cx.notify();
                            }
                            false
                        }
                        Incoming::Exited(why) => {
                            let key = (server.spec.name.clone(), server.root.clone());
                            server.failed(why.clone());
                            s.log(&key, why, cx);
                            s.refresh_servers_menu(cx);
                            cx.notify();
                            true
                        }
                    }
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
    }

    /// A file opened in a tab: tell its server, starting one if need be.
    pub(super) fn lsp_open(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(language) = lsp::language_id(path) else { return };
        let Some(text) = read_text(path) else { return };
        if let Some(id) = self.lsp_server_for(path, cx)
            && let Some(server) = self.servers.all.get_mut(&id)
        {
            server.open(path, language, text);
        }
    }

    pub(super) fn lsp_close(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.servers.adopted.remove(path);
        let mut idle = Vec::new();
        for (id, server) in self.servers.all.iter_mut() {
            if server.has(path) {
                server.close(path);
                if server.documents().next().is_none() {
                    idle.push(*id);
                }
            }
        }
        for id in idle {
            if self.servers.all.get(&id).is_some_and(|s| s.client.is_none()) {
                // Nothing running to keep: a missing or stopped server is looked for again later.
                self.servers.all.remove(&id);
                continue;
            }
            // Kept a while: indexing a project again is slow.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(IDLE).await;
                this.update(cx, |s, cx| {
                    if s.servers.all.get(&id).is_some_and(|s| s.documents().next().is_none())
                        && let Some(server) = s.servers.all.remove(&id)
                        && let Some(client) = &server.client
                    {
                        client.shutdown();
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Files may have changed on disk: show the open ones as they are now, and
    /// send servers the ones that changed.
    pub(super) fn lsp_sync(&mut self) {
        let stale: Vec<PathBuf> = self
            .files
            .iter()
            .filter_map(|(path, view)| view.as_ref().ok().filter(|v| read_text(path).is_some_and(|t| t != v.text)).map(|_| path.clone()))
            .collect();
        for path in stale {
            self.files.insert(path.clone(), read_lines(&path));
            // The server's ranges were for the old text.
            if let Some(mark) = self.symbol.as_mut().filter(|m| source_path(&m.source) == path) {
                mark.server = None;
            }
        }
        for server in self.servers.all.values_mut() {
            let paths: Vec<PathBuf> = server.documents().cloned().collect();
            for path in paths {
                if let Some(text) = read_text(&path) {
                    server.update(&path, text);
                }
            }
        }
    }

    /// Stop every server, on the way out.
    pub(super) fn lsp_shutdown(&mut self) {
        for server in self.servers.all.values_mut() {
            if let Some(client) = server.client.take() {
                client.kill();
            }
        }
    }

    /// The state of the server reading `path`, for the pane header.
    pub(super) fn lsp_badge(&self, path: &Path) -> Option<Badge> {
        let server = self.servers.all.values().find(|s| s.has(path))?;
        let name = &server.spec.name;
        let (text, warn, tooltip) = match &server.status {
            Status::Installing => (format!("{name} installing…"), false, Some("Installing it for you, on first use. Its output is in the ⚡ menu's logs.".to_string())),
            Status::Starting => (format!("{name} starting…"), false, None),
            Status::Ready => match server.busy() {
                Some(work) => (format!("{name} · {work}"), false, None),
                None => (name.clone(), false, Some(format!("{name} is running in {}", server.root.display()))),
            },
            Status::NotFound => (
                format!("{name} not found"),
                true,
                Some(match (&server.note, lsp::install_hint(name)) {
                    (Some(note), Some(hint)) => format!("{note} Or install it yourself: {hint}"),
                    (Some(note), None) => note.clone(),
                    (None, Some(hint)) => format!("Install it to go to definitions here: {hint}"),
                    (None, None) => format!("`{}` isn't on your PATH.", server.spec.command),
                }),
            ),
            Status::Failed(why) => (format!("{name} crashed"), true, Some(why.clone())),
            Status::Stopped => (format!("{name} stopped"), false, Some("Restart it from the ⚡ menu in the status bar.".to_string())),
        };
        Some(Badge { text: text.into(), warn, tooltip: tooltip.map(Into::into) })
    }
}

impl Workspace {
    /// The ready server reading `path`.
    fn ready_server(&self, path: &Path) -> Option<(u64, &Server)> {
        self.servers.all.iter().find(|(_, s)| s.is_ready() && s.has(path)).map(|(id, s)| (*id, s))
    }

    /// Whether ⌘-click can look things up in `source`.
    fn navigable(&self, source: &LineSource) -> bool {
        self.ready_server(source_path(source)).is_some()
    }

    /// Where byte `shown` of `row` in a pane is, in the server's copy of its
    /// file. A diff's row is found there through the lines that didn't change;
    /// removed lines, and files the pane shows differently from the server,
    /// have no place.
    fn target(&self, source: &LineSource, row: usize, shown: usize) -> Option<Target> {
        let path = source_path(source);
        let (id, server) = self.ready_server(path)?;
        let doc = server.text(path)?;
        let line = match source {
            LineSource::File(_) => {
                let Some(Ok(file)) = self.files.get(path) else { return None };
                (file.text == doc).then_some(row)?
            }
            LineSource::Diff(_) | LineSource::GitDiff(..) => {
                let (old, new) = self.diff_sides(source)?;
                let shown_line = diff_lines(&old, &new).into_iter().nth(row)?;
                let n = shown_line.new.filter(|_| shown_line.kind != LineKind::Del)?;
                position::line_map(&new, doc).get(n - 1).copied().flatten()?
            }
        };
        let position = position::position(doc, line, shown, syntax::tab_width(), server.encoding)?;
        Some(Target { id, client: server.client.clone()?, path: path.to_path_buf(), position })
    }

    /// A row of a pane as it's shown.
    pub(super) fn pane_line(&self, source: &LineSource, row: usize) -> Option<SharedString> {
        match source {
            LineSource::File(path) => match self.files.get(path) {
                Some(Ok(file)) => file.lines.get(row).cloned(),
                _ => None,
            },
            _ => {
                let (old, new) = self.diff_sides(source)?;
                diff_lines(&old, &new).into_iter().nth(row).map(|l| l.text.into())
            }
        }
    }

    // ── Picking and highlighting ─────────────────────────────────────────────

    /// A plain click at byte `at` of a code line: pick the word there, or let
    /// the picked one go when the click missed every word.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pick_symbol(&mut self, source: LineSource, row: usize, line: &str, at: Option<usize>, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // Away from the composer, so Escape and F12 reach the workspace.
        self.focus.focus(window);
        let Some(range) = at.and_then(|ix| word_at(line, ix)) else {
            if self.symbol.take().is_some() {
                cx.notify();
            }
            return;
        };
        self.servers.ticket += 1;
        let word = SharedString::from(line[range.clone()].to_string());
        self.symbol = Some(SymbolMark { source, word, row, at: range.start, point, server: None, id: self.servers.ticket });
        self.request_highlights(cx);
        cx.notify();
    }

    /// Ask the server where the picked symbol is used in its pane; the text
    /// match shows until it answers, and stays if it has nothing to say.
    fn request_highlights(&mut self, cx: &mut Context<Self>) {
        let Some(mark) = &self.symbol else { return };
        let Some(target) = self.target(&mark.source, mark.row, mark.at) else { return };
        let ticket = mark.id;
        let params = DocumentHighlightParams { text_document_position_params: target.at(), work_done_progress_params: Default::default(), partial_result_params: Default::default() };
        let asked = target.client.call::<DocumentHighlightRequest>(params);
        cx.spawn(async move |this, cx| {
            let Ok(Some(found)) = asked.await else { return };
            this.update(cx, |s, cx| {
                if s.symbol.as_ref().is_none_or(|m| m.id != ticket) || found.is_empty() {
                    return;
                }
                let ranges: Vec<(LspRange, bool)> = found.into_iter().map(|h| (h.range, h.kind == Some(DocumentHighlightKind::WRITE))).collect();
                let marks = s.server_marks(target.id, &target.path, &ranges);
                if let Some(mark) = s.symbol.as_mut() {
                    mark.server = marks;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// A server's ranges in a file, laid onto the rows of the picked symbol's pane.
    fn server_marks(&self, id: u64, path: &Path, ranges: &[(LspRange, bool)]) -> Option<ServerMarks> {
        let source = &self.symbol.as_ref()?.source;
        let server = self.servers.all.get(&id)?;
        let doc = server.text(path)?;
        let (tab, encoding) = (syntax::tab_width(), server.encoding);
        // Which pane rows show each line of the server's copy.
        let mut marks = ServerMarks::default();
        let mut rows_of: HashMap<usize, Vec<usize>> = HashMap::new();
        match source {
            LineSource::File(_) => {}
            _ => {
                let (old, new) = self.diff_sides(source)?;
                let map = position::line_map(&new, doc);
                for (row, line) in diff_lines(&old, &new).iter().enumerate() {
                    match line.new.filter(|_| line.kind != LineKind::Del).and_then(|n| map.get(n - 1).copied().flatten()) {
                        Some(k) => rows_of.entry(k).or_default().push(row),
                        None if line.kind != LineKind::Hunk => {
                            marks.unmapped.insert(row);
                        }
                        None => {}
                    }
                }
            }
        }
        let file = matches!(source, LineSource::File(_));
        for (range, write) in ranges {
            let k = range.start.line as usize;
            // A range running past its line is cut at the line's end.
            let end = if range.end.line == range.start.line { range.end } else { Position::new(range.start.line, u32::MAX) };
            let (Some(a), Some(b)) = (position::shown(doc, range.start, tab, encoding), position::shown(doc, end, tab, encoding)) else { continue };
            let rows = if file { vec![k] } else { rows_of.get(&k).cloned().unwrap_or_default() };
            for row in rows {
                marks.rows.entry(row).or_default().push((a..b, *write));
            }
        }
        Some(marks)
    }

    // ── The pointer over code ────────────────────────────────────────────────

    /// The pointer moved over a code line: with ⌘ held the word under it is a
    /// link; without, resting on it asks the server what it is.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hover_code(&mut self, source: &LineSource, row: usize, line: &str, at: Option<usize>, cmd: bool, point: Point<Pixels>, cx: &mut Context<Self>) {
        let range = at.and_then(|ix| word_at(line, ix));
        let link = range.clone().filter(|_| cmd && self.navigable(source)).map(|range| Link { source: source.clone(), row, range });
        if self.servers.link != link {
            self.servers.link = link;
            cx.notify();
        }
        let key = range.filter(|_| !cmd).map(|r| (source.clone(), row, r));
        if self.servers.hover.as_ref().map(|h| &h.key) == key.as_ref() {
            return;
        }
        if self.servers.hover.take().is_some_and(|h| h.text.is_some()) {
            cx.notify();
        }
        let Some(key) = key else { return };
        if !self.navigable(source) {
            return;
        }
        let (src, row_, start) = (key.0.clone(), key.1, key.2.start);
        let ask = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let Ok(Some(asked)) = this.update(cx, |s, _| {
                let target = s.target(&src, row_, start)?;
                Some(target.client.call::<HoverRequest>(HoverParams { text_document_position_params: target.at(), work_done_progress_params: Default::default() }))
            }) else {
                return;
            };
            let Ok(Some(hover)) = asked.await else { return };
            let Some(text) = hover_markdown(hover.contents) else { return };
            this.update(cx, |s, cx| {
                if let Some(card) = s.servers.hover.as_mut().filter(|h| h.key.0 == src && h.key.1 == row_ && h.key.2.start == start) {
                    card.text = Some(text.into());
                    cx.notify();
                }
            })
            .ok();
        });
        self.servers.hover = Some(HoverCard { key, point, text: None, _ask: ask });
    }

    /// ⌘ pressed or let go: the word resting under the pointer becomes a link,
    /// or stops being one.
    pub(super) fn lsp_modifiers(&mut self, cmd: bool, cx: &mut Context<Self>) {
        if !cmd {
            if self.servers.link.take().is_some() {
                cx.notify();
            }
            return;
        }
        let Some(card) = self.servers.hover.take() else { return };
        let (source, row, range) = card.key;
        if self.navigable(&source) {
            self.servers.link = Some(Link { source, row, range });
        }
        cx.notify();
    }

    /// The pointer left a code line.
    pub(super) fn leave_code(&mut self, source: &LineSource, row: usize, cx: &mut Context<Self>) {
        if self.servers.link.as_ref().is_some_and(|l| &l.source == source && l.row == row) {
            self.servers.link = None;
            cx.notify();
        }
        if self.servers.hover.as_ref().is_some_and(|h| &h.key.0 == source && h.key.1 == row) && self.servers.hover.take().is_some_and(|h| h.text.is_some()) {
            cx.notify();
        }
    }

    // ── Definitions and references ───────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub(super) fn go_to_definition_at(&mut self, source: LineSource, row: usize, line: &str, at: Option<usize>, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.look(Look::Definition, source, row, line, at, point, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn find_references_at(&mut self, source: LineSource, row: usize, line: &str, at: Option<usize>, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.look(Look::References, source, row, line, at, point, window, cx);
    }

    /// F12: the picked symbol's definition.
    pub(super) fn go_to_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.look_at_picked(Look::Definition, window, cx);
    }

    /// ⇧F12: where the picked symbol is used.
    pub(super) fn find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.look_at_picked(Look::References, window, cx);
    }

    fn look_at_picked(&mut self, look: Look, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mark) = self.symbol.clone() else {
            self.notice("Click a symbol first", window.mouse_position(), cx);
            return;
        };
        let Some(line) = self.pane_line(&mark.source, mark.row) else { return };
        self.look(look, mark.source, mark.row, &line, Some(mark.at), mark.point, window, cx);
    }

    /// Ask the server for the word at byte `at` of a row's definitions or
    /// references, and go there or list them. A diff's removed line has no
    /// place in the file, so its word is looked up by name instead.
    #[allow(clippy::too_many_arguments)]
    fn look(&mut self, look: Look, source: LineSource, row: usize, line: &str, at: Option<usize>, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        let Some(range) = at.and_then(|ix| word_at(line, ix)) else { return };
        let word = line[range.clone()].to_string();
        let origin = Spot { row: matches!(source, LineSource::File(_)).then_some(row), tab: tab_of(&source) };
        let asked: LocalBoxFuture<'static, anyhow::Result<Locations>>;
        let id;
        if let Some(target) = self.target(&source, row, range.start) {
            id = target.id;
            asked = match look {
                Look::Definition => {
                    let params = GotoDefinitionParams { text_document_position_params: target.at(), work_done_progress_params: Default::default(), partial_result_params: Default::default() };
                    target.client.call::<GotoDefinition>(params).map(|r| r.map(definitions)).boxed_local()
                }
                Look::References => {
                    let params = ReferenceParams {
                        text_document_position: target.at(),
                        context: ReferenceContext { include_declaration: true },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    };
                    let one = |l: lsp_types::Location| lsp::path(&l.uri).map(|p| (p, l.range));
                    target.client.call::<References>(params).map(move |r| r.map(|ls| ls.unwrap_or_default().into_iter().filter_map(one).collect())).boxed_local()
                }
            };
        } else if let (Look::Definition, Some((server_id, server))) = (look, self.ready_server(source_path(&source))) {
            let Some(client) = server.client.clone() else { return };
            id = server_id;
            let params = WorkspaceSymbolParams { query: word.clone(), work_done_progress_params: Default::default(), partial_result_params: Default::default() };
            asked = client.call::<WorkspaceSymbolRequest>(params).map(move |r| r.map(|found| named(found, &word))).boxed_local();
        } else {
            let why = match self.lsp_badge(source_path(&source)) {
                _ if self.navigable(&source) => "This line can't be looked up".into(),
                Some(badge) => badge.text,
                None => "No language server for this file".into(),
            };
            self.notice(why, point, cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let found = asked.await;
            this.update_in(cx, |s, window, cx| match found {
                Ok(found) => s.show_locations(id, origin, found, look, point, window, cx),
                Err(e) => s.notice(format!("{e:#}"), point, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Go to the one place found, or list them to pick from.
    #[allow(clippy::too_many_arguments)]
    fn show_locations(&mut self, id: u64, origin: Spot, found: Locations, look: Look, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let mut found = lsp::unique(found);
        match found.len() {
            0 => self.notice(look.none(), point, cx),
            1 => {
                let (path, range) = found.remove(0);
                self.jump(id, Some(origin), path, range, cx);
            }
            n => {
                let project = self.current_project();
                let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
                let rows: Vec<(SharedString, SharedString, PathBuf, LspRange)> = found
                    .into_iter()
                    .map(|(path, range)| {
                        let text = texts.entry(path.clone()).or_insert_with(|| read_text(&path));
                        let line = text.as_deref().and_then(|t| position::line(t, range.start.line as usize)).unwrap_or("").trim().to_string();
                        let label = format!("{}:{}", project.relative(&path).display(), range.start.line + 1);
                        (label.into(), line.into(), path, range)
                    })
                    .collect();
                let rows = Rc::new(rows);
                let weak = cx.weak_entity();
                let title: SharedString = match look {
                    Look::Definition => format!("{n} definitions").into(),
                    Look::References => format!("{n} references").into(),
                };
                let build: crate::menu::Builder = Rc::new(move |_| {
                    let mut entries = vec![Entry::Label(title.clone())];
                    entries.extend(rows.iter().map(|(label, line, path, range)| {
                        let (weak, origin, path, range) = (weak.clone(), origin.clone(), path.clone(), *range);
                        Entry::Item(Item::new(label.clone()).description(line.clone()).on_click(move |_, cx| {
                            weak.update(cx, |s, cx| s.jump(id, Some(origin.clone()), path.clone(), range, cx)).ok();
                        }))
                    }));
                    entries
                });
                let search = (n > 8).then_some("Filter…");
                self.open_popover(LOCATIONS, point, gpui::Corner::TopLeft, 460., search, build, window, cx);
            }
        }
    }

    /// Open `path` at `range`, remembering where we came from, and pick the
    /// symbol landed on.
    fn jump(&mut self, id: u64, origin: Option<Spot>, path: PathBuf, range: LspRange, cx: &mut Context<Self>) {
        if let Some(origin) = origin {
            if self.servers.back.last() != Some(&origin) {
                self.servers.back.push(origin);
            }
            self.servers.forward.clear();
        }
        // The same file through another path (a symlink) is the tab already open.
        let real = std::fs::canonicalize(&path).ok();
        let path = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::File(p) | TabKind::Diff(p) | TabKind::GitDiff(p, _) => Some(p),
                _ => None,
            })
            .find(|p| **p != path && real.is_some() && std::fs::canonicalize(p).ok() == real)
            .cloned()
            .unwrap_or(path);
        self.servers.adopted.entry(path.clone()).or_insert(id);
        let line = range.start.line as usize;
        self.open_at(path.clone(), Some((line + 1, line + 1)), cx);
        self.servers.at = Some(Spot { tab: TabKind::File(path.clone()), row: Some(line) });
        let encoding = self.servers.all.get(&id).map_or(Encoding::Utf16, |s| s.encoding);
        let Some(Ok(file)) = self.files.get(&path) else { return };
        let Some(shown) = position::shown(&file.text, range.start, syntax::tab_width(), encoding) else { return };
        let Some(word) = file.lines.get(line).and_then(|l| word_at(l, shown).map(|r| (r.start, l[r].to_string()))) else { return };
        let point = self.symbol.as_ref().map_or_else(Point::default, |m| m.point);
        self.servers.ticket += 1;
        self.symbol = Some(SymbolMark { source: LineSource::File(path), word: word.1.into(), row: line, at: word.0, point, server: None, id: self.servers.ticket });
        self.request_highlights(cx);
    }

    // ── Back and forward ────────────────────────────────────────────────────

    /// Where the view is now, to come back to.
    fn here(&self) -> Option<Spot> {
        let tab = self.tabs.get(self.active)?.kind.clone();
        if let Some(at) = self.servers.at.as_ref().filter(|a| a.tab == tab) {
            return Some(at.clone());
        }
        let row = match &tab {
            // The row in the middle of the view, since going there centres it.
            TabKind::File(path) => self.file_scroll.get(path).map(|s| {
                let state = s.0.borrow();
                let item = state.last_item_size.map_or(px(20.), |i| i.item.height).max(px(1.));
                let handle = &state.base_handle;
                ((-handle.offset().y + handle.bounds().size.height / 2.) / item).max(0.) as usize
            }),
            _ => None,
        };
        Some(Spot { tab, row })
    }

    fn go_to(&mut self, spot: Spot, cx: &mut Context<Self>) {
        match &spot.tab {
            TabKind::File(path) => self.open_at(path.clone(), spot.row.map(|r| (r + 1, r + 1)), cx),
            tab => self.open(tab.clone(), cx),
        }
        self.servers.at = Some(spot);
    }

    pub(super) fn go_back(&mut self, cx: &mut Context<Self>) {
        let Some(spot) = self.servers.back.pop() else { return };
        if let Some(here) = self.here() {
            self.servers.forward.push(here);
        }
        self.go_to(spot, cx);
    }

    pub(super) fn go_forward(&mut self, cx: &mut Context<Self>) {
        let Some(spot) = self.servers.forward.pop() else { return };
        if let Some(here) = self.here() {
            self.servers.back.push(here);
        }
        self.go_to(spot, cx);
    }

    // ── Floating bits ───────────────────────────────────────────────────────

    /// A short note by the pointer that fades on its own.
    fn notice(&mut self, text: impl Into<SharedString>, point: Point<Pixels>, cx: &mut Context<Self>) {
        self.servers.ticket += 1;
        let ticket = self.servers.ticket;
        self.servers.notice = Some((text.into(), point, ticket));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1800)).await;
            this.update(cx, |s, cx| {
                if s.servers.notice.as_ref().is_some_and(|n| n.2 == ticket) {
                    s.servers.notice = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The hover card and the notice, floating over everything.
    pub(super) fn lsp_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = palette(cx);
        let card = self.servers.hover.as_ref().and_then(|h| Some((h.text.clone()?, h.point)));
        let notice = self.servers.notice.as_ref().map(|(text, point, _)| (text.clone(), *point));
        if card.is_none() && notice.is_none() {
            return None;
        }
        let float = |point: Point<Pixels>, child: AnyElement| {
            deferred(anchored().position(point + gpui::point(px(0.), px(18.))).snap_to_window_with_margin(px(8.)).child(child)).with_priority(1)
        };
        let surface = || div().bg(rgb(colors.popover)).border_1().border_color(rgb(colors.popover_border)).rounded(px(8.)).shadow_lg();
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_0()
                .when_some(card, |s, (text, point)| {
                    // Pieces stacked, a rule between each: signatures and paths
                    // highlighted as code, docs as Markdown.
                    let mut body = super::ui::col();
                    for (n, part) in hover_parts(&text).into_iter().enumerate() {
                        if n > 0 {
                            body = body.child(div().h(px(1.)).w_full().bg(rgb(colors.popover_border)));
                        }
                        body = body.child(match part {
                            HoverPart::Code { lang, text } => {
                                let marks = syntax::cached(if lang.is_empty() { "text" } else { &lang }, &text);
                                let lines = syntax::lines(&text);
                                super::ui::col()
                                    .px_3()
                                    .py_2()
                                    .font_family(mono())
                                    .text_size(px(crate::theme::mono_size()))
                                    .text_color(rgb(colors.code))
                                    .children(lines.into_iter().enumerate().map(|(i, line)| {
                                        let styles = marks.as_ref().and_then(|m| m.line(colors.dark, i)).cloned().unwrap_or_default();
                                        div().child(StyledText::new(line).with_highlights(styles))
                                    }))
                                    .into_any_element()
                            }
                            HoverPart::Text(text) => div()
                                .px_3()
                                .py_2()
                                .child(super::md::render(format!("lsp-hover-{n}").into(), &text, super::md::Breaks::Soft, colors, mono(), px(crate::theme::mono_size()), None, None))
                                .into_any_element(),
                        });
                    }
                    s.child(float(point, surface().id("lsp-hover").max_w(px(440.)).max_h(px(360.)).overflow_y_scroll().child(body).into_any_element()))
                })
                .when_some(notice, |s, (text, point)| {
                    s.child(float(point, surface().px_2().py_1().text_size(px(crate::theme::scaled_ui(12.))).text_color(rgb(colors.text)).child(text).into_any_element()))
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
    use super::{HoverPart, hover_parts};

    #[test]
    fn cuts_a_hover_into_stacked_pieces() {
        // rust-analyzer's hover for a function, as it sends it.
        let hover = "\n```rust\ndemo::util\n```\n\n```rust\npub fn twice(x: i32) -> i32\n```\n\n---\n\nDoubles a number.\n\nSee `half`.";
        assert_eq!(
            hover_parts(hover),
            vec![
                HoverPart::Code { lang: "rust".into(), text: "demo::util".into() },
                HoverPart::Code { lang: "rust".into(), text: "pub fn twice(x: i32) -> i32".into() },
                HoverPart::Text("Doubles a number.\n\nSee `half`.".into()),
            ]
        );
        // A rule between prose splits it; empty fences and blank prose vanish.
        assert_eq!(hover_parts("a\n---\nb\n```\n```\n  \n"), vec![HoverPart::Text("a".into()), HoverPart::Text("b".into())]);
    }

    #[test]
    fn signatures_get_syntax_colours() {
        let marks = super::super::syntax::cached("rust", "pub fn twice(x: i32) -> i32").expect("rust is bundled");
        assert!(marks.line(true, 0).is_some_and(|l| !l.is_empty()));
    }
}
