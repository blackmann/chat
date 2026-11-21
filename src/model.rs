//! Projects, threads and what a thread has gathered from its agent. A thread is
//! a fold over the agent's events; every panel reads from it.
use crate::agent::{Answer, Command, Question, QuestionKind, Reply, Event, Failure, FileDiff, History, Kind, Setting, Status, SteerOutcome, Step, Stop};
use serde::{Deserialize, Serialize};
use futures::channel::mpsc::UnboundedSender;
use gpui::{SharedString, Task};
use std::{collections::VecDeque, path::{Path, PathBuf}};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadState {
    /// Nothing asked yet, or the last turn was stopped.
    Idle,
    Running,
    /// The agent is waiting on a permission answer or on its questions.
    Waiting,
    Done,
    Failed,
}
impl ThreadState {
    pub fn busy(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }
}

#[derive(Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub status: Status,
    pub diffs: Vec<FileDiff>,
    pub output: String,
    /// Files the call read or touched.
    #[serde(default)]
    pub locations: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
pub struct Permission {
    pub id: u64,
    pub title: String,
    pub answers: Vec<Answer>,
    /// The answer given, by name; `None` while the agent waits.
    pub chosen: Option<String>,
}

/// Questions the agent asked, with what's been picked so far.
#[derive(Serialize, Deserialize)]
pub struct Ask {
    pub id: u64,
    pub message: String,
    pub questions: Vec<Question>,
    /// What's picked so far, by field; text fields are read from their inputs.
    pub picks: std::collections::BTreeMap<String, Vec<String>>,
    /// `None` while the agent waits.
    pub outcome: Option<AskOutcome>,
}

#[derive(Serialize, Deserialize)]
pub enum AskOutcome {
    /// Each answered question, by its label, and the answer as shown.
    Answered(Vec<(String, String)>),
    Skipped,
    Dismissed,
}

impl Ask {
    /// How a question is named in its answer: its label, else its prompt, else its key.
    pub fn label(question: &Question) -> String {
        question.title.clone().or_else(|| question.prompt.clone()).unwrap_or_else(|| question.field.clone())
    }
    /// Tap a choice: picks it for a one-of question (tapping it again clears
    /// it), toggles it for a many-of one.
    pub fn toggle(&mut self, field: &str, value: &str) {
        let Some(question) = self.questions.iter().find(|q| q.field == field) else { return };
        let picks = self.picks.entry(field.to_string()).or_default();
        let had = picks.iter().any(|p| p == value);
        match question.kind {
            QuestionKind::Multi(_) => {
                if had {
                    picks.retain(|p| p != value);
                } else {
                    picks.push(value.to_string());
                }
            }
            _ => {
                picks.clear();
                if !had {
                    picks.push(value.to_string());
                }
            }
        }
    }
    /// The replies to send, given what's typed in each text field; `Err`
    /// names a required question left blank or a number that isn't one.
    pub fn replies(&self, typed: impl Fn(&str) -> String) -> Result<Vec<(String, Reply)>, String> {
        let mut replies = Vec::new();
        for q in &self.questions {
            let picks = self.picks.get(&q.field).cloned().unwrap_or_default();
            let reply = match &q.kind {
                QuestionKind::Single(_) => picks.into_iter().next().map(Reply::Text),
                QuestionKind::Multi(_) => (!picks.is_empty()).then_some(Reply::Picks(picks)),
                QuestionKind::Yes => picks.first().map(|p| Reply::Yes(p == "true")),
                QuestionKind::Text => Some(typed(&q.field).trim().to_string()).filter(|t| !t.is_empty()).map(Reply::Text),
                QuestionKind::Number { integer } => {
                    let text = typed(&q.field).trim().to_string();
                    if text.is_empty() {
                        None
                    } else if *integer {
                        Some(Reply::Integer(text.parse().map_err(|_| format!("{} needs a whole number", Self::label(q)))?))
                    } else {
                        Some(Reply::Number(text.parse().map_err(|_| format!("{} needs a number", Self::label(q)))?))
                    }
                }
            };
            match reply {
                Some(reply) => replies.push((q.field.clone(), reply)),
                None if q.required => return Err(format!("{} needs an answer", Self::label(q))),
                None => {}
            }
        }
        Ok(replies)
    }
    /// The answers as they read in the transcript once sent.
    pub fn shown(&self, replies: &[(String, Reply)]) -> Vec<(String, String)> {
        replies
            .iter()
            .filter_map(|(field, reply)| {
                let q = self.questions.iter().find(|q| &q.field == field)?;
                let name = |v: &String| match &q.kind {
                    QuestionKind::Single(c) | QuestionKind::Multi(c) => c.iter().find(|c| &c.value == v).map_or(v.clone(), |c| c.label.clone()),
                    _ => v.clone(),
                };
                let text = match reply {
                    Reply::Text(t) => name(t),
                    Reply::Picks(p) => p.iter().map(name).collect::<Vec<_>>().join(", "),
                    Reply::Yes(y) => if *y { "Yes" } else { "No" }.into(),
                    Reply::Integer(n) => n.to_string(),
                    Reply::Number(n) => n.to_string(),
                };
                Some((Self::label(q), text))
            })
            .collect()
    }
}

/// Milliseconds since the Unix epoch, by the wall clock.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What the user sent, and when. Threads saved before messages carried a time
/// stored just the text, so either form reads back.
#[derive(Serialize, Deserialize)]
#[serde(from = "SavedMessage")]
pub struct Message {
    pub text: SharedString,
    /// `None` for messages from before times were kept.
    pub at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedMessage {
    Text(SharedString),
    Timed { text: SharedString, at: Option<i64> },
}

impl From<SavedMessage> for Message {
    fn from(saved: SavedMessage) -> Self {
        match saved {
            SavedMessage::Text(text) => Self { text, at: None },
            SavedMessage::Timed { text, at } => Self { text, at },
        }
    }
}

impl Message {
    pub fn now(text: impl Into<SharedString>) -> Self {
        Self { text: text.into(), at: Some(now_ms()) }
    }
}

/// Notices that mark a turn cut off before the agent finished.
const STOPPED: &str = "Stopped";
const INTERRUPTED: &str = "Interrupted when Chat closed.";

#[derive(Serialize, Deserialize)]
pub enum Block {
    User(Message),
    /// Agent prose, grown chunk by chunk.
    Text(String),
    Thought(String),
    Tool(ToolCall),
    Permission(Permission),
    Ask(Ask),
    /// Something the app has to say: a stop, a failure.
    Notice(String),
    /// The agent isn't signed in, with the command that signs it in when there is one.
    SignIn { agent: String, command: Option<String> },
    /// The close of an agent turn: when it ended and how long the agent worked.
    TurnEnded { at: i64, worked_ms: u64 },
}

/// The live connection behind a thread. Dropping it stops the agent process.
pub struct Session {
    pub commands: UnboundedSender<Command>,
    pub _driver: Task<()>,
    pub _listener: Task<()>,
}

pub struct Thread {
    pub id: usize,
    pub title: SharedString,
    pub state: ThreadState,
    /// Finished while not being looked at, and not opened since.
    pub unread: bool,
    pub blocks: Vec<Block>,
    /// Index into the configured agents.
    pub agent: usize,
    pub session: Option<Session>,
    /// The session has opened; until then prompts wait in the channel.
    pub ready: bool,
    pub supports_steering: bool,
    /// Model, effort, mode: whatever the agent lets the user change mid-thread.
    pub settings: Vec<Setting>,
    pub plan: Vec<(Step, String)>,
    /// Context window use in tokens: used, size.
    pub usage: Option<(u64, u64)>,
    /// What the session has cost so far, and in what currency, when the agent says.
    pub cost: Option<(f64, String)>,
    /// The slash commands the agent offers: name and description.
    pub commands: Vec<(String, String)>,
    /// Why the connection is down, when it wasn't closed on purpose. Kept apart
    /// from the transcript: it is about the session, not the conversation.
    pub problem: Option<String>,
    /// The agent's id for this thread's session, to reopen it after a restart.
    pub session_id: Option<String>,
    /// Setting choices (id, value) to put back once a reopened session is ready,
    /// so a thread keeps the model it was switched to.
    pub picks: Vec<(String, String)>,
    /// Worktrees the agent said it works in, by the project folder each belongs to.
    pub worktrees: std::collections::BTreeMap<PathBuf, PathBuf>,
    /// What each setting was when the session opened: the agent's defaults.
    pub defaults: Vec<(String, String)>,
    /// Index of the current turn's first block. Steering adds user blocks
    /// inside the turn, so the last user block no longer marks its start.
    turn_start: usize,
    /// When the running turn began, to tell how long it worked once it ends.
    /// Saved, so a turn cut off by quitting can still say how long it ran.
    pub turn_began: Option<i64>,
    /// When the first message was sent, in ms since the epoch. Saved, so the
    /// list can sort without reading the transcript.
    pub created_at: Option<i64>,
    /// When a message was last sent or a turn last ended. Saved, like `created_at`.
    pub active_at: Option<i64>,
    /// Its row in the database, once saved.
    pub stored: Option<i64>,
    /// Saved, but its blocks not read back yet: they're read when it's opened.
    pub unloaded: bool,
    /// A hash of each block as last saved, so a save writes only what changed.
    pub saved: Vec<u64>,
    /// One steer awaits a reply before the next follow-up can be dispatched.
    pending_steer: Option<(usize, QueuedMessage)>,
    pub queued: VecDeque<QueuedMessage>,
}

pub struct QueuedMessage {
    pub shown: SharedString,
    prompt: String,
    steer: bool,
}

/// A file's net change across a thread: its text before the first edit and after the last.
pub struct Change {
    pub path: PathBuf,
    pub old: Option<String>,
    pub new: String,
    pub adds: usize,
    pub dels: usize,
}

impl Thread {
    pub fn new(id: usize, agent: usize) -> Self {
        Self {
            id,
            title: "New thread".into(),
            state: ThreadState::Idle,
            unread: false,
            blocks: Vec::new(),
            agent,
            session: None,
            ready: false,
            supports_steering: false,
            settings: Vec::new(),
            plan: Vec::new(),
            usage: None,
            cost: None,
            commands: Vec::new(),
            problem: None,
            session_id: None,
            picks: Vec::new(),
            worktrees: Default::default(),
            defaults: Vec::new(),
            turn_start: 0,
            turn_began: None,
            created_at: None,
            active_at: None,
            stored: None,
            unloaded: false,
            saved: Vec::new(),
            pending_steer: None,
            queued: VecDeque::new(),
        }
    }
    pub fn send(&self, command: Command) {
        if let Some(session) = &self.session {
            let _ = session.commands.unbounded_send(command);
        }
    }
    /// Nothing said in it yet: a new thread. One saved but not read back
    /// has blocks, they just aren't here.
    pub fn is_blank(&self) -> bool {
        !self.unloaded && self.blocks.is_empty()
    }
    pub fn notice(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Notice(text.into()));
    }
    fn prompt(&mut self, shown: impl Into<SharedString>, prompt: String) {
        self.turn_start = self.blocks.len();
        self.blocks.push(Block::User(Message::now(shown)));
        self.turn_began = Some(now_ms());
        self.mark_active(now_ms());
        self.state = ThreadState::Running;
        self.send(Command::Prompt(prompt));
    }

    pub fn is_active(&self) -> bool {
        self.state.busy() || self.pending_steer.is_some()
    }

    /// The last turn was cut off, by quitting or by a stop, and nothing has
    /// been said since: the thread can be picked up where it left off.
    pub fn interrupted(&self) -> bool {
        !self.is_active()
            && self.queued.is_empty()
            && self
                .blocks
                .iter()
                .rev()
                .find(|b| !matches!(b, Block::TurnEnded { .. }))
                .is_some_and(|b| matches!(b, Block::Notice(n) if n == STOPPED || n == INTERRUPTED))
    }

    pub fn submit(&mut self, shown: impl Into<SharedString>, prompt: String, steer: bool) {
        self.queued.push_back(QueuedMessage { shown: shown.into(), prompt, steer });
        self.advance_follow_ups();
    }

    fn advance_follow_ups(&mut self) {
        if self.pending_steer.is_some() {
            return;
        }
        let busy = self.state.busy();
        let can_steer = self.ready && self.supports_steering
            && self.queued.front().is_some_and(|message| message.steer);
        if busy && !can_steer {
            return;
        }
        let Some(message) = self.queued.pop_front() else { return };
        if busy {
            let index = self.blocks.len();
            self.blocks.push(Block::User(Message::now(message.shown.clone())));
            self.mark_active(now_ms());
            self.send(Command::Steer(message.prompt.clone()));
            self.pending_steer = Some((index, message));
        } else {
            self.prompt(message.shown, message.prompt);
        }
    }
    /// Unanswered permission requests can't be answered once a turn is over.
    fn dismiss_permissions(&mut self) {
        for block in &mut self.blocks {
            if let Block::Permission(p) = block
                && p.chosen.is_none()
            {
                p.chosen = Some("Dismissed".into());
            }
            if let Block::Ask(a) = block
                && a.outcome.is_none()
            {
                a.outcome = Some(AskOutcome::Dismissed);
            }
        }
    }
    /// Whether a permission or a question still waits on the user.
    pub fn awaiting_user(&self) -> bool {
        self.blocks.iter().any(|b| match b {
            Block::Permission(p) => p.chosen.is_none(),
            Block::Ask(a) => a.outcome.is_none(),
            _ => false,
        })
    }
    /// Close the running turn with when it ended and how long it took.
    fn stamp_turn(&mut self) {
        self.stamp_turn_at(now_ms());
    }
    fn stamp_turn_at(&mut self, at: i64) {
        if let Some(began) = self.turn_began.take() {
            self.blocks.push(Block::TurnEnded { at, worked_ms: (at - began).max(0) as u64 });
            self.mark_active(at);
        }
    }
    /// A turn that ends leaves no tool call running.
    fn settle_tools(&mut self) {
        for block in &mut self.blocks {
            if let Block::Tool(t) = block
                && matches!(t.status, Status::Pending | Status::Running)
            {
                t.status = Status::Failed;
            }
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Ready { settings, session, history, supports_steering, .. } => {
                self.ready = true;
                self.supports_steering = supports_steering;
                self.defaults = settings.iter().map(|s| (s.id.clone(), s.current.clone())).collect();
                self.settings = settings;
                self.session_id = Some(session);
                if history == History::Lost && !self.blocks.is_empty() {
                    self.notice("The agent couldn't reopen this thread's earlier session, so it doesn't remember the turns above.");
                }
                // Put back what the thread was switched to before it was closed.
                for (id, value) in std::mem::take(&mut self.picks) {
                    let Some(setting) = self.settings.iter_mut().find(|s| s.id == id) else { continue };
                    if setting.current != value && setting.choices.iter().any(|c| c.id == value) {
                        setting.current = value.clone();
                        if let Some(session) = &self.session {
                            let _ = session.commands.unbounded_send(Command::Set { setting: id, choice: value });
                        }
                    }
                }
                if self.state.busy() {
                    self.advance_follow_ups();
                }
            }
            Event::Text(text) => match self.blocks.last_mut() {
                Some(Block::Text(existing)) => existing.push_str(&text),
                _ => self.blocks.push(Block::Text(text)),
            },
            Event::Thought(text) => match self.blocks.last_mut() {
                Some(Block::Thought(existing)) => existing.push_str(&text),
                _ => self.blocks.push(Block::Thought(text)),
            },
            Event::Tool { id, patch } => {
                // Ids are only unique within a session, and a restored thread may
                // be on a new one; only this turn's calls can be the same call.
                let existing = self.blocks[self.turn_start..]
                    .iter_mut()
                    .rev()
                    .find_map(|b| match b {
                        Block::Tool(t) if t.id == id => Some(t),
                        _ => None,
                    });
                let tool = match existing {
                    Some(tool) => tool,
                    None => {
                        self.blocks.push(Block::Tool(ToolCall {
                            id,
                            title: String::new(),
                            kind: Kind::Other,
                            status: Status::Pending,
                            diffs: Vec::new(),
                            output: String::new(),
                            locations: Vec::new(),
                        }));
                        let Some(Block::Tool(tool)) = self.blocks.last_mut() else { unreachable!() };
                        tool
                    }
                };
                if let Some(title) = patch.title {
                    tool.title = title;
                }
                if let Some(kind) = patch.kind {
                    tool.kind = kind;
                }
                if let Some(status) = patch.status {
                    tool.status = status;
                }
                if let Some(diffs) = patch.diffs {
                    tool.diffs = diffs;
                }
                if let Some(output) = patch.output {
                    tool.output = output;
                }
                if let Some(locations) = patch.locations {
                    tool.locations = locations;
                }
            }
            Event::Plan(plan) => self.plan = plan,
            Event::Settings(settings) => self.settings = settings,
            Event::Usage { used, size, cost } => {
                self.usage = Some((used, size));
                if cost.is_some() {
                    self.cost = cost;
                }
            }
            // Limits are the account's, not the thread's: the workspace keeps them.
            Event::RateLimit(_) => {}
            Event::Commands(commands) => self.commands = commands,
            Event::Title(title) => {
                if !title.trim().is_empty() {
                    self.title = title.into();
                }
            }
            Event::Permission { id, title, answers, .. } => {
                self.blocks.push(Block::Permission(Permission { id, title, answers, chosen: None }));
                self.state = ThreadState::Waiting;
            }
            Event::Ask { id, message, questions } => {
                self.blocks.push(Block::Ask(Ask { id, message, questions, picks: Default::default(), outcome: None }));
                self.state = ThreadState::Waiting;
            }
            Event::TurnEnded(result) => {
                self.dismiss_permissions();
                self.state = match result {
                    Ok(Stop::Finished) => ThreadState::Done,
                    Ok(Stop::Cancelled) => {
                        self.settle_tools();
                        self.notice(STOPPED);
                        ThreadState::Idle
                    }
                    Ok(Stop::Refused) => {
                        self.notice("The agent declined to continue.");
                        ThreadState::Done
                    }
                    Ok(Stop::Limit) => {
                        self.notice("The agent hit its token or turn limit.");
                        ThreadState::Done
                    }
                    Err(failure) => {
                        self.settle_tools();
                        match failure {
                            Failure::SignedOut { agent, command } => self.blocks.push(Block::SignIn { agent, command }),
                            Failure::Other(error) => self.notice(error),
                        }
                        ThreadState::Failed
                    }
                };
                self.stamp_turn();
                self.advance_follow_ups();
            }
            Event::SteerEnded(result) => {
                let Some((index, mut message)) = self.pending_steer.take() else { return };
                match result {
                    Ok(SteerOutcome::Injected) => {}
                    Ok(SteerOutcome::PromptRequired) => {
                        self.blocks.remove(index);
                        message.steer = false;
                        self.queued.push_front(message);
                    }
                    Ok(SteerOutcome::StartedNewTurn) => {
                        self.notice("The adapter started a separate turn for this message.");
                    }
                    Ok(SteerOutcome::Failed) => self.notice("The agent could not apply the steering message."),
                    Err(error) => self.notice(format!("Steering failed: {error}")),
                }
                self.advance_follow_ups();
            }
            Event::Closed(reason) => {
                let interrupted = self.is_active() || !self.ready;
                self.session = None;
                self.ready = false;
                self.supports_steering = false;
                self.pending_steer = None;
                self.dismiss_permissions();
                self.settle_tools();
                self.stamp_turn();
                if reason.is_some() || interrupted {
                    match reason {
                        // How to sign in stays in the transcript; it outlasts the reconnect that clears a problem.
                        Some(Failure::SignedOut { agent, command }) => self.blocks.push(Block::SignIn { agent, command }),
                        Some(Failure::Other(reason)) => self.problem = Some(format!("The agent stopped: {reason}")),
                        None => self.problem = Some("The agent exited.".into()),
                    }
                    self.state = ThreadState::Failed;
                }
            }
        }
    }

    /// The choices to save: the live ones, or those still waiting to be put back.
    pub fn current_picks(&self) -> Vec<(String, String)> {
        if self.settings.is_empty() {
            return self.picks.clone();
        }
        self.settings.iter().map(|s| (s.id.clone(), s.current.clone())).collect()
    }
    /// Something happened at `at`: the thread's first time is when it began,
    /// its latest when it was last active.
    pub fn mark_active(&mut self, at: i64) {
        self.created_at.get_or_insert(at);
        self.active_at = Some(self.active_at.map_or(at, |a| a.max(at)));
    }

    /// When the thread began, in ms since the epoch: its first message sent.
    /// `None` for a thread with no times to go on.
    pub fn started(&self) -> Option<i64> {
        self.created_at
    }

    /// When anything last happened here, in ms since the epoch: now while a turn
    /// runs, else the latest message sent or turn ended. `None` for a thread
    /// with no times to go on.
    pub fn last_active(&self) -> Option<i64> {
        if self.state.busy() {
            return Some(now_ms());
        }
        self.active_at
    }

    /// Bring a thread read from disk to rest: nothing can still be running in a
    /// process that is gone. `saved_at` is when it was last written, about when
    /// the agent last did anything, so a cut-off turn's work ends there.
    pub fn settle(&mut self, saved_at: Option<i64>) {
        if self.state.busy() {
            self.dismiss_permissions();
            self.settle_tools();
            self.notice(INTERRUPTED);
            match saved_at {
                Some(at) => self.stamp_turn_at(at),
                None => self.turn_began = None,
            }
            self.state = ThreadState::Idle;
        }
        self.turn_began = None;
        self.turn_start = self.blocks.len();
    }

    /// Every file the agent's finished edits touched, in the order first touched.
    pub fn changes(&self) -> Vec<Change> {
        let mut changes: Vec<Change> = Vec::new();
        let done = self.blocks.iter().filter_map(|b| match b {
            Block::Tool(t) if t.status == Status::Done => Some(&t.diffs),
            _ => None,
        });
        for diff in done.flatten() {
            match changes.iter_mut().find(|c| c.path == diff.path) {
                Some(change) => change.new = diff.new.clone(),
                None => changes.push(Change {
                    path: diff.path.clone(),
                    old: diff.old.clone(),
                    new: diff.new.clone(),
                    adds: 0,
                    dels: 0,
                }),
            }
        }
        for change in &mut changes {
            (change.adds, change.dels) = count(change.old.as_deref().unwrap_or(""), &change.new);
        }
        changes
    }
}

/// How a thread's agent has used a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Read,
    Edited,
    Created,
}

impl Thread {
    /// The files the agent has read or changed in this thread, most recent first.
    /// It is what the agent has looked at, as far as it reports, not a token count.
    pub fn files(&self) -> Vec<(PathBuf, Touch)> {
        let mut seen: Vec<(PathBuf, Touch)> = Vec::new();
        let tools = self.blocks.iter().rev().filter_map(|b| match b {
            Block::Tool(t) if t.status != Status::Failed => Some(t),
            _ => None,
        });
        for tool in tools {
            let edited = tool.diffs.iter().map(|d| (d.path.clone(), if d.old.is_none() { Touch::Created } else { Touch::Edited }));
            let read = tool.locations.iter().map(|p| (p.clone(), if tool.kind == Kind::Edit { Touch::Edited } else { Touch::Read }));
            for (path, touch) in edited.chain(read) {
                match seen.iter_mut().find(|(p, _)| *p == path) {
                    // A file that was ever changed reads as changed.
                    Some(entry) => {
                        if entry.1 == Touch::Read {
                            entry.1 = touch;
                        }
                    }
                    None => seen.push((path, touch)),
                }
            }
        }
        seen
    }
}

/// Lines added and removed between two texts.
pub fn count(old: &str, new: &str) -> (usize, usize) {
    use similar::ChangeTag;
    similar::TextDiff::from_lines(old, new).iter_all_changes().fold((0, 0), |(adds, dels), change| match change.tag() {
        ChangeTag::Insert => (adds + 1, dels),
        ChangeTag::Delete => (adds, dels + 1),
        ChangeTag::Equal => (adds, dels),
    })
}

pub struct Project {
    pub name: String,
    /// `None` for regular chats, which aren't tied to a folder.
    pub path: Option<PathBuf>,
    /// Further folders the project spans, beside its main one.
    pub folders: Vec<PathBuf>,
    pub threads: Vec<Thread>,
}
impl Project {
    pub fn chats() -> Self {
        Self { name: "Chats".into(), path: None, folders: Vec::new(), threads: Vec::new() }
    }
    pub fn open(entry: crate::settings::ProjectEntry) -> Self {
        Self {
            name: entry.name.unwrap_or_else(|| folder_name(&entry.path)),
            path: Some(entry.path),
            folders: entry.folders,
            threads: Vec::new(),
        }
    }
    /// How the project is saved; `None` for chats.
    pub fn entry(&self) -> Option<crate::settings::ProjectEntry> {
        let path = self.path.clone()?;
        let name = (self.name != folder_name(&path)).then(|| self.name.clone());
        Some(crate::settings::ProjectEntry { path, name, folders: self.folders.clone() })
    }
    /// Every folder the project spans, the main one first.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.path.iter().chain(&self.folders).cloned().collect()
    }
    /// A path as the project sees it: relative to the root it is under, where it is
    /// under one. Paths in an added folder keep that folder's name in front.
    pub fn relative<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, Path> {
        if let Some(rest) = self.path.as_deref().and_then(|root| path.strip_prefix(root).ok()) {
            return rest.into();
        }
        for folder in &self.folders {
            if let Ok(rest) = path.strip_prefix(folder) {
                return Path::new(&folder_name(folder)).join(rest).into();
            }
        }
        path.into()
    }
}

/// A folder's last component, or the whole path when it has none.
pub fn folder_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// `~/…` for paths under the home directory.
pub fn tilde(path: &Path) -> String {
    match std::env::var_os("HOME").and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolPatch;

    #[test]
    fn last_active_is_the_latest_time_or_now_while_busy() {
        let mut t = Thread::new(1, 0);
        assert_eq!(t.last_active(), None);
        t.mark_active(1_000);
        t.mark_active(5_000);
        t.mark_active(3_000);
        assert_eq!(t.started(), Some(1_000));
        assert_eq!(t.last_active(), Some(5_000));
        t.state = ThreadState::Running;
        assert!(t.last_active().unwrap() > 5_000);
    }

    fn edit(id: &str, status: Status, old: &str, new: &str) -> Event {
        Event::Tool {
            id: id.into(),
            patch: ToolPatch {
                status: Some(status),
                diffs: Some(vec![FileDiff { path: "a.rs".into(), old: Some(old.into()), new: new.into() }]),
                ..Default::default()
            },
        }
    }

    /// What the user has said so far, in order.
    fn said(t: &Thread) -> Vec<&str> {
        t.blocks.iter().filter_map(|b| if let Block::User(m) = b { Some(m.text.as_ref()) } else { None }).collect()
    }

    #[test]
    fn folds_events_into_blocks() {
        let mut t = Thread::new(1, 0);
        t.apply(Event::Text("Hel".into()));
        t.apply(Event::Text("lo".into()));
        t.apply(Event::Tool { id: "1".into(), patch: ToolPatch { title: Some("read".into()), ..Default::default() } });
        t.apply(Event::Tool { id: "1".into(), patch: ToolPatch { status: Some(Status::Done), ..Default::default() } });
        t.apply(Event::Text("Done".into()));
        assert_eq!(t.blocks.len(), 3);
        assert!(matches!(&t.blocks[0], Block::Text(s) if s == "Hello"));
        assert!(matches!(&t.blocks[1], Block::Tool(c) if c.title == "read" && c.status == Status::Done));
    }

    #[test]
    fn a_reused_tool_id_in_a_later_turn_is_a_new_call() {
        let mut t = Thread::new(1, 0);
        let read = |status| Event::Tool { id: "1".into(), patch: ToolPatch { title: Some("read".into()), status: Some(status), ..Default::default() } };
        t.prompt("one", "one".into());
        t.apply(read(Status::Done));
        t.prompt("two", "two".into());
        t.apply(read(Status::Running));
        let tools: Vec<_> = t.blocks.iter().filter_map(|b| if let Block::Tool(c) = b { Some(c.status) } else { None }).collect();
        assert_eq!(tools, [Status::Done, Status::Running]);
    }

    #[test]
    fn files_list_the_latest_first_and_keep_edits() {
        let mut t = Thread::new(1, 0);
        let touch = |id: &str, kind, path: &str| Event::Tool {
            id: id.into(),
            patch: ToolPatch { kind: Some(kind), status: Some(Status::Done), locations: Some(vec![path.into()]), ..Default::default() },
        };
        t.apply(touch("1", Kind::Read, "a.rs"));
        t.apply(edit("2", Status::Done, "x", "y"));
        t.apply(touch("3", Kind::Read, "b.rs"));
        t.apply(touch("4", Kind::Read, "a.rs"));
        let files: Vec<_> = t.files().into_iter().map(|(p, touch)| (p.to_string_lossy().into_owned(), touch)).collect();
        assert_eq!(files, [("a.rs".into(), Touch::Edited), ("b.rs".into(), Touch::Read)]);
    }

    #[test]
    fn changes_span_first_old_to_last_new_and_skip_unfinished_edits() {
        let mut t = Thread::new(1, 0);
        t.apply(edit("1", Status::Done, "a\nb\n", "a\nB\n"));
        t.apply(edit("2", Status::Done, "a\nB\n", "a\nB\nc\n"));
        t.apply(edit("3", Status::Pending, "a\nB\nc\n", ""));
        let changes = t.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].old.as_deref(), changes[0].new.as_str()), (Some("a\nb\n"), "a\nB\nc\n"));
        assert_eq!((changes[0].adds, changes[0].dels), (2, 1));
    }

    #[test]
    fn questions_gather_picks_and_check_whats_required() {
        use crate::agent::{Pick, Question};
        let pick = |v: &str| Pick { value: v.into(), label: v.to_uppercase(), description: None };
        let question = |field: &str, kind, required| Question { field: field.into(), title: Some(field.into()), prompt: None, kind, required };
        let mut t = Thread::new(1, 0);
        t.state = ThreadState::Running;
        t.apply(Event::Ask { id: 7, message: "?".into(), questions: vec![
            question("one", QuestionKind::Single(vec![pick("a"), pick("b")]), true),
            question("many", QuestionKind::Multi(vec![pick("x"), pick("y")]), false),
            question("n", QuestionKind::Number { integer: true }, false),
        ]});
        assert!(t.state == ThreadState::Waiting && t.awaiting_user());
        let Some(Block::Ask(ask)) = t.blocks.last_mut() else { panic!() };
        assert_eq!(ask.replies(|_| String::new()), Err("one needs an answer".into()));
        ask.toggle("one", "a");
        ask.toggle("one", "b");
        ask.toggle("many", "x");
        ask.toggle("many", "y");
        ask.toggle("many", "x");
        assert_eq!(ask.replies(|_| "two".into()), Err("n needs a whole number".into()));
        let replies = ask.replies(|_| " 2 ".into()).unwrap();
        assert_eq!(replies, [
            ("one".into(), Reply::Text("b".into())),
            ("many".into(), Reply::Picks(vec!["y".into()])),
            ("n".into(), Reply::Integer(2)),
        ]);
        assert_eq!(ask.shown(&replies), [("one".into(), "B".into()), ("many".into(), "Y".into()), ("n".into(), "2".into())]);
        t.apply(Event::TurnEnded(Ok(Stop::Cancelled)));
        assert!(!t.awaiting_user());
        assert!(matches!(&t.blocks[0], Block::Ask(a) if matches!(a.outcome, Some(AskOutcome::Dismissed))));
    }

    #[test]
    fn a_cut_off_turn_can_be_continued() {
        let mut t = Thread::new(1, 0);
        t.state = ThreadState::Running;
        t.settle(Some(now_ms()));
        assert!(t.interrupted());
        t.ready = true;
        t.submit("Continue", "Continue".into(), false);
        assert!(!t.interrupted());
        t.apply(Event::TurnEnded(Ok(Stop::Cancelled)));
        assert!(t.interrupted());
        t.submit("next", "next".into(), false);
        t.apply(Event::TurnEnded(Ok(Stop::Finished)));
        assert!(!t.interrupted());
    }

    #[test]
    fn a_closed_connection_fails_a_running_thread() {
        let mut t = Thread::new(1, 0);
        t.ready = true;
        t.state = ThreadState::Running;
        t.apply(Event::Permission { id: 1, tool: "x".into(), title: "Edit".into(), answers: Vec::new() });
        t.apply(Event::Closed(None));
        assert!(t.state == ThreadState::Failed && !t.ready && t.problem.is_some());
        assert!(matches!(&t.blocks[0], Block::Permission(p) if p.chosen.is_some()));
    }

    #[test]
    fn a_steer_keeps_the_current_turns_permission_and_tool_call() {
        let mut t = Thread::new(1, 0);
        t.ready = true;
        t.supports_steering = true;
        t.submit("initial", "initial".into(), true);
        t.apply(edit("x", Status::Running, "old", "new"));
        t.apply(Event::Permission { id: 1, tool: "x".into(), title: "Edit".into(), answers: Vec::new() });
        t.submit("adjust", "adjust".into(), true);
        assert!(t.pending_steer.is_some());
        t.apply(Event::SteerEnded(Ok(SteerOutcome::Injected)));
        assert!(t.pending_steer.is_none());
        assert!(t.state == ThreadState::Waiting);
        assert!(matches!(&t.blocks[2], Block::Permission(p) if p.chosen.is_none()));
        t.apply(edit("x", Status::Done, "old", "new"));
        assert!(matches!(&t.blocks[1], Block::Tool(call) if call.status == Status::Done));
        assert!(matches!(&t.blocks[3], Block::User(m) if m.text.as_ref() == "adjust"));
        t.apply(Event::TurnEnded(Ok(Stop::Finished)));
        assert!(t.state == ThreadState::Done);
        assert!(matches!(t.blocks.last(), Some(Block::TurnEnded { .. })));
    }

    #[test]
    fn messages_saved_without_a_time_still_read_back() {
        let old: Block = serde_json::from_str(r#"{"User":"hi"}"#).unwrap();
        assert!(matches!(&old, Block::User(m) if m.text.as_ref() == "hi" && m.at.is_none()));
        let new: Block = serde_json::from_value(serde_json::to_value(Block::User(Message::now("hi"))).unwrap()).unwrap();
        assert!(matches!(&new, Block::User(m) if m.at.is_some()));
    }

    #[test]
    fn a_late_steer_becomes_one_prompt_after_the_running_turn_finishes() {
        for reply_first in [true, false] {
            let mut t = Thread::new(1, 0);
            t.ready = true;
            t.supports_steering = true;
            t.submit("initial", "initial".into(), true);
            t.submit("first", "first".into(), true);
            t.submit("second", "second".into(), false);
            let ended = Event::TurnEnded(Ok(Stop::Finished));
            let fallback = Event::SteerEnded(Ok(SteerOutcome::PromptRequired));
            let (first, second) = if reply_first { (fallback, ended) } else { (ended, fallback) };
            t.apply(first);
            assert_eq!(t.turn_start, 0);
            t.apply(second);
            assert!(t.state == ThreadState::Running);
            assert_eq!(said(&t), ["initial", "first"]);
            assert_eq!(t.queued.len(), 1);
            t.apply(Event::TurnEnded(Ok(Stop::Finished)));
            assert_eq!(said(&t), ["initial", "first", "second"]);
            assert!(t.queued.is_empty());
        }
    }

    #[test]
    fn a_follow_up_queues_when_the_agent_does_not_support_steering() {
        let mut t = Thread::new(1, 0);
        t.ready = true;
        t.submit("initial", "initial".into(), false);
        t.submit("follow-up", "follow-up".into(), true);
        assert!(t.pending_steer.is_none());
        assert_eq!(t.queued.len(), 1);
        t.apply(Event::TurnEnded(Ok(Stop::Finished)));
        assert_eq!(said(&t), ["initial", "follow-up"]);
        assert!(matches!(&t.blocks[1], Block::TurnEnded { .. }), "the first turn closes before the follow-up starts");
    }

    #[test]
    fn a_signed_out_agent_leaves_how_to_sign_in() {
        let signed_out = || Failure::SignedOut { agent: "Claude Code".into(), command: Some("claude auth login".into()) };
        let mut t = Thread::new(0, 0);
        t.submit("hi", "hi".into(), false);
        t.apply(Event::TurnEnded(Err(signed_out())));
        assert!(matches!(&t.blocks[1], Block::SignIn { agent, command: Some(_) } if agent == "Claude Code"));
        assert!(t.state == ThreadState::Failed && matches!(t.blocks.last(), Some(Block::TurnEnded { .. })));
        // Refused before the session opened: the same help, rather than a bare problem.
        let mut t = Thread::new(0, 0);
        t.apply(Event::Closed(Some(signed_out())));
        assert!(matches!(t.blocks.last(), Some(Block::SignIn { .. })) && t.problem.is_none());
    }
}

