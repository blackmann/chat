//! Talks to a coding agent over the Agent Client Protocol: one subprocess and one
//! session per thread. The protocol's types stop here; the rest of the app sees
//! only the plain commands and events below, so another transport could feed
//! the same model.
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, LineDirection,
    schema::ProtocolVersion,
    v1::{
        ConnectionTo,
        schema::{
            CancelNotification, ContentBlock, Implementation, InitializeRequest, LoadSessionRequest, NewSessionRequest,
            PermissionOptionKind, PlanEntryStatus, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
            RequestPermissionResponse, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
            SessionConfigOptionCategory, SessionConfigSelectOptions, SessionModeState, SessionNotification,
            SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest, StopReason, TextContent,
            ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
        },
    },
};
use futures::{
    StreamExt,
    channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// How to start an agent that speaks ACP on its stdio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}
impl AgentSpec {
    fn new(name: &str, command: &str, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
    /// Claude Code and Codex, through their ACP adapters.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self::new("Claude Code", "npx", &["-y", "@agentclientprotocol/claude-agent-acp"]),
            Self::new("Codex", "npx", &["-y", "@agentclientprotocol/codex-acp"]),
        ]
    }
}

/// What a session setting is for, so the composer knows where to show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Mode,
    Model,
    Effort,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub detail: Option<String>,
}

/// A session setting with a fixed set of values: the model, the reasoning effort,
/// the permission mode. Changing one applies to the live session, mid-thread.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub current: String,
    pub choices: Vec<Choice>,
}
impl Setting {
    pub fn current_name(&self) -> &str {
        self.choices.iter().find(|c| c.id == self.current).map_or(self.current.as_str(), |c| c.name.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Todo,
    Doing,
    Done,
}

/// A file as a tool call left it; `old` is `None` for a new file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old: Option<String>,
    pub new: String,
}

/// The parts of a tool call an update may change; `None` leaves a part as it was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolPatch {
    pub title: Option<String>,
    pub kind: Option<Kind>,
    pub status: Option<Status>,
    pub diffs: Option<Vec<FileDiff>>,
    /// Text the tool produced, e.g. command output.
    pub output: Option<String>,
    /// Files the call read or touched.
    pub locations: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub id: String,
    pub name: String,
    pub allow: bool,
}

/// What became of a thread's earlier conversation when its session opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    /// There was none: a new thread.
    New,
    /// The agent reopened the earlier session and remembers it.
    Kept,
    /// The agent couldn't reopen it, so this session starts without it.
    Lost,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The session is open and can take prompts. `session` is the agent's id
    /// for it, which reopens it later.
    Ready { agent: String, session: String, history: History, settings: Vec<Setting> },
    Text(String),
    Thought(String),
    Tool { id: String, patch: ToolPatch },
    Plan(Vec<(Step, String)>),
    Settings(Vec<Setting>),
    /// Context window use, in tokens, and what the session has cost so far
    /// (amount and currency) when the agent says.
    Usage { used: u64, size: u64, cost: Option<(f64, String)> },
    /// The slash commands the agent offers: name and description.
    Commands(Vec<(String, String)>),
    Title(String),
    /// The agent wants a decision before it goes on.
    Permission { id: u64, tool: String, title: String, answers: Vec<Answer> },
    /// The turn is over; `Err` carries why it failed.
    TurnEnded(Result<Stop, String>),
    /// The connection is gone, with the reason if it wasn't asked for.
    Closed(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Finished,
    Cancelled,
    Refused,
    /// Ran out of tokens or turns.
    Limit,
}

#[derive(Debug)]
pub enum Command {
    Prompt(String),
    Cancel,
    Set { setting: String, choice: String },
    /// Answer a permission request; `None` dismisses it.
    Answer { id: u64, answer: Option<String> },
}

/// Open a connection: returns the command and event ends, and the future that
/// drives it. Dropping that future kills the agent process. With `previous`,
/// the agent is asked to reopen that session rather than start a new one.
pub fn connect(
    spec: AgentSpec,
    cwd: PathBuf,
    previous: Option<String>,
) -> (UnboundedSender<Command>, UnboundedReceiver<Event>, impl Future<Output = ()> + Send + 'static) {
    let (command_tx, command_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();
    let run = async move {
        let result = run(spec, cwd, previous, command_rx, event_tx.clone()).await;
        let _ = event_tx.unbounded_send(Event::Closed(result.err().map(|e| e.to_string())));
    };
    (command_tx, event_rx, run)
}

async fn run(
    spec: AgentSpec,
    cwd: PathBuf,
    previous: Option<String>,
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) -> Result<(), agent_client_protocol::Error> {
    let mut config = AcpAgentConfig::new(&spec.command).args(spec.args.iter().cloned());
    if let Some(path) = shell_path() {
        config = config.env("PATH", path);
    }
    let mut agent = AcpAgent::new(config);
    if std::env::var_os("CHAT_ACP_LOG").is_some() {
        agent = agent.with_debug(|line, direction| {
            let arrow = match direction {
                LineDirection::Stdin => "→",
                LineDirection::Stdout => "←",
                LineDirection::Stderr => "!",
            };
            eprintln!("{arrow} {line}");
        });
    }
    // Permission requests wait here until the user answers.
    let waiting = Arc::new(Mutex::new(HashMap::new()));
    // Loading a session replays it as updates; the transcript is already on
    // screen, so those are dropped until the load answers.
    let replaying = Arc::new(AtomicBool::new(false));
    let mut next_permission = 0u64;
    Client
        .v1()
        .name("chat")
        .on_receive_notification(
            {
                let (events, replaying) = (events.clone(), replaying.clone());
                async move |notification: SessionNotification, _cx| {
                    if replaying.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    if let Some(event) = translate(notification.update) {
                        let _ = events.unbounded_send(event);
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            {
                let (events, waiting) = (events.clone(), waiting.clone());
                async move |request: RequestPermissionRequest, responder, _cx| {
                    next_permission += 1;
                    waiting.lock().unwrap().insert(next_permission, responder);
                    let _ = events.unbounded_send(Event::Permission {
                        id: next_permission,
                        tool: request.tool_call.tool_call_id.to_string(),
                        title: request.tool_call.fields.title.clone().unwrap_or_else(|| "Allow this action?".into()),
                        answers: request
                            .options
                            .iter()
                            .map(|o| Answer {
                                id: o.option_id.to_string(),
                                name: o.name.clone(),
                                allow: matches!(o.kind, PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways),
                            })
                            .collect(),
                    });
                    // The request also describes its tool call, sometimes for the first time.
                    let _ = events.unbounded_send(Event::Tool {
                        id: request.tool_call.tool_call_id.to_string(),
                        patch: patch(request.tool_call.fields),
                    });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let ready = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_info(Implementation::new("chat", env!("CARGO_PKG_VERSION"))),
                )
                .block_task()
                .await?;
            let capabilities = &ready.agent_capabilities;
            // Reopen the earlier session where the agent can: resume reattaches
            // quietly, load replays it.
            let reopened = match previous.clone() {
                Some(id) if capabilities.session_capabilities.resume.is_some() => connection
                    .send_request(ResumeSessionRequest::new(id.clone(), cwd.clone()))
                    .block_task()
                    .await
                    .ok()
                    .map(|r| (id, r.config_options, r.modes)),
                Some(id) if capabilities.load_session => {
                    replaying.store(true, Ordering::SeqCst);
                    let loaded = connection.send_request(LoadSessionRequest::new(id.clone(), cwd.clone())).block_task().await;
                    replaying.store(false, Ordering::SeqCst);
                    loaded.ok().map(|r| (id, r.config_options, r.modes))
                }
                _ => None,
            };
            let (session_id, options, modes, history) = match reopened {
                Some((id, options, modes)) => (id.into(), options, modes, History::Kept),
                None => {
                    let session = connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
                    let history = if previous.is_some() { History::Lost } else { History::New };
                    (session.session_id, session.config_options, session.modes, history)
                }
            };
            let _ = events.unbounded_send(Event::Ready {
                agent: ready.agent_info.map(|i| i.title.unwrap_or(i.name)).unwrap_or(spec.name),
                session: session_id.to_string(),
                history,
                settings: settings(options.unwrap_or_default(), modes),
            });
            // Long requests run as tasks so commands (cancel above all) keep flowing.
            while let Some(command) = commands.next().await {
                match command {
                    Command::Prompt(text) => {
                        let request = PromptRequest::new(session_id.clone(), vec![ContentBlock::Text(TextContent::new(text))]);
                        let (connection, events) = (connection.clone(), events.clone());
                        connection.clone().spawn(async move {
                            let result = connection.send_request(request).block_task().await;
                            let _ = events.unbounded_send(Event::TurnEnded(match result {
                                Ok(response) => Ok(match response.stop_reason {
                                    StopReason::EndTurn => Stop::Finished,
                                    StopReason::Cancelled => Stop::Cancelled,
                                    StopReason::Refusal => Stop::Refused,
                                    _ => Stop::Limit,
                                }),
                                Err(e) => Err(e.to_string()),
                            }));
                            Ok(())
                        })?;
                    }
                    Command::Cancel => {
                        // A cancelled turn must not leave permission requests hanging.
                        for (_, responder) in waiting.lock().unwrap().drain() {
                            let _ = responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                        }
                        connection.send_notification(CancelNotification::new(session_id.clone()))?;
                    }
                    Command::Answer { id, answer } => {
                        if let Some(responder) = waiting.lock().unwrap().remove(&id) {
                            let _ = responder.respond(RequestPermissionResponse::new(match answer {
                                Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option)),
                                None => RequestPermissionOutcome::Cancelled,
                            }));
                        }
                    }
                    Command::Set { setting, choice } => {
                        let (connection, events, session_id) = (connection.clone(), events.clone(), session_id.clone());
                        connection.clone().spawn(async move {
                            // Agents confirm the change by sending the settings back; a refusal changes nothing.
                            if setting == LEGACY_MODE {
                                let request = SetSessionModeRequest::new(session_id, choice);
                                let _ = connection.send_request(request).block_task().await;
                            } else {
                                let request = SetSessionConfigOptionRequest::new(session_id, setting, choice.as_str());
                                if let Ok(response) = connection.send_request(request).block_task().await {
                                    let _ = events.unbounded_send(Event::Settings(settings(response.config_options, None)));
                                }
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            Ok(())
        })
        .await
}

const LEGACY_MODE: &str = "\0mode";

fn settings(options: Vec<SessionConfigOption>, modes: Option<SessionModeState>) -> Vec<Setting> {
    let mut out: Vec<Setting> = options
        .into_iter()
        .filter_map(|option| {
            let SessionConfigKind::Select(select) = option.kind else {
                return None;
            };
            let choices = match select.options {
                SessionConfigSelectOptions::Ungrouped(options) => options,
                SessionConfigSelectOptions::Grouped(groups) => groups.into_iter().flat_map(|g| g.options).collect(),
                _ => Vec::new(),
            };
            Some(Setting {
                id: option.id.to_string(),
                name: option.name,
                role: match option.category {
                    Some(SessionConfigOptionCategory::Mode) => Role::Mode,
                    Some(SessionConfigOptionCategory::Model) => Role::Model,
                    Some(SessionConfigOptionCategory::ThoughtLevel) => Role::Effort,
                    _ => Role::Other,
                },
                current: select.current_value.to_string(),
                choices: choices
                    .into_iter()
                    .map(|c| Choice { id: c.value.to_string(), name: c.name, detail: c.description })
                    .collect(),
            })
        })
        .collect();
    // Agents that predate config options still describe their modes separately.
    if let Some(modes) = modes
        && !out.iter().any(|s| s.role == Role::Mode)
    {
        out.insert(
            0,
            Setting {
                id: LEGACY_MODE.into(),
                name: "Mode".into(),
                role: Role::Mode,
                current: modes.current_mode_id.to_string(),
                choices: modes
                    .available_modes
                    .into_iter()
                    .map(|m| Choice { id: m.id.to_string(), name: m.name, detail: m.description })
                    .collect(),
            },
        );
    }
    out
}

fn text(block: ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) => Some(t.text),
        _ => None,
    }
}

fn patch(fields: ToolCallUpdateFields) -> ToolPatch {
    let (mut diffs, mut output) = (None, None);
    if let Some(content) = fields.content {
        let mut found = Vec::new();
        let mut said = String::new();
        for item in content {
            match item {
                ToolCallContent::Diff(d) => found.push(FileDiff { path: d.path, old: d.old_text, new: d.new_text }),
                ToolCallContent::Content(c) => {
                    if let Some(t) = text(c.content) {
                        said.push_str(&t);
                    }
                }
                _ => {}
            }
        }
        diffs = Some(found);
        output = Some(said);
    }
    ToolPatch {
        title: fields.title,
        kind: fields.kind.map(|k| match k {
            ToolKind::Read => Kind::Read,
            ToolKind::Edit => Kind::Edit,
            ToolKind::Delete => Kind::Delete,
            ToolKind::Move => Kind::Move,
            ToolKind::Search => Kind::Search,
            ToolKind::Execute => Kind::Execute,
            ToolKind::Think => Kind::Think,
            ToolKind::Fetch => Kind::Fetch,
            _ => Kind::Other,
        }),
        status: fields.status.map(|s| match s {
            ToolCallStatus::Pending => Status::Pending,
            ToolCallStatus::InProgress => Status::Running,
            ToolCallStatus::Completed => Status::Done,
            ToolCallStatus::Failed => Status::Failed,
            _ => Status::Running,
        }),
        diffs,
        output,
        locations: fields.locations.map(|l| l.into_iter().map(|l| l.path).collect()),
    }
}

fn translate(update: SessionUpdate) -> Option<Event> {
    Some(match update {
        SessionUpdate::AgentMessageChunk(chunk) => Event::Text(text(chunk.content)?),
        SessionUpdate::AgentThoughtChunk(chunk) => Event::Thought(text(chunk.content)?),
        SessionUpdate::ToolCall(call) => {
            let update = ToolCallUpdate::from(call);
            Event::Tool { id: update.tool_call_id.to_string(), patch: patch(update.fields) }
        }
        SessionUpdate::ToolCallUpdate(update) => Event::Tool { id: update.tool_call_id.to_string(), patch: patch(update.fields) },
        SessionUpdate::Plan(plan) => Event::Plan(
            plan.entries
                .into_iter()
                .map(|e| {
                    let step = match e.status {
                        PlanEntryStatus::Completed => Step::Done,
                        PlanEntryStatus::InProgress => Step::Doing,
                        _ => Step::Todo,
                    };
                    (step, e.content)
                })
                .collect(),
        ),
        SessionUpdate::ConfigOptionUpdate(update) => Event::Settings(settings(update.config_options, None)),
        SessionUpdate::UsageUpdate(usage) => Event::Usage {
            used: usage.used,
            size: usage.size,
            cost: usage.cost.map(|c| (c.amount, c.currency)),
        },
        SessionUpdate::AvailableCommandsUpdate(update) => {
            Event::Commands(update.available_commands.into_iter().map(|c| (c.name, c.description)).collect())
        }
        SessionUpdate::SessionInfoUpdate(info) => Event::Title(info.title.take()?),
        _ => return None,
    })
}

/// The user's shell PATH. An app launched from the Dock gets a bare PATH that
/// has neither `npx` nor anything a version manager installed.
fn shell_path() -> Option<String> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        let shell = std::env::var("SHELL").ok()?;
        // Markers fence the value off from whatever the shell's startup files print.
        let output = std::process::Command::new(shell)
            .args(["-l", "-i", "-c", "printf '<<%s>>' \"$PATH\""])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let printed = String::from_utf8_lossy(&output.stdout);
        let path = printed.rsplit_once("<<")?.1.split_once(">>")?.0;
        (!path.is_empty()).then(|| path.to_string())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    /// Drive the stand-in agent through a whole turn, including a mid-thread model switch.
    #[test]
    fn talks_to_the_mock_agent() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), None);
        std::thread::spawn(move || block_on(run));
        let mut next = move || block_on(events.next()).expect("connection closed early");

        // Commands can be announced as the session opens, before or after Ready.
        let Event::Ready { agent, settings, history, .. } = (0..10).map(|_| next()).find(|e| !matches!(e, Event::Commands(_))).unwrap()
        else {
            panic!("expected Ready first")
        };
        assert_eq!(history, History::New);
        assert_eq!(agent, "Mock Agent");
        assert_eq!(settings.iter().map(|s| s.role).collect::<Vec<_>>(), [Role::Mode, Role::Model, Role::Effort]);

        commands.unbounded_send(Command::Set { setting: "model".into(), choice: "small".into() }).unwrap();
        // The agent may announce its commands first; the settings reply follows.
        let settings = loop {
            match next() {
                Event::Commands(c) => assert!(c.iter().any(|(name, _)| name == "review")),
                Event::Settings(settings) => break settings,
                other => panic!("expected Settings, got {other:?}"),
            }
        };
        assert_eq!(settings[1].current_name(), "Mock Small");

        commands.unbounded_send(Command::Prompt("hello".into())).unwrap();
        let (mut said, mut edited, mut planned) = (String::new(), false, false);
        loop {
            match next() {
                Event::Text(t) => said.push_str(&t),
                Event::Permission { id, answers, .. } => {
                    let allow = answers.iter().find(|a| a.allow).unwrap().id.clone();
                    commands.unbounded_send(Command::Answer { id, answer: Some(allow) }).unwrap();
                }
                Event::Tool { patch, .. } => edited |= patch.diffs.is_some_and(|d| !d.is_empty()),
                Event::Plan(steps) => planned |= steps.iter().all(|(s, _)| *s == Step::Done),
                Event::TurnEnded(result) => {
                    assert_eq!(result, Ok(Stop::Finished));
                    break;
                }
                Event::Closed(why) => panic!("closed: {why:?}"),
                _ => {}
            }
        }
        assert!(said.contains("Running on `small`"), "{said}");
        assert!(edited && planned);
    }

    /// Reopening a session keeps it, and its replay never reaches the thread.
    #[test]
    fn reopens_a_session_without_its_replay() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Some("mock-7".into()));
        std::thread::spawn(move || block_on(run));
        let mut next = move || block_on(events.next()).expect("connection closed early");
        let Event::Ready { session, history, .. } = (0..10).map(|_| next()).find(|e| !matches!(e, Event::Commands(_))).unwrap()
        else {
            panic!("expected Ready first, with the replay dropped")
        };
        assert_eq!((session.as_str(), history), ("mock-7", History::Kept));
        commands.unbounded_send(Command::Prompt("explain".into())).unwrap();
        loop {
            match next() {
                Event::Text(t) => assert!(!t.contains("REPLAYED"), "replay leaked: {t}"),
                Event::TurnEnded(_) => break,
                _ => {}
            }
        }
    }
}
