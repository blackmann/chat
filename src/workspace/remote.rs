//! Remote access from paired phones: who's let in, what they're shown, and what
//! they can do. The transport is `crate::remote`; this is the Mac's side of the
//! conversation, and the devices popover on the rail.
//!
//! A phone sees projects, threads and transcripts, sends messages, stops turns
//! and answers permission requests. It can't change a thread's mode, model or
//! effort: escalating what an agent may do stays on the Mac.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use crate::model::Project;
use crate::remote::{ConnId, Device, Incoming, Saved, Server};
use gpui_component::{
    Sizable,
    button::{Button, ButtonVariants},
    switch::Switch,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::hash::{Hash, Hasher};

/// How often git is read again for a folder a phone is looking at.
const GIT_EVERY: Duration = Duration::from_secs(3);
/// A pairing QR code stays valid this long.
const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
/// How often phones are sent what changed.
const PUSH_EVERY: Duration = Duration::from_millis(400);
/// The most transcript blocks sent at once, from the end.
const MAX_BLOCKS: usize = 300;
/// The most of a tool's output sent, from its end.
const MAX_OUTPUT: usize = 3000;
/// The longest message a phone may send.
const MAX_TEXT: usize = 100_000;
const WIDTH: f32 = 300.;
/// The popover's anchor on the rail.
pub(super) const ANCHOR: &str = "remote";

pub(super) struct RemoteState {
    saved: Saved,
    server: Option<Server>,
    conns: HashMap<ConnId, Conn>,
    pairing: Option<Pairing>,
    pub(super) open: bool,
    error: Option<String>,
    /// The key is being read (perhaps behind a Keychain prompt); the server starts after.
    starting: bool,
    /// "Pair device…" was chosen before the server was up.
    pair_when_ready: bool,
    mac_name: String,
    _listen: Option<Task<()>>,
    _pump: Option<Task<()>>,
    /// Git's view of folders phones are looking at, read in the background
    /// every so often while they look: when, and what it said.
    git: HashMap<PathBuf, (Instant, serde_json::Value)>,
    git_reading: HashSet<PathBuf>,
    /// The paths each message names, by a hash of its text, so transcripts
    /// aren't parsed again on every push.
    mentions: RefCell<HashMap<u64, Rc<Vec<String>>>>,
    /// Everything sent, for tests to read.
    #[cfg(test)]
    pub(super) log: RefCell<Vec<(ConnId, String)>>,
}

struct Conn {
    device: String,
    /// Let in: a paired device, or one just accepted. Until then nothing it asks is done.
    authorized: bool,
    direct: bool,
    /// What it's looking at: a project's threads, a thread's transcript.
    project: Option<String>,
    thread: Option<usize>,
    /// A hash of what was last sent of each kind, so only changes go out.
    /// Projects, threads, the transcript, and its folders' git changes.
    sent: [Option<u64>; 4],
}

struct Pairing {
    secret: String,
    started: Instant,
    /// The QR code's modules, row by row, and the side's length.
    qr: Rc<(Vec<bool>, usize)>,
    request: Option<PairRequest>,
}

struct PairRequest {
    conn: ConnId,
    device: String,
    name: String,
    code: String,
}

impl RemoteState {
    #[cfg(test)]
    pub(super) fn mac_id(&self) -> Option<String> {
        self.server.as_ref().map(|s| s.id().to_string())
    }
    #[cfg(test)]
    pub(super) fn pairing_secret(&self) -> Option<String> {
        self.pairing.as_ref().map(|p| p.secret.clone())
    }
    #[cfg(test)]
    pub(super) fn devices(&self) -> Vec<String> {
        self.saved.devices.iter().map(|d| d.id.clone()).collect()
    }
    pub(super) fn new() -> Self {
        Self {
            saved: Saved::load(),
            server: None,
            conns: HashMap::new(),
            pairing: None,
            open: false,
            error: None,
            starting: false,
            pair_when_ready: false,
            mac_name: crate::remote::mac_name(),
            _listen: None,
            _pump: None,
            git: HashMap::new(),
            git_reading: HashSet::new(),
            mentions: RefCell::default(),
            #[cfg(test)]
            log: RefCell::default(),
        }
    }
    fn out(&self, conn: ConnId, text: String) {
        #[cfg(test)]
        self.log.borrow_mut().push((conn, text.clone()));
        if let Some(server) = &self.server {
            server.send(conn, text);
        }
    }
    /// A paired phone is connected.
    pub(super) fn connected(&self) -> bool {
        self.conns.values().any(|c| c.authorized)
    }
}

/// Where a path a message names points on this Mac, when it's a picture or a
/// video: `file://`, `~/` and absolute paths as they are, relative ones in the
/// first of the thread's folders that has them.
fn media_path(written: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    let written = written.trim_start_matches('<').trim_end_matches('>');
    if written.contains("://") && !written.starts_with("file://") {
        return None;
    }
    let written = written.strip_prefix("file://").map(|w| w.replace("%20", " ")).unwrap_or_else(|| written.to_string());
    let path = PathBuf::from(&written);
    super::media::kind(&path)?;
    if let Some(rest) = written.strip_prefix("~/") {
        return Some(std::env::home_dir()?.join(rest));
    }
    if path.is_absolute() {
        return Some(path);
    }
    let candidates: Vec<PathBuf> = folders.iter().map(|f| f.join(&path)).collect();
    candidates.iter().find(|c| c.is_file()).or(candidates.first()).cloned()
}

/// What a phone asks for a file by: the same for the same file until it
/// changes, so the phone can keep what it fetched. Only files shared with a
/// connection can be fetched by it, so the token needn't be secret.
fn media_token(path: &Path, meta: Option<&std::fs::Metadata>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(path.as_os_str().as_encoded_bytes());
    if let Some(m) = meta {
        h.update(m.len().to_le_bytes());
        if let Ok(at) = m.modified().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(std::io::Error::other)) {
            h.update(at.as_nanos().to_le_bytes());
        }
    }
    hex::encode(&h.finalize()[..16])
}

/// What a phone asks for, once it's in.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    View {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        thread: Option<usize>,
    },
    Send {
        thread: usize,
        text: String,
    },
    NewThread {
        project: String,
        text: String,
    },
    Stop {
        thread: usize,
    },
    Answer {
        thread: usize,
        permission: u64,
        answer: Option<String>,
    },
    /// Answers to the agent's questions: choices by field, what's typed by
    /// field; or, with `skip`, none.
    Reply {
        thread: usize,
        ask: u64,
        #[serde(default)]
        picks: std::collections::BTreeMap<String, Vec<String>>,
        #[serde(default)]
        typed: HashMap<String, String>,
        #[serde(default)]
        skip: bool,
    },
    Ping,
}

#[derive(Serialize)]
struct ProjectInfo {
    key: String,
    name: String,
    lane: usize,
    waiting: usize,
    running: usize,
    unread: usize,
    latest: Option<String>,
    active_at: Option<i64>,
}

#[derive(Serialize)]
struct ThreadInfo {
    id: usize,
    title: String,
    state: &'static str,
    unread: bool,
    waiting: bool,
    problem: Option<String>,
    active_at: Option<i64>,
}

fn state_name(state: ThreadState) -> &'static str {
    match state {
        ThreadState::Idle => "idle",
        ThreadState::Running => "running",
        ThreadState::Waiting => "waiting",
        ThreadState::Done => "done",
        ThreadState::Failed => "failed",
    }
}

/// How a phone names a project: its folder, or `chats` for the folderless one.
fn project_key(project: &Project) -> String {
    project.path.as_ref().map_or_else(|| "chats".into(), |p| p.display().to_string())
}

/// The end of `text`, at most `max` bytes, cut on a character boundary.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn ago(ms: i64) -> String {
    let secs = (chrono::Utc::now().timestamp_millis() - ms).max(0) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => match secs / 86400 {
            1 => "yesterday".into(),
            d => format!("{d} days ago"),
        },
    }
}

impl Workspace {
    /// Pick up where it was left: on if it was on.
    pub(super) fn remote_init(&mut self, cx: &mut Context<Self>) {
        if self.remote.saved.enabled {
            self.remote_start(cx);
        }
    }

    /// Read the key off the main thread, since the Keychain may ask first, then start.
    fn remote_start(&mut self, cx: &mut Context<Self>) {
        if self.remote.server.is_some() || self.remote.starting {
            return;
        }
        self.remote.starting = true;
        self.remote.error = None;
        let in_file = self.remote.saved.key.clone();
        self.remote._listen = Some(cx.spawn(async move |this, cx| {
            let (key, source) = cx.background_executor().spawn(async move { crate::remote::load_key(in_file.as_deref()) }).await;
            let _ = this.update(cx, |s, cx| s.remote_serve(key, source, cx));
        }));
    }

    fn remote_serve(&mut self, key: [u8; 32], source: crate::remote::KeySource, cx: &mut Context<Self>) {
        self.remote.starting = false;
        if !self.remote.saved.enabled {
            return;
        }
        // Kept where it was read from: out of the file once it's in the Keychain.
        let in_file = match source {
            crate::remote::KeySource::Keychain => None,
            crate::remote::KeySource::File => Some(hex::encode(key)),
        };
        if self.remote.saved.key != in_file {
            self.remote.saved.key = in_file;
            let _ = self.remote.saved.save();
        }
        let (server, mut incoming) = Server::start(key);
        self.remote.server = Some(server);
        self.remote._listen = Some(cx.spawn(async move |this, cx| {
            while let Some(message) = incoming.next().await {
                if this.update(cx, |s, cx| s.remote_incoming(message, cx)).is_err() {
                    break;
                }
            }
        }));
        self.remote._pump = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PUSH_EVERY).await;
                if this.update(cx, |s, cx| s.remote_tick(cx)).is_err() {
                    break;
                }
            }
        }));
        if std::mem::take(&mut self.remote.pair_when_ready) {
            self.start_pairing(cx);
        }
        cx.notify();
    }

    fn remote_stop(&mut self) {
        self.remote.server = None;
        self.remote.starting = false;
        self.remote.pair_when_ready = false;
        self.remote.conns.clear();
        self.remote.pairing = None;
        self.remote._listen = None;
        self.remote._pump = None;
    }

    pub(super) fn set_remote_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        self.remote.saved.enabled = on;
        let _ = self.remote.saved.save();
        if on {
            self.remote_start(cx);
        } else {
            self.remote_stop();
        }
        cx.notify();
    }

    fn remote_send(&self, conn: ConnId, value: serde_json::Value) {
        self.remote.out(conn, value.to_string());
    }

    pub(super) fn remote_incoming(&mut self, message: Incoming, cx: &mut Context<Self>) {
        match message {
            Incoming::Failed(error) => {
                self.remote.error = Some(format!("Couldn't start remote access: {error}"));
                self.remote_stop();
            }
            Incoming::Hello { conn, device, name, secret } => self.remote_hello(conn, device, name, secret, cx),
            Incoming::Line { conn, line } => {
                if self.remote.conns.get(&conn).is_some_and(|c| c.authorized) {
                    match serde_json::from_str::<Request>(&line) {
                        Ok(request) => self.remote_request(conn, request, cx),
                        Err(_) => self.remote_send(conn, json!({"type": "error", "message": "Unknown request"})),
                    }
                }
            }
            Incoming::Path { conn, direct } => {
                if let Some(c) = self.remote.conns.get_mut(&conn) {
                    c.direct = direct;
                }
                self.remote_send(conn, json!({"type": "path", "direct": direct}));
            }
            Incoming::Closed { conn } => {
                self.remote.conns.remove(&conn);
                if let Some(p) = &mut self.remote.pairing
                    && p.request.as_ref().is_some_and(|r| r.conn == conn)
                {
                    p.request = None;
                }
            }
        }
        cx.notify();
    }

    /// A phone said who it is: a paired one is let in, one holding the
    /// pairing secret waits for the Mac to accept it, anyone else is turned away.
    fn remote_hello(&mut self, conn: ConnId, device: String, name: String, secret: Option<String>, cx: &mut Context<Self>) {
        let Some(mac) = self.remote.server.as_ref().map(|s| s.id().to_string()) else { return };
        let fresh = || Conn { device: device.clone(), authorized: false, direct: false, project: None, thread: None, sent: [None; 4] };
        if let Some(known) = self.remote.saved.devices.iter_mut().find(|d| d.id == device) {
            known.last_seen = Some(chrono::Utc::now().timestamp_millis());
            if !name.is_empty() {
                known.name = name;
            }
            let _ = self.remote.saved.save();
            self.remote.conns.insert(conn, Conn { authorized: true, ..fresh() });
            self.remote_send(conn, json!({"type": "welcome", "mac": self.remote.mac_name}));
            self.remote_tick(cx);
            return;
        }
        let pairing = self.remote.pairing.as_mut().filter(|p| p.started.elapsed() < PAIRING_TTL && p.request.is_none());
        if let (Some(pairing), Some(secret)) = (pairing, secret)
            && crate::remote::same_secret(&secret, &pairing.secret)
        {
            let code = crate::remote::pair_code(&mac, &device, &pairing.secret);
            pairing.request = Some(PairRequest {
                conn,
                device: device.clone(),
                name: if name.is_empty() { "Phone".into() } else { name },
                code: code.clone(),
            });
            self.remote.conns.insert(conn, fresh());
            self.remote_send(conn, json!({"type": "pairing", "code": code, "mac": self.remote.mac_name}));
            self.remote.open = true;
            cx.activate(true);
            return;
        }
        self.remote_send(conn, json!({"type": "rejected", "reason": "This phone isn't paired with this Mac."}));
        if let Some(server) = &self.remote.server {
            server.close(conn);
        }
    }

    pub(super) fn start_pairing(&mut self, cx: &mut Context<Self>) {
        if !self.remote.saved.enabled {
            self.set_remote_enabled(true, cx);
        }
        let Some(mac) = self.remote.server.as_ref().map(|s| s.id().to_string()) else {
            // Still reading the key: pair once it's up.
            self.remote.pair_when_ready = true;
            cx.notify();
            return;
        };
        let secret = crate::remote::new_secret();
        let url = crate::remote::pair_url(&mac, &secret, &self.remote.mac_name);
        let qr = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M)
            .map(|code| {
                let width = code.width();
                (code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect(), width)
            })
            .unwrap_or_default();
        self.remote.pairing = Some(Pairing { secret, started: Instant::now(), qr: Rc::new(qr), request: None });
        cx.notify();
    }

    pub(super) fn end_pairing(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(request) = self.remote.pairing.take().and_then(|p| p.request) else {
            cx.notify();
            return;
        };
        if accept {
            let now = chrono::Utc::now().timestamp_millis();
            self.remote.saved.devices.retain(|d| d.id != request.device);
            self.remote.saved.devices.push(Device { id: request.device, name: request.name, paired_at: now, last_seen: Some(now) });
            let _ = self.remote.saved.save();
            if let Some(c) = self.remote.conns.get_mut(&request.conn) {
                c.authorized = true;
            }
            self.remote_send(request.conn, json!({"type": "paired", "mac": self.remote.mac_name}));
            self.remote_tick(cx);
        } else {
            self.remote_send(request.conn, json!({"type": "rejected", "reason": "Pairing was declined on the Mac."}));
            if let Some(server) = &self.remote.server {
                server.close(request.conn);
            }
        }
        cx.notify();
    }

    pub(super) fn remove_device(&mut self, id: &str, cx: &mut Context<Self>) {
        self.remote.saved.devices.retain(|d| d.id != id);
        let _ = self.remote.saved.save();
        let gone: Vec<ConnId> = self.remote.conns.iter().filter(|(_, c)| c.device == id).map(|(k, _)| *k).collect();
        for conn in gone {
            self.remote.conns.remove(&conn);
            self.remote_send(conn, json!({"type": "rejected", "reason": "This phone was removed on the Mac."}));
            if let Some(server) = &self.remote.server {
                server.close(conn);
            }
        }
        cx.notify();
    }

    /// Where a thread is, by id: its project and its index there.
    fn locate(&self, id: usize) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(p, project)| project.threads.iter().position(|t| t.id == id).map(|t| (p, t)))
    }

    fn load_thread(&mut self, p: usize, i: usize) {
        if let Some(store) = &self.store {
            store.load_blocks(&mut self.projects[p].threads[i]);
        }
    }

    fn remote_request(&mut self, conn: ConnId, request: Request, cx: &mut Context<Self>) {
        match request {
            Request::Ping => self.remote_send(conn, json!({"type": "pong"})),
            Request::View { project, thread } => {
                if let Some((p, i)) = thread.and_then(|t| self.locate(t)) {
                    self.load_thread(p, i);
                }
                if let Some(c) = self.remote.conns.get_mut(&conn) {
                    c.project = project;
                    c.thread = thread;
                    c.sent[1] = None;
                    c.sent[2] = None;
                    c.sent[3] = None;
                }
                self.remote_tick(cx);
            }
            Request::Send { thread, text } => {
                if let Some((p, i)) = self.locate(thread) {
                    self.remote_submit(p, i, text, cx);
                }
            }
            Request::NewThread { project, text } => {
                let Some(p) = self.projects.iter().position(|pr| project_key(pr) == project) else {
                    return self.remote_send(conn, json!({"type": "error", "message": "That project isn't open on the Mac."}));
                };
                self.next_thread += 1;
                let agent = self.settings.agent;
                let mut thread = Thread::new(self.next_thread, agent);
                thread.picks = self.starting_picks(p, agent);
                let id = thread.id;
                // At the end, so no index into the list moves; lists sort by activity.
                self.projects[p].threads.push(thread);
                let i = self.projects[p].threads.len() - 1;
                self.remote_submit(p, i, text, cx);
                self.remote_send(conn, json!({"type": "created", "thread": id}));
            }
            Request::Stop { thread } => {
                if let Some((p, i)) = self.locate(thread) {
                    self.projects[p].threads[i].send(Command::Cancel);
                }
            }
            Request::Answer { thread, permission, answer } => {
                if let Some((p, i)) = self.locate(thread) {
                    let t = &mut self.projects[p].threads[i];
                    let mut found = None;
                    for block in &mut t.blocks {
                        if let Block::Permission(perm) = block
                            && perm.id == permission
                            && perm.chosen.is_none()
                        {
                            // Only an answer the agent offered.
                            let picked = answer.as_ref().and_then(|a| perm.answers.iter().find(|x| &x.id == a));
                            if answer.is_some() && picked.is_none() {
                                break;
                            }
                            perm.chosen = Some(picked.map_or("Dismissed".into(), |a| a.name.clone()));
                            found = Some(picked.map(|a| a.id.clone()));
                        }
                    }
                    if let Some(answer) = found {
                        t.send(Command::Answer { id: permission, answer });
                        if !t.awaiting_user() && t.state == ThreadState::Waiting {
                            t.state = ThreadState::Running;
                        }
                        self.save_soon(cx);
                    }
                }
            }
            Request::Reply { thread, ask, picks, typed, skip } => {
                if let Some((p, i)) = self.locate(thread)
                    && let Err(message) = self.remote_reply(p, i, ask, picks, typed, skip, cx)
                {
                    self.remote_send(conn, json!({"type": "ask_error", "thread": thread, "ask": ask, "message": message}));
                }
            }
        }
        cx.notify();
    }

    /// Answer a thread's questions from a phone, checked as the Mac's card
    /// checks them. Only choices the agent offered are taken.
    fn remote_reply(
        &mut self,
        p: usize,
        i: usize,
        ask: u64,
        picks: std::collections::BTreeMap<String, Vec<String>>,
        typed: HashMap<String, String>,
        skip: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        use crate::agent::QuestionKind;
        let t = &mut self.projects[p].threads[i];
        let id = t.id;
        let Some(a) = t.blocks.iter_mut().find_map(|b| match b {
            Block::Ask(a) if a.id == ask && a.outcome.is_none() => Some(a),
            _ => None,
        }) else {
            return Err("Those questions were already answered.".into());
        };
        let replies = if skip {
            None
        } else {
            let mut kept = std::collections::BTreeMap::new();
            for q in &a.questions {
                let given = picks.get(&q.field).cloned().unwrap_or_default();
                let valid: Vec<String> = match &q.kind {
                    QuestionKind::Single(c) => given.into_iter().filter(|v| c.iter().any(|c| &c.value == v)).take(1).collect(),
                    QuestionKind::Multi(c) => given.into_iter().filter(|v| c.iter().any(|c| &c.value == v)).collect(),
                    QuestionKind::Yes => given.into_iter().filter(|v| v == "true" || v == "false").take(1).collect(),
                    _ => Vec::new(),
                };
                if !valid.is_empty() {
                    kept.insert(q.field.clone(), valid);
                }
            }
            a.picks = kept;
            let typed = |field: &str| typed.get(field).map(|t| t.chars().take(MAX_TEXT).collect()).unwrap_or_default();
            Some(a.replies(typed)?)
        };
        a.outcome = Some(match &replies {
            Some(replies) => crate::model::AskOutcome::Answered(a.shown(replies)),
            None => crate::model::AskOutcome::Skipped,
        });
        t.send(Command::Reply { id: ask, replies });
        if !t.awaiting_user() && t.state == ThreadState::Waiting {
            t.state = ThreadState::Running;
        }
        // The Mac's card for it is done too.
        self.ask_errors.remove(&(id, ask));
        self.ask_inputs.retain(|(t, a, _), _| !(*t == id && *a == ask));
        self.save_soon(cx);
        Ok(())
    }

    /// Send a phone's message to a thread, as the composer would.
    fn remote_submit(&mut self, p: usize, i: usize, text: String, cx: &mut Context<Self>) {
        let text: String = text.trim().chars().take(MAX_TEXT).collect();
        if text.is_empty() {
            return;
        }
        self.load_thread(p, i);
        if self.projects[p].threads[i].session.is_none() {
            self.connect(p, i, cx);
        }
        let steer = self.settings.follow_up_mode == FollowUpMode::Steer;
        let t = &mut self.projects[p].threads[i];
        if t.blocks.is_empty() {
            t.title = text.lines().next().unwrap_or_default().chars().take(80).collect::<String>().into();
        }
        t.submit(text.clone(), text, steer);
        self.save_soon(cx);
    }

    /// Send each phone what changed in what it's looking at; expire a stale pairing.
    pub(super) fn remote_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = &self.remote.pairing {
            if p.request.is_none() && p.started.elapsed() >= PAIRING_TTL {
                self.remote.pairing = None;
            }
            // The countdown.
            cx.notify();
        }
        if !self.remote.connected() {
            return;
        }
        let projects = json!({"type": "projects", "mac": self.remote.mac_name, "projects": self.project_infos()}).to_string();
        let conns: Vec<(ConnId, Option<String>, Option<usize>, [Option<u64>; 4])> = self
            .remote
            .conns
            .iter()
            .filter(|(_, c)| c.authorized)
            .map(|(k, c)| (*k, c.project.clone(), c.thread, c.sent))
            .collect();
        for (conn, project, thread, sent) in conns {
            let mut out = [None, None, None, None];
            out[0] = Some(projects.clone());
            if let Some(key) = &project
                && let Some(pr) = self.projects.iter().find(|p| &project_key(p) == key)
            {
                out[1] = Some(json!({"type": "threads", "project": key, "threads": thread_infos(pr)}).to_string());
            }
            if let Some(id) = thread
                && let Some((p, i)) = self.locate(id)
            {
                let mut media = Vec::new();
                out[2] = Some(json!({"type": "thread", "thread": self.thread_detail(p, i, &mut media)}).to_string());
                if let Some(server) = &self.remote.server {
                    server.allow(conn, media);
                }
                let folders = self.thread_folders(p, i);
                self.read_git_soon(&folders, cx);
                let repos: Vec<serde_json::Value> = folders.iter().filter_map(|f| self.remote.git.get(f).map(|(_, v)| v.clone())).collect();
                out[3] = Some(json!({"type": "changes", "thread": id, "repos": repos, "loading": repos.len() < folders.len()}).to_string());
            }
            let mut sent = sent;
            for (k, text) in out.into_iter().enumerate() {
                let Some(text) = text else { continue };
                let h = hash(&text);
                if sent[k] != Some(h) {
                    sent[k] = Some(h);
                    self.remote.out(conn, text);
                }
            }
            if let Some(c) = self.remote.conns.get_mut(&conn) {
                c.sent = sent;
            }
        }
    }

    fn project_infos(&self) -> Vec<ProjectInfo> {
        self.projects
            .iter()
            .enumerate()
            .map(|(lane, p)| {
                let shown = p.threads.iter().filter(|t| !t.is_blank());
                let latest = shown.clone().max_by_key(|t| t.active_at.or(t.created_at).unwrap_or(0));
                ProjectInfo {
                    key: project_key(p),
                    name: p.name.clone(),
                    lane,
                    waiting: shown.clone().filter(|t| t.awaiting_user()).count(),
                    running: shown.clone().filter(|t| t.state.busy() && !t.awaiting_user()).count(),
                    unread: shown.clone().filter(|t| t.unread).count(),
                    latest: latest.map(|t| t.title.to_string()),
                    active_at: latest.and_then(|t| t.active_at.or(t.created_at)),
                }
            })
            .collect()
    }

    /// The pictures and videos a message points at that are on this Mac, as
    /// the phone is told of them, each added to `shared` by its token.
    fn message_media(&self, text: &str, folders: &[PathBuf], shared: &mut Vec<(String, PathBuf)>) -> Vec<serde_json::Value> {
        if text.is_empty() {
            return Vec::new();
        }
        let named = {
            let key = hash(text);
            let mut cache = self.remote.mentions.borrow_mut();
            if cache.len() > 4000 {
                cache.clear();
            }
            cache.entry(key).or_insert_with(|| Rc::new(super::md::mentioned_paths(text))).clone()
        };
        named
            .iter()
            .filter_map(|written| media_path(written, folders))
            .map(|path| {
                let kind = match super::media::kind(&path) {
                    Some(super::media::Kind::Video) => "video",
                    _ => "image",
                };
                let meta = std::fs::metadata(&path).ok();
                let token = media_token(&path, meta.as_ref());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let exists = meta.as_ref().is_some_and(|m| m.is_file());
                if exists {
                    shared.push((token.clone(), path));
                }
                json!({"ref": token, "name": name, "kind": kind, "size": meta.map(|m| m.len()), "exists": exists})
            })
            .collect()
    }

    fn thread_detail(&self, p: usize, i: usize, shared: &mut Vec<(String, PathBuf)>) -> serde_json::Value {
        let project = &self.projects[p];
        let t = &project.threads[i];
        let folders = self.thread_folders(p, i);
        let with_media = |mut block: serde_json::Value, text: &str, shared: &mut Vec<(String, PathBuf)>| {
            let media = self.message_media(text, &folders, shared);
            if !media.is_empty() {
                block["media"] = media.into();
            }
            block
        };
        let settings: Vec<String> = [Role::Model, Role::Effort, Role::Mode]
            .iter()
            .filter_map(|role| t.settings.iter().find(|s| s.role == *role))
            .map(|s| s.current_name().to_string())
            .collect();
        let tool = |id: &str| {
            t.blocks.iter().find_map(|b| match b {
                Block::Tool(call) if call.id == id => Some(call),
                _ => None,
            })
        };
        let start = t.blocks.len().saturating_sub(MAX_BLOCKS);
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        for (n, b) in t.blocks[start..].iter().enumerate() {
            let index = start + n;
            blocks.push(match b {
                Block::User(m) => with_media(json!({"kind": "user", "index": index, "text": m.text.to_string(), "at": m.at}), &m.text, shared),
                Block::Text(text) => with_media(json!({"kind": "text", "index": index, "text": text}), text, shared),
                Block::Thought(_) => continue,
                Block::Tool(call) => {
                    let (adds, dels) = call.diffs.iter().fold((0, 0), |(a, d), diff| {
                        let (x, y) = crate::model::count(diff.old.as_deref().unwrap_or(""), &diff.new);
                        (a + x, d + y)
                    });
                    json!({
                        "kind": "tool", "index": index, "title": call.title, "tool": call.kind, "status": call.status,
                        "output": tail(&call.output, MAX_OUTPUT),
                        "files": call.diffs.iter().map(|d| project.relative(&d.path).display().to_string()).collect::<Vec<_>>(),
                        "adds": adds, "dels": dels,
                    })
                }
                Block::Permission(perm) => json!({
                    "kind": "permission", "index": index, "id": perm.id, "title": perm.title,
                    "detail": tool(&perm.tool).map(|c| c.title.clone()),
                    "tool": tool(&perm.tool).map(|c| c.kind),
                    "answers": perm.answers.iter().map(|a| json!({"id": a.id, "name": a.name, "allow": a.allow})).collect::<Vec<_>>(),
                    "chosen": perm.chosen,
                }),
                Block::Ask(ask) => ask_json(index, ask),
                Block::Notice(text) => json!({"kind": "notice", "index": index, "text": text}),
                Block::SignIn { agent, .. } => json!({"kind": "notice", "index": index, "text": format!("{agent} needs you to sign in on the Mac.")}),
                Block::TurnEnded { at, worked_ms } => json!({"kind": "ended", "index": index, "at": at, "worked_ms": worked_ms}),
            });
        }
        json!({
            "id": t.id,
            "project": project_key(project),
            "project_name": project.name,
            "cwd": project.path.as_deref().map(tilde),
            "title": t.title.to_string(),
            "state": state_name(t.state),
            "agent": self.settings.agents.get(t.agent).map(|a| a.name.clone()),
            "settings": settings,
            "model": t.settings.iter().find(|s| s.role == Role::Model).map(|s| s.current_name().to_string()),
            "effort": t.settings.iter().find(|s| s.role == Role::Effort).map(|s| s.current_name().to_string()),
            "mode": t.settings.iter().find(|s| s.role == Role::Mode).map(|s| s.current_name().to_string()),
            "queued": t.queued.len(),
            "problem": t.problem,
            "truncated": start > 0,
            "blocks": blocks,
            "context": self.thread_context(p, i),
        })
    }

    /// What the Mac's Context page shows: the context window, the plan, the
    /// files the agent touched, and the instruction files agents load.
    fn thread_context(&self, p: usize, i: usize) -> serde_json::Value {
        use crate::agent::Step;
        use crate::model::Touch;
        let project = &self.projects[p];
        let t = &project.threads[i];
        let changes = t.changes();
        let files: Vec<serde_json::Value> = t
            .files()
            .into_iter()
            .take(100)
            .map(|(path, touch)| {
                let change = changes.iter().find(|c| c.path == path);
                json!({
                    "path": project.relative(&path).display().to_string(),
                    "touch": match touch { Touch::Read => "read", Touch::Edited => "edited", Touch::Created => "created" },
                    "adds": change.map(|c| c.adds),
                    "dels": change.map(|c| c.dels),
                })
            })
            .collect();
        let instructions: Vec<serde_json::Value> = super::instruction_files(project.path.as_deref())
            .into_iter()
            .map(|(path, scope)| {
                let shown = project.path.as_deref().and_then(|root| path.strip_prefix(root).ok()).map_or_else(|| tilde(&path), |rel| rel.display().to_string());
                json!({"path": shown, "scope": scope})
            })
            .collect();
        json!({
            "used": t.usage.map(|(used, _)| used),
            "size": t.usage.map(|(_, size)| size),
            "cost": t.cost.as_ref().map(|(amount, _)| amount),
            "currency": t.cost.as_ref().map(|(_, currency)| currency),
            "plan": t.plan.iter().map(|(step, text)| json!({
                "step": match step { Step::Done => "done", Step::Doing => "doing", Step::Todo => "todo" },
                "text": text,
            })).collect::<Vec<_>>(),
            "files": files,
            "instructions": instructions,
        })
    }

    /// The folders a thread works in: each of its project's, or the worktree
    /// the agent said it uses for one.
    fn thread_folders(&self, p: usize, i: usize) -> Vec<PathBuf> {
        let t = &self.projects[p].threads[i];
        self.projects[p].roots().into_iter().map(|root| t.worktrees.get(&root).cloned().unwrap_or(root)).collect()
    }

    /// Read git's view of these folders in the background, unless a fresh one is at hand.
    fn read_git_soon(&mut self, folders: &[PathBuf], cx: &mut Context<Self>) {
        for folder in folders {
            let fresh = self.remote.git.get(folder).is_some_and(|(at, _)| at.elapsed() < GIT_EVERY);
            if fresh || !self.remote.git_reading.insert(folder.clone()) {
                continue;
            }
            let folder = folder.clone();
            cx.spawn(async move |this, cx| {
                let read = folder.clone();
                let snapshot = cx.background_executor().spawn(async move { crate::git::snapshot(&read) }).await;
                let _ = this.update(cx, |s, _| {
                    s.remote.git_reading.remove(&folder);
                    s.remote.git.insert(folder.clone(), (Instant::now(), repo_json(&folder, snapshot)));
                });
            })
            .detach();
        }
    }

    // ── The popover ───────────────────────────────────────────────────────

    pub(super) fn toggle_remote(&mut self, cx: &mut Context<Self>) {
        if self.dismissed.take().is_some_and(|(a, at)| a == ANCHOR && at.elapsed() < Duration::from_millis(400)) {
            return;
        }
        self.remote.open = !self.remote.open;
        cx.notify();
    }

    pub(super) fn close_remote(&mut self, cx: &mut Context<Self>) {
        if self.remote.open {
            self.remote.open = false;
            // A pairing nobody asked to join yet ends with the popover.
            if self.remote.pairing.as_ref().is_some_and(|p| p.request.is_none()) {
                self.remote.pairing = None;
            }
            cx.notify();
        }
    }

    /// The phone button above Settings on the rail.
    pub(super) fn remote_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let lit = self.remote.open || self.remote.connected();
        self.anchor(
            ANCHOR,
            Button::new("open-remote")
                .icon(icon(Glyph::Smartphone).text_color(rgb(if lit { colors.blue } else { colors.muted })))
                .ghost()
                .small()
                .when(!self.remote.open, |b| b.tooltip("Remote devices"))
                .on_click(cx.listener(|s, _, _, cx| s.toggle_remote(cx))),
        )
        .into_any_element()
    }

    pub(super) fn remote_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let small = px(crate::theme::scaled_ui(11.));
        let body = match &self.remote.pairing {
            Some(pairing) => self.pairing_view(pairing, cx),
            None if self.remote.pair_when_ready => div()
                .px(px(8.))
                .pb(px(10.))
                .text_size(small)
                .text_color(rgb(palette(cx).muted))
                .child("Getting ready to pair…"),
            None => self.devices_view(cx),
        };
        col()
            .id("remote-popover")
            .debug_selector(|| "remote-popover".into())
            .occlude()
            .w(px(WIDTH))
            .p(px(6.))
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .text_size(px(crate::theme::scaled_ui(13.)))
            .text_color(rgb(colors.text))
            .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                if s.remote.open {
                    s.dismissed = Some((ANCHOR, Instant::now()));
                    s.close_remote(cx);
                }
            }))
            .child(
                row()
                    .justify_between()
                    .px(px(8.))
                    .pt(px(6.))
                    .pb(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Remote access"))
                    .child(Switch::new("remote-enabled").checked(self.remote.saved.enabled).on_click(cx.listener(|s, on: &bool, _, cx| s.set_remote_enabled(*on, cx)))),
            )
            .when_some(self.remote.error.clone(), |c, e| c.child(div().px(px(8.)).pb(px(6.)).text_size(small).text_color(rgb(colors.red)).child(e)))
            .child(body)
            .into_any_element()
    }

    fn devices_view(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let small = px(crate::theme::scaled_ui(11.5));
        let mut list = col();
        if !self.remote.saved.enabled {
            list = list.child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(small)
                    .text_color(rgb(colors.muted))
                    .child("Off. Paired phones can't reach this Mac until you turn it on."),
            );
        }
        for (n, device) in self.remote.saved.devices.iter().enumerate() {
            let conn = self.remote.conns.values().find(|c| c.authorized && c.device == device.id);
            let status: SharedString = match conn {
                Some(c) => format!("Connected · {}", if c.direct { "direct" } else { "relayed" }).into(),
                None => device.last_seen.map_or("Never connected".into(), |at| format!("Last seen {}", ago(at))).into(),
            };
            let id = device.id.clone();
            let group = SharedString::from(format!("device-{n}"));
            list = list.child(
                row()
                    .id(("device", n))
                    .group(group.clone())
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(rgb(colors.popover_hover)))
                    .child(icon(Glyph::Smartphone).text_color(rgb(colors.muted)))
                    .child(
                        col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(device.name.clone()))
                            .child(
                                row()
                                    .gap(px(5.))
                                    .text_size(small)
                                    .text_color(rgb(colors.muted))
                                    .when(conn.is_some(), |r| r.child(div().size(px(6.)).rounded_full().bg(rgb(colors.green))))
                                    .child(status),
                            ),
                    )
                    .child(
                        div().invisible().group_hover(group, |s| s.visible()).child(
                            Button::new(("remove-device", n))
                                .ghost()
                                .xsmall()
                                .label("Remove")
                                .text_color(rgb(colors.red))
                                .on_click(cx.listener(move |s, _, _, cx| s.remove_device(&id, cx))),
                        ),
                    ),
            );
        }
        if self.remote.saved.devices.is_empty() {
            list = list.child(div().px(px(8.)).py(px(6.)).text_color(rgb(colors.muted)).child("No phones paired yet."));
        }
        list.child(div().my(px(6.)).mx(px(-6.)).h(px(1.)).bg(rgb(colors.popover_border)))
            .child(
                row()
                    .id("pair-device")
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(rgb(colors.blue))
                    .hover(|s| s.bg(rgb(colors.popover_hover)))
                    .child(icon(gpui_component::IconName::Plus))
                    .child("Pair device…")
                    .on_click(cx.listener(|s, _, _, cx| s.start_pairing(cx))),
            )
            .child(
                div()
                    .px(px(8.))
                    .pt(px(2.))
                    .pb(px(4.))
                    .text_size(px(crate::theme::scaled_ui(11.)))
                    .text_color(rgb(colors.quiet))
                    .child("Paired devices can run agents on this Mac."),
            )
    }

    fn pairing_view(&self, pairing: &Pairing, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let small = px(crate::theme::scaled_ui(11.5));
        if let Some(request) = &pairing.request {
            let code = format!("{} {}", &request.code[..3], &request.code[3..]);
            return col()
                .items_center()
                .px(px(8.))
                .pb(px(8.))
                .gap(px(6.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("“{}” wants to pair", request.name)))
                .child(div().text_size(small).text_color(rgb(colors.muted)).child("Accept only if your phone shows the same code."))
                .child(div().font_family(mono()).text_size(px(22.)).py(px(6.)).child(code))
                .child(
                    row()
                        .gap(px(8.))
                        .child(Button::new("pair-reject").small().label("Reject").on_click(cx.listener(|s, _, _, cx| s.end_pairing(false, cx))))
                        .child(Button::new("pair-accept").small().primary().label("Accept").on_click(cx.listener(|s, _, _, cx| s.end_pairing(true, cx)))),
                );
        }
        let left = PAIRING_TTL.saturating_sub(pairing.started.elapsed()).as_secs();
        let qr = pairing.qr.clone();
        col()
            .items_center()
            .px(px(8.))
            .pb(px(8.))
            .gap(px(4.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Scan with Chat on your phone"))
            .child(
                div().my(px(8.)).p(px(10.)).bg(rgb(0xffffff)).rounded(px(8.)).child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let (modules, n) = (&qr.0, qr.1);
                            if n == 0 {
                                return;
                            }
                            let cell = bounds.size.width / n as f32;
                            for (k, dark) in modules.iter().enumerate() {
                                if *dark {
                                    let (x, y) = ((k % n) as f32, (k / n) as f32);
                                    let origin = bounds.origin + point(cell * x, cell * y);
                                    // A hair over a cell, so neighbours meet without seams.
                                    window.paint_quad(fill(Bounds::new(origin, size(cell + px(0.5), cell + px(0.5))), rgb(0x111111)));
                                }
                            }
                        },
                    )
                    .size(px(170.)),
                ),
            )
            .child(div().text_size(small).text_color(rgb(colors.muted)).child("Then check the code on your phone matches here."))
            .child(
                row()
                    .gap(px(4.))
                    .text_size(small)
                    .text_color(rgb(colors.quiet))
                    .child(format!("Expires in {}:{:02} ·", left / 60, left % 60))
                    .child(
                        div()
                            .id("cancel-pairing")
                            .cursor_pointer()
                            .text_color(rgb(colors.blue))
                            .child("Cancel")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.remote.pairing = None;
                                cx.notify();
                            })),
                    ),
            )
    }
}

/// A set of the agent's questions, with what each offers and, once done, the answers.
fn ask_json(index: usize, ask: &crate::model::Ask) -> serde_json::Value {
    use crate::agent::QuestionKind;
    use crate::model::AskOutcome;
    let questions: Vec<serde_json::Value> = ask
        .questions
        .iter()
        .map(|q| {
            let (kind, choices, integer) = match &q.kind {
                QuestionKind::Single(c) => ("single", Some(c), false),
                QuestionKind::Multi(c) => ("multi", Some(c), false),
                QuestionKind::Text => ("text", None, false),
                QuestionKind::Yes => ("yes", None, false),
                QuestionKind::Number { integer } => ("number", None, *integer),
            };
            json!({
                "field": q.field, "title": q.title, "prompt": q.prompt, "kind": kind,
                "integer": integer, "required": q.required,
                "choices": choices.map(|c| c.iter().map(|c| json!({"value": c.value, "label": c.label, "description": c.description})).collect::<Vec<_>>()),
            })
        })
        .collect();
    let (answered, skipped) = match &ask.outcome {
        Some(AskOutcome::Answered(pairs)) => (Some(pairs.iter().map(|(q, a)| vec![q.clone(), a.clone()]).collect::<Vec<_>>()), false),
        Some(AskOutcome::Skipped) | Some(AskOutcome::Dismissed) => (None, true),
        None => (None, false),
    };
    json!({
        "kind": "ask", "index": index, "id": ask.id, "text": ask.message, "open": ask.outcome.is_none(),
        "questions": questions, "answered": answered, "skipped": skipped,
    })
}

/// One folder's git state for a phone: branch, how far ahead, and its staged
/// and unstaged files with their status letter and line counts.
fn repo_json(folder: &std::path::Path, snapshot: Result<crate::git::Snapshot, String>) -> serde_json::Value {
    let name = crate::model::folder_name(folder);
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(error) => return json!({"name": name, "error": error}),
    };
    let side = |staged: bool| -> Vec<serde_json::Value> {
        snapshot
            .entries
            .iter()
            .filter(|e| if staged { e.staged() } else { e.unstaged() })
            .take(300)
            .map(|e| {
                let stat = e.stat(staged);
                json!({
                    "path": e.path.display().to_string(),
                    "status": (e.status(staged) as char).to_string(),
                    "adds": stat.map(|(a, _)| a),
                    "dels": stat.map(|(_, d)| d),
                })
            })
            .collect()
    };
    json!({
        "name": name,
        "branch": snapshot.branch,
        "upstream": snapshot.upstream,
        "ahead": snapshot.ahead,
        "staged": side(true),
        "unstaged": side(false),
    })
}

fn thread_infos(project: &Project) -> Vec<ThreadInfo> {
    let mut threads: Vec<ThreadInfo> = project
        .threads
        .iter()
        .filter(|t| !t.is_blank())
        .map(|t| ThreadInfo {
            id: t.id,
            title: t.title.to_string(),
            state: state_name(t.state),
            unread: t.unread,
            waiting: t.awaiting_user(),
            problem: t.problem.clone(),
            active_at: t.active_at.or(t.created_at),
        })
        .collect();
    threads.sort_by(|a, b| b.active_at.cmp(&a.active_at));
    threads
}

#[cfg(test)]
mod tests {
    use super::{media_path, media_token};
    use std::path::PathBuf;

    #[test]
    fn media_paths_resolve_in_the_threads_folders() {
        let base = std::env::temp_dir().join(format!("chat-media-paths-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(b.join("out")).unwrap();
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(b.join("out/demo.webm"), b"x").unwrap();
        let folders = [a.clone(), b.clone()];
        assert_eq!(media_path("out/demo.webm", &folders), Some(b.join("out/demo.webm")), "the folder that has it");
        assert_eq!(media_path("out/gone.mp4", &folders), Some(a.join("out/gone.mp4")), "else the first, to say it's gone");
        assert_eq!(media_path("file:///tmp/My%20Shot.png", &folders), Some(PathBuf::from("/tmp/My Shot.png")));
        assert_eq!(media_path("<~/Movies/a.mov>", &folders), std::env::home_dir().map(|h| h.join("Movies/a.mov")));
        assert_eq!(media_path("src/main.rs", &folders), None, "not media");
        assert_eq!(media_path("https://x.dev/a.png", &folders), None, "not on this Mac");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn tokens_change_with_the_file() {
        let path = std::env::temp_dir().join(format!("chat-media-token-{}.png", std::process::id()));
        std::fs::write(&path, b"one").unwrap();
        let first = media_token(&path, std::fs::metadata(&path).ok().as_ref());
        assert_eq!(first, media_token(&path, std::fs::metadata(&path).ok().as_ref()), "stable");
        std::fs::write(&path, b"three").unwrap();
        assert_ne!(first, media_token(&path, std::fs::metadata(&path).ok().as_ref()), "a new version, a new token");
        std::fs::remove_file(&path).ok();
    }
}
