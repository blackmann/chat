//! Talks to a coding agent over the Agent Client Protocol: one subprocess and one
//! session per thread. The protocol's types stop here; the rest of the app sees
//! only the plain commands and events below, so another transport could feed
//! the same model.
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, LineDirection, UntypedMessage,
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
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const CODEX_ACP_PACKAGE: &str = "@agentclientprotocol/codex-acp";
const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

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
            Self::new("Claude Code", "npx", &["-y", CLAUDE_ACP_PACKAGE]),
            Self::new("Codex", "npx", &["-y", CODEX_ACP_PACKAGE]),
        ]
    }

    /// Which known agent this is, by its name or the adapter it runs, for its icon.
    pub fn brand(&self) -> Option<crate::icons::Glyph> {
        let hay = std::iter::once(&self.name).chain(std::iter::once(&self.command)).chain(&self.args).map(|s| s.to_lowercase());
        for s in hay {
            if s.contains("claude") {
                return Some(crate::icons::Glyph::Claude);
            }
            if s.contains("codex") || s.contains("openai") {
                return Some(crate::icons::Glyph::OpenAi);
            }
        }
        None
    }

    fn uses_claude_adapter(&self) -> bool {
        Path::new(&self.command).file_name() == Some(OsStr::new("claude-agent-acp"))
            || self.args.iter().any(|arg| {
                arg == CLAUDE_ACP_PACKAGE
                    || arg.strip_prefix(CLAUDE_ACP_PACKAGE).is_some_and(|suffix| suffix.starts_with('@'))
            })
    }

    fn uses_codex_adapter(&self) -> bool {
        Path::new(&self.command).file_name() == Some(OsStr::new("codex-acp"))
            || self.args.iter().any(|arg| {
                arg == CODEX_ACP_PACKAGE
                    || arg.strip_prefix(CODEX_ACP_PACKAGE).is_some_and(|suffix| suffix.starts_with('@'))
            })
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

/// One of a subscription's usage limits, as the agent last reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct RateLimit {
    /// The window, as Claude names it: `five_hour`, `seven_day`, `seven_day_opus`, …
    pub window: String,
    pub status: LimitStatus,
    /// How much of the window is used, 0–100, when told.
    pub used: Option<f64>,
    /// When the window resets, in Unix seconds.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitStatus {
    Allowed,
    Warning,
    Rejected,
}

impl RateLimit {
    /// Read the Claude SDK's `SDKRateLimitInfo`.
    fn parse(info: &serde_json::Value) -> Option<Self> {
        let status = match info.get("status")?.as_str()? {
            "allowed" => LimitStatus::Allowed,
            "allowed_warning" => LimitStatus::Warning,
            "rejected" => LimitStatus::Rejected,
            _ => return None,
        };
        let window = info.get("rateLimitType")?.as_str()?.to_string();
        // A fraction from the API headers; take a percentage too, in case it's ever sent as one.
        let used = info.get("utilization").and_then(|u| u.as_f64()).map(|u| if u <= 1. { u * 100. } else { u }).map(|u| u.clamp(0., 100.));
        let resets_at = info.get("resetsAt").and_then(|r| r.as_f64()).map(|r| if r > 1e11 { r / 1000. } else { r } as i64);
        Some(Self { window, status, used, resets_at })
    }

    /// Shorter windows first: the five-hour one, then the weekly ones, then overage.
    pub fn rank(&self) -> u8 {
        match self.window.as_str() {
            "five_hour" => 0,
            w if w.starts_with("seven_day") => 1,
            _ => 2,
        }
    }

    /// A short name for the status bar: `5h`, `Week`, `Opus week`, …
    pub fn short_name(&self) -> String {
        match self.window.as_str() {
            "five_hour" => "5h".into(),
            "seven_day" => "Week".into(),
            "overage" => "Extra".into(),
            w => match w.strip_prefix("seven_day_") {
                Some(model) => format!("{} week", capitalize(&model.replace('_', " "))),
                None => capitalize(&w.replace('_', " ")),
            },
        }
    }

    /// The full name, for a tooltip.
    pub fn long_name(&self) -> String {
        match self.window.as_str() {
            "five_hour" => "5-hour limit".into(),
            "seven_day" => "Weekly limit".into(),
            "overage" => "Extra usage".into(),
            _ => format!("{} limit", self.short_name().replace("week", "weekly")),
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The session is open and can take prompts. `session` is the agent's id
    /// for it, which reopens it later.
    Ready { agent: String, session: String, history: History, settings: Vec<Setting>, supports_steering: bool },
    Text(String),
    Thought(String),
    Tool { id: String, patch: ToolPatch },
    Plan(Vec<(Step, String)>),
    Settings(Vec<Setting>),
    /// Context window use, in tokens, and what the session has cost so far
    /// (amount and currency) when the agent says.
    Usage { used: u64, size: u64, cost: Option<(f64, String)> },
    /// Where one of the account's usage limits stands (Claude subscriptions).
    RateLimit(RateLimit),
    /// The slash commands the agent offers: name and description.
    Commands(Vec<(String, String)>),
    Title(String),
    /// The agent wants a decision before it goes on.
    Permission { id: u64, tool: String, title: String, answers: Vec<Answer> },
    /// The turn is over; `Err` carries why it failed.
    TurnEnded(Result<Stop, String>),
    /// The steering request settled independently of the running turn.
    SteerEnded(Result<SteerOutcome, String>),
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

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum SteerOutcome {
    Injected,
    PromptRequired,
    StartedNewTurn,
    Failed,
}

#[derive(Debug)]
pub enum Command {
    Prompt(String),
    Steer(String),
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
    extra: Vec<PathBuf>,
    previous: Option<String>,
    picks: Vec<(String, String)>,
) -> (UnboundedSender<Command>, UnboundedReceiver<Event>, impl Future<Output = ()> + Send + 'static) {
    let (command_tx, command_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();
    let run = async move {
        let result = run(spec, cwd, extra, previous, picks, command_rx, event_tx.clone()).await;
        let _ = event_tx.unbounded_send(Event::Closed(result.err().map(|e| e.to_string())));
    };
    (command_tx, event_rx, run)
}

async fn run(
    spec: AgentSpec,
    cwd: PathBuf,
    extra: Vec<PathBuf>,
    previous: Option<String>,
    picks: Vec<(String, String)>,
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) -> Result<(), agent_client_protocol::Error> {
    let mut config = AcpAgentConfig::new(&spec.command).args(spec.args.iter().cloned());
    if let Some(path) = shell_path() {
        config = config.env("PATH", path);
    }
    // Claude Code leaves its task-list tools out for newer models unless asked;
    // the adapter turns those tools into the plan shown in the Context panel.
    if spec.uses_claude_adapter() && std::env::var_os("CLAUDE_CODE_ENABLE_TODO_TOOLS").is_none() {
        config = config.env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1");
    }
    // The adapter forwards CODEX_CONFIG with thread/resume. Restoring the model
    // through session/set_config_option happens after Codex checks the rollout.
    if previous.is_some() && spec.uses_codex_adapter() {
        let inherited = std::env::var_os("CODEX_CONFIG");
        if let Some(overrides) = codex_resume_config(inherited.as_deref(), &picks) {
            config = config.env("CODEX_CONFIG", overrides);
        }
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
                    .send_request(ResumeSessionRequest::new(id.clone(), cwd.clone()).additional_directories(extra.clone()))
                    .block_task()
                    .await
                    .ok()
                    .map(|r| (id, r.config_options, r.modes)),
                Some(id) if capabilities.load_session => {
                    replaying.store(true, Ordering::SeqCst);
                    let loaded = connection.send_request(LoadSessionRequest::new(id.clone(), cwd.clone()).additional_directories(extra.clone())).block_task().await;
                    replaying.store(false, Ordering::SeqCst);
                    loaded.ok().map(|r| (id, r.config_options, r.modes))
                }
                _ => None,
            };
            let (session_id, options, modes, history) = match reopened {
                Some((id, options, modes)) => (id.into(), options, modes, History::Kept),
                None => {
                    let session = connection.send_request(NewSessionRequest::new(cwd).additional_directories(extra)).block_task().await?;
                    let history = if previous.is_some() { History::Lost } else { History::New };
                    (session.session_id, session.config_options, session.modes, history)
                }
            };
            let _ = events.unbounded_send(Event::Ready {
                agent: ready.agent_info.map(|i| i.title.unwrap_or(i.name)).unwrap_or(spec.name),
                session: session_id.to_string(),
                history,
                settings: settings(options.unwrap_or_default(), modes),
                supports_steering: ready.meta.as_ref()
                    .and_then(|meta| meta.get("steering"))
                    .and_then(|steering| steering["supported"].as_bool())
                    .unwrap_or(false),
            });
            // Long requests run as tasks so commands (cancel above all) keep flowing.
            while let Some(command) = commands.next().await {
                match command {
                    Command::Prompt(text) => {
                        let request = PromptRequest::new(session_id.clone(), vec![ContentBlock::Text(TextContent::new(text))]);
                        let response = connection.send_request(request);
                        let events = events.clone();
                        connection.spawn(async move {
                            let result = response.block_task().await;
                            // A turn stopped by a usage limit says which, and when it lifts.
                            if let Err(e) = &result
                                && let Some(limit) = e.data.as_ref().and_then(|d| d.get("rateLimit")).and_then(RateLimit::parse)
                            {
                                let _ = events.unbounded_send(Event::RateLimit(limit));
                            }
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
                    Command::Steer(text) => {
                        let params = serde_json::json!({
                            "sessionId": session_id,
                            "prompt": [ContentBlock::Text(TextContent::new(text))],
                            "_meta": { "steering": { "idleBehavior": "promptRequired" } },
                        });
                        let request = UntypedMessage::new("_session/steering", params)?;
                        let response = connection.send_request(request);
                        let events = events.clone();
                        connection.spawn(async move {
                            let result = response.block_task().await.map_err(|e| e.to_string())
                                .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string()));
                            let _ = events.unbounded_send(Event::SteerEnded(result));
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

fn codex_resume_config(inherited: Option<&OsStr>, picks: &[(String, String)]) -> Option<String> {
    let (_, model) = picks.iter().find(|(id, value)| id == "model" && !value.is_empty())?;
    let mut config: serde_json::Map<String, serde_json::Value> = match inherited {
        Some(value) if !value.is_empty() => serde_json::from_str(value.to_str()?).ok()?,
        _ => serde_json::Map::new(),
    };
    config.insert("model".into(), model.as_str().into());
    if let Some((_, effort)) = picks.iter().find(|(id, value)| id == "reasoning_effort" && !value.is_empty()) {
        config.insert("model_reasoning_effort".into(), effort.as_str().into());
    }
    Some(serde_json::Value::Object(config).to_string())
}

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
        SessionUpdate::UsageUpdate(usage) => {
            // Claude's adapter sends a rate limit event as a usage update that
            // repeats the last context figures, with the limit in `_meta`.
            let limit = usage.meta.as_ref().and_then(|m| m.get("_claude/rateLimit")).and_then(RateLimit::parse);
            match limit {
                Some(limit) => Event::RateLimit(limit),
                None => Event::Usage { used: usage.used, size: usage.size, cost: usage.cost.map(|c| (c.amount, c.currency)) },
            }
        }
        SessionUpdate::AvailableCommandsUpdate(update) => {
            Event::Commands(update.available_commands.into_iter().map(|c| (c.name, c.description)).collect())
        }
        SessionUpdate::SessionInfoUpdate(info) => Event::Title(info.title.take()?),
        _ => return None,
    })
}

/// The user's shell PATH. An app launched from the Dock gets a bare PATH that
/// has neither `npx` nor anything a version manager installed.
pub fn shell_path() -> Option<String> {
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

    #[test]
    fn rate_limit_reads_claude_info() {
        let limit = RateLimit::parse(&serde_json::json!({
            "status": "allowed_warning", "rateLimitType": "five_hour", "utilization": 0.82, "resetsAt": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(limit.status, LimitStatus::Warning);
        assert_eq!(limit.short_name(), "5h");
        assert_eq!(limit.used.map(|u| u.round()), Some(82.));
        assert_eq!(limit.resets_at, Some(1_800_000_000));

        let weekly = RateLimit::parse(&serde_json::json!({ "status": "rejected", "rateLimitType": "seven_day_opus", "resetsAt": 1_800_000_000_000u64 })).unwrap();
        assert_eq!((weekly.short_name().as_str(), weekly.long_name().as_str()), ("Opus week", "Opus weekly limit"));
        assert_eq!(weekly.resets_at, Some(1_800_000_000));
        assert!(limit.rank() < weekly.rank());

        assert_eq!(RateLimit::parse(&serde_json::json!({ "status": "allowed" })), None);
    }

    #[test]
    fn rate_limit_usage_update_becomes_its_own_event() {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "usage_update", "used": 10, "size": 100,
            "_meta": { "_claude/rateLimit": { "status": "allowed", "rateLimitType": "five_hour", "utilization": 0.1 } }
        }))
        .unwrap();
        assert!(matches!(translate(update), Some(Event::RateLimit(RateLimit { status: LimitStatus::Allowed, .. }))));
    }

    #[test]
    fn codex_resume_uses_the_saved_model_and_effort_with_existing_config() {
        let picks = vec![
            ("model".into(), "gpt-6.1-sol".into()),
            ("reasoning_effort".into(), "high".into()),
            ("mode".into(), "auto-review".into()),
        ];
        let inherited = OsStr::new(r#"{"model":"gpt-5.6-sol","model_reasoning_effort":"low","approval_policy":"on-request"}"#);
        let config = codex_resume_config(Some(inherited), &picks).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&config).unwrap(),
            serde_json::json!({
                "model": "gpt-6.1-sol",
                "model_reasoning_effort": "high",
                "approval_policy": "on-request",
            }),
        );
        assert!(codex_resume_config(None, &picks[1..]).is_none());
        assert!(codex_resume_config(Some(OsStr::new("invalid JSON")), &picks).is_none());
    }

    /// Drive the stand-in agent through a model switch and a steer during a permission request.
    #[test]
    fn talks_to_the_mock_agent() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Vec::new(), None, Vec::new());
        std::thread::spawn(move || block_on(run));
        let mut next = move || block_on(events.next()).expect("connection closed early");

        // Commands can be announced as the session opens, before or after Ready.
        let Event::Ready { agent, settings, history, supports_steering, .. } = (0..10).map(|_| next()).find(|e| !matches!(e, Event::Commands(_))).unwrap()
        else {
            panic!("expected Ready first")
        };
        assert_eq!(history, History::New);
        assert_eq!(agent, "Mock Agent");
        assert!(supports_steering);
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
        let mut permission = None;
        loop {
            match next() {
                Event::Text(t) => said.push_str(&t),
                Event::Permission { id, answers, .. } => {
                    let allow = answers.iter().find(|a| a.allow).unwrap().id.clone();
                    permission = Some((id, allow));
                    commands.unbounded_send(Command::Steer("keep the edit small".into())).unwrap();
                }
                Event::SteerEnded(result) => {
                    assert_eq!(result, Ok(SteerOutcome::Injected));
                    let (id, allow) = permission.take().expect("the turn should still be waiting");
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
        assert!(said.contains("Steered: keep the edit small"), "{said}");
        assert!(edited && planned);
        commands.unbounded_send(Command::Steer("arrived late".into())).unwrap();
        assert_eq!(next(), Event::SteerEnded(Ok(SteerOutcome::PromptRequired)));
    }

    /// Reopening a session keeps it, and its replay never reaches the thread.
    #[test]
    fn reopens_a_session_without_its_replay() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Vec::new(), Some("mock-7".into()), Vec::new());
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
