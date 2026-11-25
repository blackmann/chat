//! Talks to a coding agent over the Agent Client Protocol: one subprocess and one
//! session per thread. The protocol's types stop here; the rest of the app sees
//! only the plain commands and events below, so another transport could feed
//! the same model.
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ErrorCode, LineDirection, UntypedMessage,
    schema::ProtocolVersion,
    v1::{
        ConnectionTo,
        schema::{
            CancelNotification, ClientCapabilities, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
            ElicitationAcceptAction, ElicitationAction, ElicitationCapabilities, ElicitationContentValue,
            ElicitationFormCapabilities, ElicitationMode, ElicitationPropertySchema, MultiSelectItems, McpServer, McpServerStdio, Implementation, InitializeRequest, LoadSessionRequest, NewSessionRequest,
            PermissionOptionKind, PlanEntryStatus, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
            RequestPermissionResponse, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
            SessionConfigOptionCategory, SessionConfigSelectOptions, SessionModeState, SessionNotification,
            SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest, StopReason, TextContent, ImageContent,
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
    collections::{BTreeMap, HashMap},
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
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

    /// The command that signs this agent in from a terminal, for the adapters
    /// Chat knows. Claude Code's adapter carries its own CLI; Codex has its own.
    fn sign_in(&self) -> Option<String> {
        if self.uses_claude_adapter() {
            Some(format!("npx -y {CLAUDE_ACP_PACKAGE} --cli auth login"))
        } else if self.uses_codex_adapter() {
            Some("npx -y @openai/codex login".into())
        } else {
            None
        }
    }

    /// An agent that needs signing in says only "Authentication required";
    /// that one is told apart so the thread can say what to do about it.
    fn failure(&self, error: agent_client_protocol::Error) -> Failure {
        if error.code == ErrorCode::AUTH_REQUIRED {
            Failure::SignedOut { agent: self.name.clone(), command: self.sign_in() }
        } else {
            Failure::Other(error.to_string())
        }
    }

    fn uses_claude_adapter(&self) -> bool {
        Path::new(&self.command).file_name() == Some(OsStr::new("claude-agent-acp"))
            || self.args.iter().any(|arg| {
                arg == CLAUDE_ACP_PACKAGE
                    || arg.strip_prefix(CLAUDE_ACP_PACKAGE).is_some_and(|suffix| suffix.starts_with('@'))
            })
    }

    /// Whether a pick made in one session becomes the agent's default for the
    /// next: Codex writes it to its config.
    pub fn saves_picks(&self) -> bool {
        self.uses_codex_adapter()
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

/// One field of a form the agent asks the user to fill in, such as one of
/// Claude's questions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// The key its answer goes back under.
    pub field: String,
    /// A short label (Claude's question header).
    pub title: Option<String>,
    /// The question itself, or what to type.
    pub prompt: Option<String>,
    pub kind: QuestionKind,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum QuestionKind {
    /// Pick one.
    Single(Vec<Pick>),
    /// Pick any.
    Multi(Vec<Pick>),
    Text,
    Yes,
    Number { integer: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    /// What goes back when it's picked.
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// An answer to one field of a form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Text(String),
    Picks(Vec<String>),
    Yes(bool),
    Integer(i64),
    Number(f64),
}

/// The fields of a form request the app can draw; `None` when one can't be,
/// since a form missing a field it requires can't be answered.
fn questions(mode: ElicitationMode) -> Option<Vec<Question>> {
    let ElicitationMode::Form(form) = mode else { return None };
    let required = form.requested_schema.required.unwrap_or_default();
    let mut questions = Vec::new();
    for (field, property) in form.requested_schema.properties {
        let choices = |options: Vec<agent_client_protocol::v1::schema::EnumOption>| {
            options.into_iter().map(|o| Pick { value: o.value, label: o.title, description: o.description }).collect()
        };
        let plain = |values: Vec<String>| values.into_iter().map(|v| Pick { value: v.clone(), label: v, description: None }).collect();
        let (title, prompt, kind) = match property {
            ElicitationPropertySchema::String(s) => {
                let kind = match (s.one_of, s.enum_values) {
                    (Some(options), _) => QuestionKind::Single(choices(options)),
                    (None, Some(values)) => QuestionKind::Single(plain(values)),
                    (None, None) => QuestionKind::Text,
                };
                (s.title, s.description, kind)
            }
            ElicitationPropertySchema::Array(a) => {
                let options = match a.items {
                    MultiSelectItems::Titled(t) => choices(t.options),
                    MultiSelectItems::String(s) => plain(s.values),
                    _ => return None,
                };
                (a.title, a.description, QuestionKind::Multi(options))
            }
            ElicitationPropertySchema::Boolean(b) => (b.title, b.description, QuestionKind::Yes),
            ElicitationPropertySchema::Number(n) => (n.title, n.description, QuestionKind::Number { integer: false }),
            ElicitationPropertySchema::Integer(n) => (n.title, n.description, QuestionKind::Number { integer: true }),
            _ if !required.contains(&field) => continue,
            _ => return None,
        };
        questions.push(Question { required: required.contains(&field), field, title, prompt, kind });
    }
    Some(questions)
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

/// Tokens spent, split the way the API bills them. `input` leaves out the
/// cache's reads and writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

/// What one turn spent: in all, and by model when the agent says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnUsage {
    pub tokens: Tokens,
    pub models: Vec<(String, Tokens)>,
}

impl TurnUsage {
    /// Read a prompt response's `usage` and its `_meta.quota`, which Claude's
    /// and Codex's adapters fill with a `token_count` per model. The per-model
    /// counts include subagents, which `usage` leaves out, so they win when there.
    pub fn parse(usage: Option<&serde_json::Value>, meta: Option<&serde_json::Value>) -> Option<TurnUsage> {
        let n = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let models: Vec<(String, Tokens)> = meta
            .and_then(|m| m.pointer("/quota/model_usage"))
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let model = m.get("model")?.as_str()?.to_string();
                let c = m.get("token_count")?;
                let tokens = Tokens {
                    input: n(c, "inputTokens"),
                    output: n(c, "outputTokens") + n(c, "reasoningOutputTokens"),
                    cache_read: n(c, "cachedInputTokens"),
                    cache_write: n(c, "cachedWriteTokens"),
                };
                (tokens.total() > 0).then_some((model, tokens))
            })
            .collect();
        let tokens = if models.is_empty() {
            let u = usage?;
            Tokens {
                input: n(u, "inputTokens"),
                output: n(u, "outputTokens") + n(u, "thoughtTokens"),
                cache_read: n(u, "cachedReadTokens"),
                cache_write: n(u, "cachedWriteTokens"),
            }
        } else {
            models.iter().fold(Tokens::default(), |mut t, (_, m)| {
                t.add(m);
                t
            })
        };
        (tokens.total() > 0).then_some(TurnUsage { tokens, models })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitStatus {
    Allowed,
    Warning,
    Rejected,
}

impl RateLimit {
    /// Read the Claude SDK's `SDKRateLimitInfo`: the limit it reports on,
    /// plus every window in `unifiedWindows`. The top-level `utilization` only
    /// comes once a warning threshold is passed; the windows carry it always.
    fn parse(info: &serde_json::Value) -> Vec<Self> {
        let status = |v: &serde_json::Value| match v.as_str()? {
            "allowed" => Some(LimitStatus::Allowed),
            "allowed_warning" => Some(LimitStatus::Warning),
            "rejected" => Some(LimitStatus::Rejected),
            _ => None,
        };
        // A fraction of the window, which can run past 1 when usage goes over the cap.
        let used = |v: &serde_json::Value| v.get("utilization").and_then(|u| u.as_f64()).map(|u| (u * 100.).max(0.));
        let resets_at = |v: &serde_json::Value| v.get("resetsAt").and_then(|r| r.as_f64()).map(|r| if r > 1e11 { r / 1000. } else { r } as i64);
        let top = info.get("status").and_then(status);
        let mut limits: Vec<Self> = Vec::new();
        if let (Some(status), Some(window)) = (top, info.get("rateLimitType").and_then(|w| w.as_str())) {
            limits.push(Self { window: window.to_string(), status, used: used(info), resets_at: resets_at(info) });
        }
        for (window, w) in info.get("unifiedWindows").and_then(|w| w.as_object()).into_iter().flatten() {
            match limits.iter_mut().find(|l| &l.window == window) {
                Some(l) => {
                    l.used = l.used.or(used(w));
                    l.resets_at = l.resets_at.or(resets_at(w));
                }
                None => limits.push(Self {
                    window: window.clone(),
                    status: w.get("status").and_then(status).unwrap_or(LimitStatus::Allowed),
                    used: used(w),
                    resets_at: resets_at(w),
                }),
            }
        }
        limits
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

/// Why a turn or a connection failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// The agent isn't signed in. `command` signs it in from a terminal, when Chat knows one.
    SignedOut { agent: String, command: Option<String> },
    Other(String),
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
    RateLimit(Vec<RateLimit>),
    /// The tokens the turn that's ending spent; sent just before `TurnEnded`.
    TurnUsage(TurnUsage),
    /// The slash commands the agent offers: name and description.
    Commands(Vec<(String, String)>),
    Title(String),
    /// The agent wants a decision before it goes on.
    Permission { id: u64, tool: String, title: String, answers: Vec<Answer> },
    /// The agent asks questions (a form) before it goes on.
    Ask { id: u64, message: String, questions: Vec<Question> },
    /// The turn is over; `Err` carries why it failed.
    TurnEnded(Result<Stop, Failure>),
    /// The steering request settled independently of the running turn.
    SteerEnded(Result<SteerOutcome, String>),
    /// The connection is gone, with the reason if it wasn't asked for.
    Closed(Option<Failure>),
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
    /// Answer an `Ask`; `None` skips it, leaving every question unanswered.
    Reply { id: u64, replies: Option<Vec<(String, Reply)>> },
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
    tool: Option<Vec<String>>,
) -> (UnboundedSender<Command>, UnboundedReceiver<Event>, impl Future<Output = ()> + Send + 'static) {
    let (command_tx, command_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();
    let run = async move {
        let result = run(spec.clone(), cwd, extra, previous, picks, tool, command_rx, event_tx.clone()).await;
        let _ = event_tx.unbounded_send(Event::Closed(result.err().map(|e| spec.failure(e))));
    };
    (command_tx, event_rx, run)
}

/// Added to the agent's system prompt: in this app a plan belongs in the
/// Context panel, which is fed by the agent's task-list tools, not in a file.
const PLANNING_PROMPT: &str = "You are running inside a chat app that shows your task list as the plan in its \
Context panel. When the user asks you to plan, make a plan, or create a plan, record it with your task-list tools \
(TodoWrite, or TaskCreate and TaskUpdate), one task per step, and keep their status current as you work. Do not \
write the plan to a Markdown or other file in the project unless the user explicitly asks for a file or document.

The chat draws images inline. To show the user an image file (a screenshot, a chart, a rendered design), write a \
Markdown image with its absolute path, such as ![chart](/abs/path/chart.png); PNG, JPEG, GIF, WebP, BMP and TIFF \
are drawn. Do this only when seeing the image helps the user, not for every image you touch.";

/// Session `_meta` for the agent. The Claude adapter appends
/// `systemPrompt.append` to Claude Code's own prompt on new, resumed and
/// loaded sessions alike.
fn session_meta(spec: &AgentSpec) -> Option<serde_json::Map<String, serde_json::Value>> {
    spec.uses_claude_adapter().then(|| {
        let mut meta = serde_json::Map::new();
        meta.insert("systemPrompt".into(), serde_json::json!({ "append": PLANNING_PROMPT }));
        meta
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    spec: AgentSpec,
    cwd: PathBuf,
    extra: Vec<PathBuf>,
    previous: Option<String>,
    picks: Vec<(String, String)>,
    tool: Option<Vec<String>>,
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) -> Result<(), agent_client_protocol::Error> {
    let path = shell_path();
    // Found here rather than at spawn, so a missing command is named along with
    // where it was looked for instead of surfacing as a bare "os error 2".
    let Some(command) = crate::lsp::which(&spec.command, path.as_deref()) else {
        let searched = path.or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        let message = format!("`{}` wasn't found on PATH ({searched})", spec.command);
        return Err(agent_client_protocol::Error::new(ErrorCode::INTERNAL_ERROR, message));
    };
    let mut config = AcpAgentConfig::new(command).args(spec.args.iter().cloned());
    if let Some(path) = path {
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
    // This app, started again as an MCP server, offers the agent its tools.
    let servers: Vec<McpServer> = tool
        .zip(std::env::current_exe().ok())
        .map(|(args, exe)| McpServer::Stdio(McpServerStdio::new("chat", exe).args(args)))
        .into_iter()
        .collect();
    let meta = session_meta(&spec);
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
    // And questions, likewise; both share one run of ids.
    let asking = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(0));
    // Loading a session replays it as updates; the transcript is already on
    // screen, so those are dropped until the load answers.
    let replaying = Arc::new(AtomicBool::new(false));
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
                let (events, waiting, next_id) = (events.clone(), waiting.clone(), next_id.clone());
                async move |request: RequestPermissionRequest, responder, _cx| {
                    let id = next_id.fetch_add(1, Ordering::SeqCst) + 1;
                    waiting.lock().unwrap().insert(id, responder);
                    let _ = events.unbounded_send(Event::Permission {
                        id,
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
        .on_receive_request(
            {
                let (events, asking, next_id) = (events.clone(), asking.clone(), next_id.clone());
                async move |request: CreateElicitationRequest, responder, _cx| {
                    let Some(questions) = questions(request.mode) else {
                        // Only forms are advertised; anything else can't be shown.
                        let _ = responder.respond(CreateElicitationResponse::new(ElicitationAction::Decline));
                        return Ok(());
                    };
                    let id = next_id.fetch_add(1, Ordering::SeqCst) + 1;
                    asking.lock().unwrap().insert(id, responder);
                    let _ = events.unbounded_send(Event::Ask { id, message: request.message, questions });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let ready = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_info(Implementation::new("chat", env!("CARGO_PKG_VERSION")))
                        // Forms are how the agent asks questions (Claude's AskUserQuestion).
                        .client_capabilities(
                            ClientCapabilities::new()
                                .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::new())),
                        ),
                )
                .block_task()
                .await?;
            let capabilities = &ready.agent_capabilities;
            let images = capabilities.prompt_capabilities.image;
            // Reopen the earlier session where the agent can: resume reattaches
            // quietly, load replays it.
            let reopened = match previous.clone() {
                Some(id) if capabilities.session_capabilities.resume.is_some() => connection
                    .send_request(ResumeSessionRequest::new(id.clone(), cwd.clone()).additional_directories(extra.clone()).mcp_servers(servers.clone()).meta(meta.clone()))
                    .block_task()
                    .await
                    .ok()
                    .map(|r| (id, r.config_options, r.modes)),
                Some(id) if capabilities.load_session => {
                    replaying.store(true, Ordering::SeqCst);
                    let loaded = connection.send_request(LoadSessionRequest::new(id.clone(), cwd.clone()).additional_directories(extra.clone()).mcp_servers(servers.clone()).meta(meta.clone())).block_task().await;
                    replaying.store(false, Ordering::SeqCst);
                    loaded.ok().map(|r| (id, r.config_options, r.modes))
                }
                _ => None,
            };
            let (session_id, options, modes, history) = match reopened {
                Some((id, options, modes)) => (id.into(), options, modes, History::Kept),
                None => {
                    let session = connection.send_request(NewSessionRequest::new(cwd).additional_directories(extra).mcp_servers(servers).meta(meta)).block_task().await?;
                    let history = if previous.is_some() { History::Lost } else { History::New };
                    (session.session_id, session.config_options, session.modes, history)
                }
            };
            let _ = events.unbounded_send(Event::Ready {
                agent: ready.agent_info.map(|i| i.title.unwrap_or(i.name)).unwrap_or_else(|| spec.name.clone()),
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
                        let request = PromptRequest::new(session_id.clone(), prompt(text, images));
                        let response = connection.send_request(request);
                        let (events, spec) = (events.clone(), spec.clone());
                        connection.spawn(async move {
                            let result = response.block_task().await;
                            // A turn stopped by a usage limit says which, and when it lifts.
                            if let Err(e) = &result
                                && let Some(info) = e.data.as_ref().and_then(|d| d.get("rateLimit"))
                            {
                                let _ = events.unbounded_send(Event::RateLimit(RateLimit::parse(info)));
                            }
                            if let Ok(response) = &result {
                                let usage = response.usage.as_ref().and_then(|u| serde_json::to_value(u).ok());
                                let meta = response.meta.as_ref().map(|m| serde_json::Value::Object(m.clone()));
                                if let Some(spent) = TurnUsage::parse(usage.as_ref(), meta.as_ref()) {
                                    let _ = events.unbounded_send(Event::TurnUsage(spent));
                                }
                            }
                            let _ = events.unbounded_send(Event::TurnEnded(match result {
                                Ok(response) => Ok(match response.stop_reason {
                                    StopReason::EndTurn => Stop::Finished,
                                    StopReason::Cancelled => Stop::Cancelled,
                                    StopReason::Refusal => Stop::Refused,
                                    _ => Stop::Limit,
                                }),
                                Err(e) => Err(spec.failure(e)),
                            }));
                            Ok(())
                        })?;
                    }
                    Command::Steer(text) => {
                        let params = serde_json::json!({
                            "sessionId": session_id,
                            "prompt": prompt(text, images),
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
                        for (_, responder) in asking.lock().unwrap().drain() {
                            let _ = responder.respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
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
                    Command::Reply { id, replies } => {
                        if let Some(responder) = asking.lock().unwrap().remove(&id) {
                            let action = match replies {
                                Some(replies) => {
                                    let content: BTreeMap<String, ElicitationContentValue> = replies
                                        .into_iter()
                                        .map(|(field, reply)| {
                                            (field, match reply {
                                                Reply::Text(text) => ElicitationContentValue::String(text),
                                                Reply::Picks(picks) => ElicitationContentValue::StringArray(picks),
                                                Reply::Yes(yes) => ElicitationContentValue::Boolean(yes),
                                                Reply::Integer(n) => ElicitationContentValue::Integer(n),
                                                Reply::Number(n) => ElicitationContentValue::Number(n),
                                            })
                                        })
                                        .collect();
                                    ElicitationAction::Accept(ElicitationAcceptAction::new().content(content))
                                }
                                None => ElicitationAction::Decline,
                            };
                            let _ = responder.respond(CreateElicitationResponse::new(action));
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
        // Images reach the transcript as Markdown, which draws local ones.
        ContentBlock::Image(i) => {
            let path = save_image(&i.data, &i.mime_type)?;
            Some(format!("\n\n![image](<{}>)\n\n", path.display()))
        }
        ContentBlock::ResourceLink(link) if is_image(&link.uri) => Some(format!("\n\n![{}](<{}>)\n\n", link.name, link.uri)),
        _ => None,
    }
}

fn is_image(uri: &str) -> bool {
    let ext = uri.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff")
}

/// Write an agent's image to disk, named by its contents so a resumed thread
/// that replays it finds the same file.
fn save_image(data: &str, mime: &str) -> Option<PathBuf> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => return None,
    };
    store_image(&bytes, ext)
}

/// Where images in threads are kept: the agent's, and ones pasted in.
fn images_dir() -> PathBuf {
    crate::settings::dir().unwrap_or_else(std::env::temp_dir).join("images")
}

/// Keep an image in the images folder, named by its contents.
pub fn store_image(bytes: &[u8], ext: &str) -> Option<PathBuf> {
    store_image_in(&images_dir(), bytes, ext)
}

fn store_image_in(dir: &Path, bytes: &[u8], ext: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{:016x}.{ext}", hash.finish()));
    if !path.exists() {
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

/// Where an image link points on this machine: an absolute path, `~/…` or
/// `file://…`. Only images GPUI can draw count; other files stay links.
pub fn local_image(url: &str) -> Option<PathBuf> {
    let url = url.strip_prefix('<').and_then(|u| u.strip_suffix('>')).unwrap_or(url);
    let path = if let Some(rest) = url.strip_prefix("file://") {
        PathBuf::from(rest.replace("%20", " "))
    } else if let Some(rest) = url.strip_prefix("~/") {
        std::env::home_dir()?.join(rest)
    } else {
        PathBuf::from(url)
    };
    (path.is_absolute() && is_image(&path.to_string_lossy())).then_some(path)
}

/// Images past this size are left where they are rather than copied.
const MAX_KEPT_IMAGE: u64 = 32 * 1024 * 1024;

/// `text` with the local images it shows copied into the images folder and
/// pointed at there, so a screenshot an agent left in `/tmp` still shows
/// after a restart clears it. `None` when nothing needed copying.
pub fn keep_images(text: &str) -> Option<String> {
    keep_images_in(text, &images_dir())
}

fn keep_images_in(text: &str, dir: &Path) -> Option<String> {
    if !text.contains("![") {
        return None;
    }
    let markdown::mdast::Node::Root(root) = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).ok()? else {
        return None;
    };
    fn walk(node: &markdown::mdast::Node, dir: &Path, kept: &mut Vec<(usize, usize, String)>) {
        if let markdown::mdast::Node::Image(i) = node
            && let Some(pos) = &i.position
            && let Some(path) = local_image(&i.url)
            && !path.starts_with(dir)
            && std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MAX_KEPT_IMAGE)
            && let Ok(bytes) = std::fs::read(&path)
            && let Some(copy) = store_image_in(dir, &bytes, &path.extension().unwrap_or_default().to_string_lossy().to_ascii_lowercase())
        {
            let alt = i.alt.replace(['[', ']'], "");
            kept.push((pos.start.offset, pos.end.offset, format!("![{alt}](<{}>)", copy.display())));
        }
        for child in node.children().into_iter().flatten() {
            walk(child, dir, kept);
        }
    }
    let mut kept = Vec::new();
    for child in &root.children {
        walk(child, dir, &mut kept);
    }
    if kept.is_empty() {
        return None;
    }
    let mut out = text.to_string();
    kept.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
    for (start, end, link) in kept {
        out.replace_range(start..end, &link);
    }
    Some(out)
}

/// A message as the agent gets it: the text, then each pasted image it
/// mentions (`@` and a path in the images folder) as the image itself, for
/// agents that take images. The mention stays in the text so the agent can
/// still refer to the file.
fn prompt(text: String, images: bool) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if images {
        let marker = format!("@{}/", images_dir().display());
        let mut seen = Vec::new();
        for (at, _) in text.match_indices(&marker) {
            let rest = &text[at + 1..];
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let path = PathBuf::from(&rest[..end]);
            if seen.contains(&path) {
                continue;
            }
            if let Some(block) = image_block(&path) {
                blocks.push(block);
            }
            seen.push(path);
        }
    }
    blocks.insert(0, ContentBlock::Text(TextContent::new(text)));
    blocks
}

fn image_block(path: &Path) -> Option<ContentBlock> {
    use base64::Engine;
    let mime = match path.extension()?.to_str()? {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    let bytes = std::fs::read(path).ok()?;
    Some(ContentBlock::Image(ImageContent::new(base64::engine::general_purpose::STANDARD.encode(bytes), mime)))
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
            let limits = usage.meta.as_ref().and_then(|m| m.get("_claude/rateLimit")).map(RateLimit::parse);
            match limits {
                Some(limits) => Event::RateLimit(limits),
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
    fn local_images_are_kept_in_the_images_folder() {
        let root = std::env::temp_dir().join(format!("chat-keep-{}", std::process::id()));
        let (tmp, kept) = (root.join("tmp"), root.join("images"));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("shot.png"), b"\x89PNGshot").unwrap();
        let text = format!(
            "Before:\n\n![before](<{}>)\n\n![gone](/nowhere/x.png) ![remote](https://x.dev/a.png)",
            tmp.join("shot.png").display()
        );
        let out = keep_images_in(&text, &kept).unwrap();
        let copy = std::fs::read_dir(&kept).unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read(&copy).unwrap(), b"\x89PNGshot");
        assert_eq!(
            out,
            format!("Before:\n\n![before](<{}>)\n\n![gone](/nowhere/x.png) ![remote](https://x.dev/a.png)", copy.display())
        );
        assert_eq!(keep_images_in(&out, &kept), None, "a kept image stays put");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rate_limit_reads_claude_info() {
        let limits = RateLimit::parse(&serde_json::json!({
            "status": "allowed_warning", "rateLimitType": "five_hour", "utilization": 0.82, "resetsAt": 1_800_000_000
        }));
        let [limit] = limits.as_slice() else { panic!("{limits:?}") };
        assert_eq!(limit.status, LimitStatus::Warning);
        assert_eq!(limit.short_name(), "5h");
        assert_eq!(limit.used.map(|u| u.round()), Some(82.));
        assert_eq!(limit.resets_at, Some(1_800_000_000));

        let weekly = RateLimit::parse(&serde_json::json!({ "status": "rejected", "rateLimitType": "seven_day_opus", "resetsAt": 1_800_000_000_000u64 }));
        let [weekly] = weekly.as_slice() else { panic!("{weekly:?}") };
        assert_eq!((weekly.short_name().as_str(), weekly.long_name().as_str()), ("Opus week", "Opus weekly limit"));
        assert_eq!(weekly.resets_at, Some(1_800_000_000));
        assert!(limit.rank() < weekly.rank());

        assert_eq!(RateLimit::parse(&serde_json::json!({ "status": "allowed" })), vec![]);
    }

    #[test]
    fn rate_limit_reads_every_window() {
        // The usual event: no top-level utilization, the percentages in the windows.
        let limits = RateLimit::parse(&serde_json::json!({
            "status": "allowed", "rateLimitType": "five_hour", "resetsAt": 1_800_000_000,
            "unifiedWindows": {
                "five_hour": { "utilization": 0.42, "resetsAt": 1_800_000_000 },
                "seven_day": { "utilization": 1.05, "resetsAt": 1_800_400_000, "status": "allowed_warning" }
            }
        }));
        let five = limits.iter().find(|l| l.window == "five_hour").unwrap();
        assert_eq!((five.status, five.used.map(|u| u.round())), (LimitStatus::Allowed, Some(42.)));
        let week = limits.iter().find(|l| l.window == "seven_day").unwrap();
        assert_eq!((week.status, week.used.map(|u| u.round()), week.resets_at), (LimitStatus::Warning, Some(105.), Some(1_800_400_000)));
        assert_eq!(limits.len(), 2);
    }

    #[test]
    fn rate_limit_usage_update_becomes_its_own_event() {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "usage_update", "used": 10, "size": 100,
            "_meta": { "_claude/rateLimit": { "status": "allowed", "rateLimitType": "five_hour", "utilization": 0.1 } }
        }))
        .unwrap();
        assert!(matches!(translate(update), Some(Event::RateLimit(limits)) if limits[0].status == LimitStatus::Allowed));
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
    fn a_signed_out_agent_is_told_apart_with_its_sign_in_command() {
        let [claude, codex] = AgentSpec::defaults().try_into().unwrap();
        let refused = || agent_client_protocol::Error::from(ErrorCode::AUTH_REQUIRED);
        let Failure::SignedOut { agent, command } = claude.failure(refused()) else { panic!() };
        assert_eq!((agent.as_str(), command.as_deref()), ("Claude Code", Some("npx -y @agentclientprotocol/claude-agent-acp --cli auth login")));
        assert!(matches!(codex.failure(refused()), Failure::SignedOut { command: Some(c), .. } if c.contains("codex login")));
        let other = AgentSpec::new("Mock", "python3", &["mock-agent.py"]);
        assert_eq!(other.failure(refused()), Failure::SignedOut { agent: "Mock".into(), command: None });
        let broken = agent_client_protocol::Error::from(ErrorCode::INTERNAL_ERROR);
        assert!(matches!(claude.failure(broken), Failure::Other(_)));
    }

    #[test]
    fn reads_claudes_questions_from_a_form() {
        let request: CreateElicitationRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "toolCallId": "t", "mode": "form", "message": "Pick",
            "requestedSchema": { "type": "object", "required": ["question_1"], "properties": {
                "question_0": { "type": "string", "title": "Db", "oneOf": [{ "const": "pg", "title": "Postgres", "description": "SQL" }] },
                "question_0_custom": { "type": "string", "title": "Other", "description": "Type your own" },
                "question_1": { "type": "array", "description": "Which?", "items": { "anyOf": [{ "const": "a", "title": "A" }] } },
                "question_2": { "type": "boolean" },
                "question_3": { "type": "integer" },
            }},
        }))
        .unwrap();
        let questions = questions(request.mode).unwrap();
        let kinds: Vec<_> = questions.iter().map(|q| (q.field.as_str(), &q.kind, q.required)).collect();
        let pg = Pick { value: "pg".into(), label: "Postgres".into(), description: Some("SQL".into()) };
        let a = Pick { value: "a".into(), label: "A".into(), description: None };
        assert_eq!(kinds, [
            ("question_0", &QuestionKind::Single(vec![pg]), false),
            ("question_0_custom", &QuestionKind::Text, false),
            ("question_1", &QuestionKind::Multi(vec![a]), true),
            ("question_2", &QuestionKind::Yes, false),
            ("question_3", &QuestionKind::Number { integer: true }, false),
        ]);
        assert_eq!(questions[1].prompt.as_deref(), Some("Type your own"));
    }

    #[test]
    fn answers_the_mock_agents_question() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Vec::new(), None, Vec::new(), None);
        std::thread::spawn(move || block_on(run));
        let mut next = move || block_on(events.next()).expect("connection closed early");
        for skip in [false, true] {
            commands.unbounded_send(Command::Prompt("ask me a question".into())).unwrap();
            let mut said = String::new();
            loop {
                match next() {
                    Event::Ask { id, message, questions } => {
                        assert_eq!(message, "Which database?");
                        assert!(matches!(&questions[0].kind, QuestionKind::Single(p) if p.len() == 2));
                        let replies = (!skip).then(|| vec![
                            ("question_0".to_string(), Reply::Text("Redis".into())),
                            ("question_0_custom".to_string(), Reply::Text("for caching".into())),
                        ]);
                        commands.unbounded_send(Command::Reply { id, replies }).unwrap();
                    }
                    Event::Text(t) => said.push_str(&t),
                    Event::TurnEnded(result) => {
                        assert_eq!(result, Ok(Stop::Finished));
                        break;
                    }
                    Event::Closed(why) => panic!("closed: {why:?}"),
                    _ => {}
                }
            }
            let expected = if skip {
                r#"Answer: {"action": "decline"}"#
            } else {
                r#"Answer: {"action": "accept", "content": {"question_0": "Redis", "question_0_custom": "for caching"}}"#
            };
            assert_eq!(said.trim(), expected);
        }
    }

    #[test]
    fn talks_to_the_mock_agent() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Vec::new(), None, Vec::new(), None);
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

        commands.unbounded_send(Command::Prompt("sign in".into())).unwrap();
        assert_eq!(next(), Event::TurnEnded(Err(Failure::SignedOut { agent: "Mock".into(), command: None })));
    }

    /// Reopening a session keeps it, and its replay never reaches the thread.
    #[test]
    fn reopens_a_session_without_its_replay() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let spec = AgentSpec::new("Mock", "python3", &[script]);
        let (commands, mut events, run) = connect(spec, std::env::temp_dir(), Vec::new(), Some("mock-7".into()), Vec::new(), None);
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
