//! Projects, threads and what a thread has gathered from its agent. A thread is
//! a fold over the agent's events; every panel reads from it.
use crate::agent::{Answer, Command, Event, FileDiff, History, Kind, Setting, Status, Step, Stop};
use serde::{Deserialize, Serialize};
use futures::channel::mpsc::UnboundedSender;
use gpui::{SharedString, Task};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadState {
    /// Nothing asked yet, or the last turn was stopped.
    Idle,
    Running,
    /// The agent is waiting on a permission answer.
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

#[derive(Serialize, Deserialize)]
pub enum Block {
    User(SharedString),
    /// Agent prose, grown chunk by chunk.
    Text(String),
    Thought(String),
    Tool(ToolCall),
    Permission(Permission),
    /// Something the app has to say: a stop, a failure.
    Notice(String),
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
    pub blocks: Vec<Block>,
    /// Index into the configured agents.
    pub agent: usize,
    pub session: Option<Session>,
    /// The session has opened; until then prompts wait in the channel.
    pub ready: bool,
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
            blocks: Vec::new(),
            agent,
            session: None,
            ready: false,
            settings: Vec::new(),
            plan: Vec::new(),
            usage: None,
            cost: None,
            commands: Vec::new(),
            problem: None,
            session_id: None,
            picks: Vec::new(),
        }
    }
    pub fn send(&self, command: Command) {
        if let Some(session) = &self.session {
            let _ = session.commands.unbounded_send(command);
        }
    }
    pub fn notice(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Notice(text.into()));
    }
    /// Unanswered permission requests can't be answered once a turn is over.
    fn dismiss_permissions(&mut self) {
        for block in &mut self.blocks {
            if let Block::Permission(p) = block
                && p.chosen.is_none()
            {
                p.chosen = Some("Dismissed".into());
            }
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
            Event::Ready { settings, session, history, .. } => {
                self.ready = true;
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
                let existing = self
                    .blocks
                    .iter_mut()
                    .rev()
                    .take_while(|b| !matches!(b, Block::User(_)))
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
            Event::TurnEnded(result) => {
                self.dismiss_permissions();
                self.state = match result {
                    Ok(Stop::Finished) => ThreadState::Done,
                    Ok(Stop::Cancelled) => {
                        self.settle_tools();
                        self.notice("Stopped");
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
                    Err(error) => {
                        self.settle_tools();
                        self.notice(error);
                        ThreadState::Failed
                    }
                };
            }
            Event::Closed(reason) => {
                let interrupted = self.state.busy() || !self.ready;
                self.session = None;
                self.ready = false;
                self.dismiss_permissions();
                self.settle_tools();
                if reason.is_some() || interrupted {
                    self.problem = Some(reason.map_or("The agent exited.".into(), |r| format!("The agent stopped: {r}")));
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
    /// Bring a thread read from disk to rest: nothing can still be running in a
    /// process that is gone.
    pub fn settle(&mut self) {
        if self.state.busy() {
            self.dismiss_permissions();
            self.settle_tools();
            self.notice("Interrupted when Chat closed.");
            self.state = ThreadState::Idle;
        }
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
    pub branch: Option<String>,
    pub threads: Vec<Thread>,
}
impl Project {
    pub fn chats() -> Self {
        Self { name: "Chats".into(), path: None, branch: None, threads: Vec::new() }
    }
    pub fn open(path: PathBuf) -> Self {
        Self {
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string()),
            branch: branch(&path),
            path: Some(path),
            threads: Vec::new(),
        }
    }
    /// A path as the project sees it: relative to its root where it can be.
    pub fn relative<'a>(&self, path: &'a Path) -> &'a Path {
        self.path.as_deref().and_then(|root| path.strip_prefix(root).ok()).unwrap_or(path)
    }
}

/// The checked-out branch, read straight from `.git/HEAD`.
fn branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(name) => name.to_string(),
        None => head.get(..7)?.to_string(),
    })
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
        t.blocks.push(Block::User("one".into()));
        t.apply(read(Status::Done));
        t.blocks.push(Block::User("two".into()));
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
    fn a_closed_connection_fails_a_running_thread() {
        let mut t = Thread::new(1, 0);
        t.ready = true;
        t.state = ThreadState::Running;
        t.apply(Event::Permission { id: 1, tool: "x".into(), title: "Edit".into(), answers: Vec::new() });
        t.apply(Event::Closed(None));
        assert!(t.state == ThreadState::Failed && !t.ready && t.problem.is_some());
        assert!(matches!(&t.blocks[0], Block::Permission(p) if p.chosen.is_some()));
    }
}
