//! The window's state: which project and thread are showing, the open tabs and
//! the panels around them. Views live in the submodules.
mod chat;
mod commands;
mod dashboard;
mod effort;
mod file_kind;
mod find;
mod chrome;
mod md;
mod media;
mod mentions;
mod missions;
mod new_project;
mod panes;
mod retro_mac;
mod preferences;
mod remote;
mod search;
mod select;
mod side;
mod symbols;
mod lsp;
mod servers;
mod git_panel;
mod syntax;
mod terminal;
mod usage;
pub use terminal::init_terminal_keys;
mod ui;
#[cfg(test)]
mod e2e;

use crate::{
    agent::{self, Command, Event, Role, Setting},
    store,
    menu::{self as popover, Menu},
    model::{Block, Project, Session, Thread, ThreadState, tilde},
    settings::{ActivityMode, FollowUpMode, ProjectPalette, Settings, ThreadSort},
    theme::{Palette, mono, palette},
};
use futures::StreamExt;
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{input::InputState, resizable::ResizableState};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

actions!(
    chat,
    [
        Quit,
        ToggleThreads,
        ToggleSide,
        NewThread,
        AddProject,
        CloseTab,
        NextTab,
        PreviousTab,
        Submit,
        ShowContext,
        ShowChanges,
        ShowFiles,
        OpenTerminal,
        OpenPreferences,
        AddToChat,
        GoToDefinition,
        FindReferences,
        GoBack,
        GoForward,
        AskUp,
        AskDown,
        AskNext,
        AskPrevious,
        AskPick,
        AskSubmit,
        AskSkip,
        FindInThread,
        FindNext,
        FindPrevious,
        CloseFind,
        FindInFiles,
        CloseFileFind,
        OpenFound,
        FoundUp,
        FoundDown,
    ]
);

#[derive(Clone, Debug, PartialEq)]
pub enum TabKind {
    Chat,
    File(PathBuf),
    Diff(PathBuf),
    /// One side of a file's change, from the Git panel: staged (HEAD → index) or not.
    GitDiff(PathBuf, bool),
    /// A shell, by the folder it started in: one per project folder.
    Terminal(PathBuf),
    /// A language server's log, by its name and root.
    ServerLog(String, PathBuf),
}

pub struct Tab {
    pub id: usize,
    pub kind: TabKind,
    /// Held at the start of the strip beside the chat, and not closed with the rest.
    pub pinned: bool,
}
impl Tab {
    /// The chat always stays; a pinned tab stays until it's unpinned.
    pub fn fixed(&self) -> bool {
        self.pinned || self.kind == TabKind::Chat
    }
}

/// The right panel's pages.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Context,
    Changes,
    Files,
}
impl Side {
    pub const ALL: [Side; 3] = [Side::Context, Side::Changes, Side::Files];
    pub fn title(self) -> &'static str {
        match self {
            Side::Context => "Context",
            Side::Changes => "Changes",
            Side::Files => "Files",
        }
    }
}

/// What a project had showing when you left it, put back when you return. Its
/// terminals are kept here too, so their shells go on running in the background.
struct ProjectView {
    tabs: Vec<Tab>,
    active: usize,
    expanded: HashSet<PathBuf>,
    terminals: Terminals,
    /// The thread that was showing, by id: indexes shift as threads are added.
    thread: Option<usize>,
    /// The folder picked on the Changes page.
    git_repo: Option<PathBuf>,
}

/// Files dragged over the Files page: the folder they'd land in, and the
/// folder row under the pointer, which opens if they rest on it.
#[derive(Clone, PartialEq)]
struct FileDrop {
    target: PathBuf,
    over: Option<PathBuf>,
}

/// How long dragged files rest on a closed folder before it opens.
const SPRING_DELAY: Duration = Duration::from_millis(600);

/// Shells by the folder each started in, or why one couldn't start.
type Terminals = HashMap<PathBuf, Result<Entity<terminal::TerminalView>, SharedString>>;

/// A file or folder in the Files page.
#[derive(Clone, PartialEq)]
pub struct Entry {
    pub name: SharedString,
    pub path: PathBuf,
    pub dir: bool,
    /// Git ignores it, or a folder it's in: shown faded.
    pub ignored: bool,
}

#[derive(Clone)]
struct FileMention {
    token: String,
    label: String,
    /// What the agent receives in the mention's place: a path, or a snippet.
    sent: String,
    /// A one-line reference kept in the transcript's chip.
    reference: String,
    /// The icon drawn in the chip, by kind of file.
    icon: SharedString,
}

/// Folders that are never worth walking into.
const SKIPPED: &[&str] = &[".git", ".DS_Store", "node_modules", "target"];
/// Agent processes kept running at once. Past this, the least recently shown
/// idle thread's agent is stopped; its session reopens when it is shown again.
const LIVE_SESSIONS: usize = 4;
/// The tab strip's terminal button, where the folder picker opens.
const TERMINAL_BUTTON: &str = "open-terminal";
/// Files larger than this aren't opened in a tab.
const MAX_FILE: u64 = 2 * 1024 * 1024;

pub struct Workspace {
    focus: FocusHandle,
    /// Whether the window has focus: a thread that finishes while it doesn't
    /// hasn't been seen, even if it's the one showing.
    window_active: bool,
    settings: Settings,
    projects: Vec<Project>,
    project: usize,
    thread: usize,
    next_thread: usize,
    /// The chat tab is always first and can't be closed.
    tabs: Vec<Tab>,
    active: usize,
    next_tab: usize,
    /// The tab strip's scroll, and the tab it last brought into view (by id),
    /// so a newly active tab scrolls in once without fighting the wheel after.
    tabs_scroll: ScrollHandle,
    tab_in_view: std::cell::Cell<Option<usize>>,
    side: Side,
    threads_open: bool,
    side_open: bool,
    /// Folders open in the Files page.
    expanded: HashSet<PathBuf>,
    /// Folder listings, read when a folder is opened and again whenever git
    /// looks (the watcher, the window coming forward), for those showing.
    listings: HashMap<PathBuf, Vec<Entry>>,
    /// Bumped per re-read of the listings, so a slow one can't undo a newer one.
    listings_revision: u64,
    /// The lines of each file open in a tab, or why it couldn't be shown.
    files: HashMap<PathBuf, Result<Rc<FileView>, SharedString>>,
    /// Open images and videos, shown as pictures rather than lines.
    media: HashMap<PathBuf, media::Media>,
    /// Where each open file is scrolled, so a link can jump to a line.
    file_scroll: HashMap<PathBuf, UniformListScrollHandle>,
    /// Each open file's changes since HEAD and blame, read in the background.
    file_git: HashMap<PathBuf, Rc<crate::git::FileInfo>>,
    /// Changes in each open file whose replaced lines show above them, by
    /// the first line of the change.
    hunks_open: HashMap<PathBuf, HashSet<usize>>,
    /// The row last clicked in each file or diff pane, where its blame shows.
    cursor_line: HashMap<panes::LineSource, usize>,
    /// Blame for each diff pane's two sides, read in the background.
    diff_blame: RefCell<HashMap<panes::LineSource, panes::DiffBlame>>,
    /// File views show who last changed the cursor's line.
    show_blame: bool,
    /// Lines a link jumped to, lit briefly so the eye finds them.
    flash: Option<Flash>,
    /// The project's shells, by the folder each started in.
    terminals: Terminals,
    /// Projects not showing, by index, as they were left.
    views: HashMap<usize, ProjectView>,
    thread_search: Entity<InputState>,
    /// The find bar over the transcript (⌘ F), and whether it's showing.
    thread_find: Entity<InputState>,
    thread_find_open: bool,
    /// The search box atop the Files page (⌘ ⇧ O), and whether it's showing.
    file_find: Entity<InputState>,
    file_find_open: bool,
    /// The file picked among those found, and the list's scroll.
    file_find_selected: usize,
    file_find_scroll: ScrollHandle,
    /// What the last content search found, once it's done.
    thread_hits: Option<search::Results>,
    /// The search under way: for which project and query.
    thread_searching: Option<(usize, String, Task<()>)>,
    composer: Entity<InputState>,
    /// Dropped files displayed as basename chips in the composer. Their paths
    /// are substituted only in the prompt sent to the agent.
    file_mentions: Rc<RefCell<Vec<FileMention>>>,
    command_index: usize,
    command_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    /// The right panel's page (context or files) and the Git changes list.
    side_scroll: ScrollHandle,
    git_changes_scroll: ScrollHandle,
    commands_dismissed: bool,
    /// Empty the composer on the next render, outside the action that sent it.
    clear_composer: bool,
    transcript: ScrollHandle,
    /// Keep the transcript pinned to its end as output streams in. Scrolling up
    /// lets go; scrolling back to the end, sending, or switching threads takes hold again.
    follow: bool,
    /// While a press is down in the transcript (on its scrollbar, say), where the
    /// view sat when it began. New output waits to pin until it lifts.
    holding_transcript: Option<Pixels>,
    thread_panels: Entity<ResizableState>,
    side_panels: Entity<ResizableState>,
    popup: Option<Entity<Menu>>,
    popup_position: Point<Pixels>,
    /// The popover's corner that sits at `popup_position`: menus low in the window open upward.
    popup_corner: Corner,
    popup_anchor: Option<&'static str>,
    /// The trigger whose menu a mouse-down just closed, so its click doesn't reopen it.
    dismissed: Option<(&'static str, Instant)>,
    /// Where each popover trigger was last painted.
    anchors: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
    prefs_open: bool,
    prefs_focus: FocusHandle,
    /// The settings page showing, the search across them, and the page's scroll.
    prefs_section: preferences::Section,
    prefs_search: Entity<InputState>,
    prefs_page_scroll: ScrollHandle,
    /// Installed font families, listed once when a font menu first opens.
    font_names: Rc<std::cell::OnceCell<Vec<String>>>,
    /// Instruction files found for each project (by folder; `None` for chats).
    instructions: HashMap<Option<PathBuf>, Vec<(PathBuf, &'static str)>>,
    /// Tool calls opened to show their full command and output, by thread and block.
    open_tools: HashSet<(usize, usize)>,
    /// Finished turns whose folded activity is shown, by thread and the turn's closing block.
    open_summaries: HashSet<(usize, usize)>,
    /// The last thought line shown in each collapsed thought block. Entries are
    /// created only after hovering, so a growing block otherwise follows its
    /// newest line automatically.
    thought_selection: HashMap<(usize, usize), usize>,
    /// Threads with a running agent, least recently shown first.
    live: Vec<usize>,
    /// A save is already scheduled.
    saving: bool,
    /// Where threads are saved; `None` when the database couldn't be opened,
    /// and nothing is.
    store: Option<store::Store>,
    git_snapshots: HashMap<PathBuf, Result<crate::git::Snapshot, String>>,
    git_diffs: HashMap<(PathBuf, bool), Result<crate::git::Diff, String>>,
    git_refreshing: bool,
    /// How long the last refresh took to read; file events wait about that
    /// long again, so a build in an unignored folder can't keep git running nonstop.
    git_refresh_cost: Duration,
    /// Something changed during a refresh: read again once it lands.
    git_queued: bool,
    git_watch: Option<crate::git_watch::Watcher>,
    git_busy: bool,
    git_revision: usize,
    git_errors: HashMap<PathBuf, String>,
    /// Worktrees picked in the Changes page, by thread and project folder. They
    /// win over what the thread's agent reported until it reports again.
    git_picks: HashMap<(Option<usize>, PathBuf), PathBuf>,
    commit_message: Entity<InputState>,
    clear_commit_message: Option<String>,
    /// An undone commit's message, to put back in an empty commit box.
    restore_commit_message: Option<String>,
    /// An agent writing a commit message for this folder's changes.
    commit_writing: Option<(PathBuf, Task<()>)>,
    /// A message an agent wrote, for the folder it was written for.
    written_commit_message: Option<(PathBuf, String)>,
    /// Which Git operation is running, so its button can say so.
    git_running: Option<GitRunning>,
    /// The stash a first click on Drop armed; a second click drops it.
    stash_drop_armed: Option<usize>,
    /// The section, staged or not, a first click on Discard all armed; a
    /// second click discards.
    discard_armed: Option<bool>,
    /// The commit button amends the last commit instead.
    amend: bool,
    /// Files of the last commit unticked while amending, to take out of it.
    amend_drop: HashSet<PathBuf>,
    /// The repository the Changes page shows, when a project spans several folders.
    git_repo: Option<PathBuf>,
    /// Text selected in the transcript.
    selection: select::Shared,
    /// Lines selected in a file or diff pane.
    line_selection: Option<panes::LineSelection>,
    /// Text selected in a file or diff pane, and which pane it is in.
    code_text: select::Shared,
    code_text_source: Option<panes::LineSource>,
    /// The word clicked in a file or diff pane, marked wherever it appears there.
    symbol: Option<symbols::SymbolMark>,
    /// Language servers for the files open in tabs.
    servers: lsp::Servers,
    /// The reasoning effort slider is showing.
    effort_open: bool,
    /// The usage widget is open while this redraws it.
    usage_tick: Option<Task<()>>,
    /// The dashboard, while it shows in place of the threads and tabs.
    dashboard: Option<dashboard::Dashboard>,
    /// Where the effort slider's track was last painted, to map the pointer to a step.
    effort_track: Rc<std::cell::Cell<Bounds<Pixels>>>,
    effort_dragging: bool,
    /// The project being renamed, and the field its new name is typed in.
    renaming: Option<usize>,
    rename_input: Entity<InputState>,
    /// The project being added, while its popover is open: the folders picked so far.
    new_project: Option<Vec<PathBuf>>,
    new_project_input: Entity<InputState>,
    /// The text fields of questions still waiting, by thread, question set and field.
    ask_inputs: HashMap<(usize, u64, String), Entity<InputState>>,
    /// Why a set of questions couldn't be sent yet, by thread and question set.
    ask_errors: HashMap<(usize, u64), String>,
    /// The waiting question card takes keys through this.
    ask_focus: FocusHandle,
    /// Where the keys are in that card: thread, question set, question, choice.
    ask_cursor: Option<(usize, u64, usize, usize)>,
    /// A question just arrived on the showing thread; the next frame moves the keys to it.
    focus_ask: bool,
    rename_position: Point<Pixels>,
    /// The thread the composer's text belongs to, and what other threads had
    /// typed when they were left: text and the files mentioned in it.
    draft_owner: Option<usize>,
    drafts: HashMap<usize, (String, Vec<FileMention>)>,
    restore_draft: bool,
    /// A project's commit message and thread search, kept while it isn't showing.
    project_inputs: HashMap<usize, (String, String)>,
    restore_project_inputs: bool,
    /// Files and symbols to mention with `@`, for the current project.
    mention_index: Option<Rc<mentions::Index>>,
    mention_indexing: bool,
    mention_selected: usize,
    mention_scroll: ScrollHandle,
    mention_dismissed: bool,
    /// Code views scrolled sideways, by view: their line numbers stay put.
    sideways: Rc<RefCell<HashMap<SharedString, panes::Sideways>>>,
    /// The commit box was resized and the new height isn't saved yet; it is
    /// when the drag ends.
    commit_height_unsaved: bool,
    /// While a project is dragged along the rail: which one, and the gap it
    /// would drop into, counted as the index of the project below that gap.
    drop_slot: Option<(usize, usize)>,
    /// Where files dragged from Finder over the Files page would land.
    file_drop: Option<FileDrop>,
    /// What the rows under the pointer said on the last drag move, before it's
    /// taken up: the folder it would land in, and the folder row under it.
    file_drop_probe: Rc<RefCell<Option<(PathBuf, Option<PathBuf>)>>>,
    /// The usage limits each agent (by index) last reported, by window. They
    /// belong to the account, so any of the agent's threads updates them.
    limits: HashMap<usize, HashMap<String, agent::RateLimit>>,
    /// Other apps a file can be opened in, found once at startup.
    editors: Rc<crate::editors::Editors>,
    /// The mission in the mailbox and the streak; for the app, not a project.
    missions: crate::missions::Missions,
    /// What the mission card shows beyond the mission itself.
    mission_ui: missions::MissionUi,
    /// Phones paired to reach this Mac, and the ones connected.
    remote: remote::RemoteState,
    _quit: Option<Subscription>,
}

/// A Git operation in progress, as far as the commit box cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitRunning {
    Commit,
    Push,
    Undo,
    Stash,
    Other,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let thread_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
        let file_mentions = Rc::new(RefCell::new(Vec::new()));
        let composer_mentions = file_mentions.clone();
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 12)
                .placeholder("Ask for a change, or how something works…")
        });
        composer.update(cx, |input, _| {
            input.lsp.document_color_provider = Some(Rc::new(chat::FileMentionColors::new(composer_mentions)));
        });
        cx.observe(&thread_search, |s, _, cx| {
            s.search_threads(cx);
            cx.notify();
        })
        .detach();
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();
        let thread_find = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
        cx.subscribe(&thread_find, |s, _, e: &gpui_component::input::InputEvent, cx| {
            if let gpui_component::input::InputEvent::Change = e {
                s.sync_thread_find(true, cx);
            }
        })
        .detach();
        let file_find = cx.new(|cx| InputState::new(window, cx).placeholder("Go to file…"));
        cx.subscribe(&file_find, |s, _, e: &gpui_component::input::InputEvent, cx| {
            if let gpui_component::input::InputEvent::Change = e {
                s.file_query_changed(cx);
            }
        })
        .detach();
        let prefs_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings…"));
        cx.observe(&prefs_search, |_, _, cx| cx.notify()).detach();
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        cx.subscribe_in(&rename_input, window, |s, _, e: &gpui_component::input::InputEvent, window, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = e {
                s.finish_rename(true, window, cx);
            }
        })
        .detach();
        let new_project_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        cx.subscribe_in(&new_project_input, window, |s, _, e: &gpui_component::input::InputEvent, window, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = e {
                s.create_new_project(window, cx);
            }
        })
        .detach();
        let settings = Settings::load();
        let commit_message = git_panel::commit_input(settings.commit_box_height.is_some(), window, cx);
        let mut projects = vec![Project::chats()];
        projects.extend(settings.projects.iter().filter(|p| p.path.is_dir()).cloned().map(Project::open));
        let mut next_thread = 0;
        let store = store::Store::open();
        if let Some(store) = &store {
            for project in &mut projects {
                project.threads = store.load(project, &settings.agents, settings.agent, &mut next_thread);
            }
        }
        let mut workspace = Self {
            focus: cx.focus_handle(),
            // Open on the first real project when there is one.
            project: projects.len().min(2) - 1,
            settings,
            projects,
            thread: 0,
            next_thread,
            tabs: vec![Tab { id: 0, kind: TabKind::Chat, pinned: false }],
            active: 0,
            next_tab: 1,
            side: Side::Context,
            threads_open: true,
            side_open: true,
            expanded: HashSet::new(),
            listings: HashMap::new(),
            listings_revision: 0,
            files: HashMap::new(),
            media: HashMap::new(),
            file_scroll: HashMap::new(),
            file_git: HashMap::new(),
            hunks_open: HashMap::new(),
            cursor_line: HashMap::new(),
            diff_blame: RefCell::new(HashMap::new()),
            show_blame: true,
            flash: None,
            terminals: HashMap::new(),
            views: HashMap::new(),
            thread_search,
            thread_find,
            thread_find_open: false,
            file_find,
            file_find_open: false,
            file_find_selected: 0,
            file_find_scroll: ScrollHandle::new(),
            thread_hits: None,
            thread_searching: None,
            composer,
            file_mentions,
            command_index: 0,
            tabs_scroll: ScrollHandle::new(),
            tab_in_view: std::cell::Cell::new(None),
            command_scroll: ScrollHandle::new(),
            thread_scroll: ScrollHandle::new(),
            side_scroll: ScrollHandle::new(),
            git_changes_scroll: ScrollHandle::new(),
            commands_dismissed: false,
            clear_composer: false,
            transcript: ScrollHandle::new(),
            follow: true,
            holding_transcript: None,
            thread_panels: cx.new(|_| ResizableState::default()),
            side_panels: cx.new(|_| ResizableState::default()),
            popup: None,
            popup_position: Point::default(),
            popup_corner: Corner::TopLeft,
            popup_anchor: None,
            dismissed: None,
            anchors: Rc::default(),
            prefs_open: false,
            prefs_focus: cx.focus_handle(),
            prefs_section: Default::default(),
            prefs_search,
            prefs_page_scroll: ScrollHandle::new(),
            font_names: Rc::default(),
            instructions: HashMap::new(),
            open_tools: HashSet::new(),
            open_summaries: HashSet::new(),
            thought_selection: HashMap::new(),
            live: Vec::new(),
            saving: false,
            git_snapshots: HashMap::new(),
            git_diffs: HashMap::new(),
            git_refreshing: false,
            git_refresh_cost: Duration::ZERO,
            git_queued: false,
            git_watch: None,
            git_busy: false,
            git_revision: 0,
            git_errors: HashMap::new(),
            git_picks: HashMap::new(),
            commit_message,
            clear_commit_message: None,
            restore_commit_message: None,
            commit_writing: None,
            written_commit_message: None,
            git_running: None,
            stash_drop_armed: None,
            discard_armed: None,
            amend: false,
            amend_drop: HashSet::new(),
            git_repo: None,
            selection: Rc::default(),
            line_selection: None,
            code_text: Rc::default(),
            code_text_source: None,
            symbol: None,
            servers: lsp::Servers::default(),
            effort_open: false,
            usage_tick: None,
            dashboard: None,
            effort_track: Rc::default(),
            effort_dragging: false,
            renaming: None,
            rename_input,
            new_project: None,
            new_project_input,
            ask_inputs: HashMap::new(),
            ask_errors: HashMap::new(),
            ask_focus: cx.focus_handle(),
            ask_cursor: None,
            focus_ask: false,
            rename_position: Point::default(),
            draft_owner: None,
            drafts: HashMap::new(),
            restore_draft: false,
            project_inputs: HashMap::new(),
            restore_project_inputs: false,
            mention_index: None,
            mention_indexing: false,
            mention_selected: 0,
            mention_scroll: ScrollHandle::new(),
            mention_dismissed: false,
            sideways: Rc::default(),
            drop_slot: None,
            file_drop: None,
            file_drop_probe: Rc::default(),
            limits: HashMap::new(),
            commit_height_unsaved: false,
            window_active: window.is_window_active(),
            editors: Rc::default(),
            missions: crate::missions::Missions::from_saved(store.as_ref().map(|s| s.load_missions()).unwrap_or_default()),
            mission_ui: missions::MissionUi::new(cx),
            store,
            remote: remote::RemoteState::new(),
            _quit: None,
        };
        cx.spawn(async move |this, cx| {
            let editors = cx.background_executor().spawn(async { crate::editors::load() }).await;
            let _ = this.update(cx, |s, cx| {
                s.editors = Rc::new(editors);
                cx.notify();
            });
        })
        .detach();
        // Whatever hasn't been written yet is written on the way out.
        workspace._quit = Some(cx.on_app_quit(|s, _| {
            s.save_now();
            s.lsp_shutdown();
            async {}
        }));
        crate::theme::set_fonts(
            workspace.settings.ui_font.as_deref(),
            workspace.settings.mono_font.as_deref(),
            workspace.settings.ui_font_size,
            workspace.settings.mono_font_size,
            cx,
        );
        syntax::set_indentation(
            workspace.settings.tab_width.unwrap_or(syntax::DEFAULT_TAB_WIDTH),
            workspace.settings.indent_guides.unwrap_or(true),
        );
        // Before any agent starts, so each one is offered the worktree tool.
        workspace.listen_for_worktrees(cx);
        // Reopen where things were left: the project's latest thread.
        if workspace.current_project().threads.is_empty() {
            workspace.start_thread(cx);
        } else {
            let first = workspace.first_listed();
            workspace.select_thread(first, cx);
        }
        workspace.start_git_watch(window, cx);
        workspace.listen_for_notification_clicks(window, cx);
        workspace.remote_init(cx);
        cx.observe_window_activation(window, |s, window, cx| {
            s.window_active = window.is_window_active();
            // Coming back to the window is seeing the thread on show.
            if s.window_active && let Some(t) = s.current_thread_mut() {
                crate::notify::withdraw(t.id);
                if t.unread {
                    t.unread = false;
                    s.save_soon(cx);
                    cx.notify();
                }
            }
        })
        .detach();
        workspace.observe_command_draft(cx);
        workspace.observe_mention_draft(cx);
        workspace.composer.update(cx, |s, cx| s.focus(window, cx));
        workspace
    }

    fn current_project(&self) -> &Project {
        &self.projects[self.project]
    }
    fn current_thread(&self) -> Option<&Thread> {
        self.current_project().threads.get(self.thread)
    }
    fn current_thread_mut(&mut self) -> Option<&mut Thread> {
        self.projects[self.project].threads.get_mut(self.thread)
    }
    /// The setting of the current thread that plays `role`, if its agent offers one.
    fn setting(&self, role: Role) -> Option<&Setting> {
        self.current_thread()?.settings.iter().find(|s| s.role == role)
    }
    fn agent_name(&self, thread: &Thread) -> &str {
        self.settings.agents.get(thread.agent).map_or("Agent", |a| a.name.as_str())
    }

    // ── Projects and threads ────────────────────────────────────────────────

    fn select_project(&mut self, index: usize, cx: &mut Context<Self>) {
        self.close_dashboard(cx);
        if self.project == index {
            return;
        }
        // Tabs, the file tree and the terminal belong to the project that was
        // showing: put them away as they are, and bring back the new one's.
        let leaving = ProjectView {
            tabs: std::mem::take(&mut self.tabs),
            active: self.active,
            expanded: std::mem::take(&mut self.expanded),
            terminals: std::mem::take(&mut self.terminals),
            thread: self.current_thread().map(|t| t.id),
            git_repo: self.git_repo.take(),
        };
        self.views.insert(self.project, leaving);
        let inputs = (self.commit_message.read(cx).value().to_string(), self.thread_search.read(cx).value().to_string());
        self.project_inputs.insert(self.project, inputs);
        self.restore_project_inputs = true;
        self.project = index;
        let back = self.views.remove(&index);
        let thread = back.as_ref().and_then(|v| v.thread);
        match back {
            Some(view) => {
                self.tabs = view.tabs;
                self.expanded = view.expanded;
                self.terminals = view.terminals;
                self.git_repo = view.git_repo;
                self.active = view.active.min(self.tabs.len().saturating_sub(1));
            }
            None => {
                self.tabs = vec![Tab { id: self.next_tab, kind: TabKind::Chat, pinned: false }];
                self.next_tab += 1;
                self.active = 0;
            }
        }
        let threads = &self.projects[index].threads;
        match thread.and_then(|id| threads.iter().position(|t| t.id == id)) {
            Some(at) => {
                // Back to the same thread and the same tab.
                let active = self.active;
                self.select_thread(at, cx);
                self.active = active;
                self.focus_terminal(cx);
            }
            None if threads.is_empty() => self.start_thread(cx),
            None => self.select_thread(self.first_listed(), cx),
        }
        self.refresh_git(cx);
    }
    /// Show a thread, starting its agent if it has none, so its models and
    /// settings are there before the next message.
    /// Put the composer's text away for the thread it belongs to.
    fn stash_draft(&mut self, cx: &App) {
        let Some(owner) = self.draft_owner else { return };
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.drafts.remove(&owner);
        } else {
            self.drafts.insert(owner, (text, self.file_mentions.borrow().clone()));
        }
    }
    /// Look through the threads' contents for the search query, off the main
    /// thread, once typing pauses.
    fn search_threads(&mut self, cx: &mut Context<Self>) {
        let query = self.thread_search.read(cx).value().trim().to_string();
        let project = self.project;
        if query.is_empty() {
            self.thread_hits = None;
            self.thread_searching = None;
            return;
        }
        // The input also notifies on cursor moves and focus.
        let same = |p: usize, q: &str| p == project && q == query;
        if self.thread_hits.as_ref().is_some_and(|r| same(r.project, &r.query))
            || self.thread_searching.as_ref().is_some_and(|(p, q, _)| same(*p, q))
        {
            return;
        }
        let task = cx.spawn({
            let query = query.clone();
            async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                // Threads read in are searched as they are, saved or not; the
                // rest through the database's index.
                let Ok(Some((mut docs, db, ids))) = this.update(cx, |s, _| {
                    let p = s.projects.get(project)?;
                    let docs: Vec<search::Doc> = p.threads.iter().filter(|t| !t.unloaded).map(search::doc).collect();
                    let ids: HashMap<i64, usize> = p.threads.iter().filter(|t| t.unloaded).filter_map(|t| Some((t.stored?, t.id))).collect();
                    let db = s.store.as_ref().map(|st| (st.path().to_path_buf(), store::search_key(p)));
                    Some((docs, db, ids))
                }) else {
                    return;
                };
                let hits = cx.background_executor().spawn({
                    let query = query.clone();
                    async move {
                        if let Some((path, key)) = db {
                            let rows = store::search(&path, &key, &query).unwrap_or_else(|e| {
                                eprintln!("chat: couldn't search threads: {e}");
                                Vec::new()
                            });
                            let mut by_thread: Vec<(usize, Vec<(usize, String)>)> = Vec::new();
                            for (stored, seq, text) in rows {
                                let Some(&id) = ids.get(&stored) else { continue };
                                match by_thread.last_mut() {
                                    Some((last, texts)) if *last == id => texts.push((seq, text)),
                                    _ => by_thread.push((id, vec![(seq, text)])),
                                }
                            }
                            docs.extend(by_thread.into_iter().map(|(id, texts)| search::doc_from(id, texts)));
                        }
                        search::run(&docs, &query)
                    }
                });
                let hits = hits.await;
                let _ = this.update(cx, |s, cx| {
                    s.thread_hits = Some(search::Results { project, query, hits });
                    s.thread_searching = None;
                    cx.notify();
                });
            }
        });
        self.thread_searching = Some((project, query, task));
    }

    fn select_thread(&mut self, index: usize, cx: &mut Context<Self>) {
        self.close_dashboard(cx);
        let id = self.current_project().threads.get(index).map(|t| t.id);
        if id != self.draft_owner {
            self.stash_draft(cx);
            self.draft_owner = id;
            // The composer is refilled on the next render, which has the window it needs.
            self.restore_draft = true;
        }
        self.thread = index;
        // A saved thread's blocks are read in when it's first shown.
        if let (Some(store), Some(t)) = (&self.store, self.projects[self.project].threads.get_mut(index)) {
            store.load_blocks(t);
        }
        self.selection.borrow_mut().clear();
        self.active = 0;
        self.follow = true;
        self.transcript.scroll_to_bottom();
        if let Some(t) = self.current_thread_mut() {
            t.unread = false;
            crate::notify::withdraw(t.id);
        }
        if let Some(t) = self.current_thread() {
            let id = t.id;
            if t.session.is_none() && t.problem.is_none() {
                self.connect(self.project, index, cx);
            }
            // Most recently shown goes last.
            if let Some(at) = self.live.iter().position(|l| *l == id) {
                self.live.remove(at);
                self.live.push(id);
            }
        }
        cx.notify();
    }
    /// Put a fresh thread at the top of the current project and connect its agent,
    /// so its models are there to pick from before the first message.
    fn start_thread(&mut self, cx: &mut Context<Self>) {
        // An untouched thread is already what a new one would be.
        if let Some(blank) = self.current_project().threads.iter().position(|t| t.is_blank()) {
            return self.select_thread(blank, cx);
        }
        self.next_thread += 1;
        let mut thread = Thread::new(self.next_thread, self.settings.agent);
        thread.picks = self.starting_picks(self.project, thread.agent);
        self.projects[self.project].threads.insert(0, thread);
        self.select_thread(0, cx);
    }
    fn new_thread(&mut self, _: &NewThread, window: &mut Window, cx: &mut Context<Self>) {
        self.start_thread(cx);
        self.composer.update(cx, |s, cx| s.focus(window, cx));
    }
    /// Write the rail back to settings: paths, names and folders.
    fn save_projects(&mut self) {
        self.settings.projects = self.projects.iter().filter_map(Project::entry).collect();
        // Picks go with their project.
        let kept: Vec<PathBuf> = self.projects.iter().map(|p| PathBuf::from(p.key())).collect();
        self.settings.project_picks.retain(|path, _| kept.contains(path));
        let _ = self.settings.save();
    }
    /// Add folders to a project. Its agents are told about them from their next session on.
    fn add_folders(&mut self, project: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.get(project).is_none_or(|p| p.path.is_none()) {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add to project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |s, cx| {
                let p = &mut s.projects[project];
                for path in paths {
                    if !p.roots().contains(&path) {
                        p.folders.push(path);
                    }
                }
                s.save_projects();
                s.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn remove_folder(&mut self, project: usize, folder: &Path, cx: &mut Context<Self>) {
        self.projects[project].folders.retain(|f| f != folder);
        if self.git_repo.as_deref() == Some(folder) {
            self.git_repo = None;
        }
        self.save_projects();
        self.refresh_git(cx);
        cx.notify();
    }
    /// Ask before taking a project off the rail.
    fn confirm_remove_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index).filter(|p| p.path.is_some()) else {
            return;
        };
        let message = format!("Remove “{}”?", project.name);
        let detail = "Its folders stay on disk, and its threads are kept: add the folder again to get them back. Agents and terminals running in it are stopped.";
        let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), &["Remove", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |s, cx| s.remove_project(index, cx));
            }
        })
        .detach();
    }
    /// Take a project off the rail. Its threads stay in the store, so adding
    /// the folder back brings them back. Chats can't be removed.
    fn remove_project(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == 0 || self.projects.get(index).is_none_or(|p| p.path.is_none()) {
            return;
        }
        // Everything it has goes to disk before it goes.
        self.save_now();
        if self.project == index {
            self.select_project(index - 1, cx);
        }
        let project = self.projects.remove(index);
        let ids: HashSet<usize> = project.threads.iter().map(|t| t.id).collect();
        // Dropping the sessions and the view stops its agents and shells.
        drop(project);
        self.live.retain(|id| !ids.contains(id));
        self.drafts.retain(|id, _| !ids.contains(id));
        self.git_picks.retain(|(thread, _), _| thread.is_none_or(|t| !ids.contains(&t)));
        self.views.remove(&index);
        self.project_inputs.remove(&index);
        // Everything kept by project index after it moves up one.
        let shift = |i: usize| if i > index { i - 1 } else { i };
        self.project = shift(self.project);
        self.renaming = self.renaming.filter(|&i| i != index).map(shift);
        self.drop_slot = None;
        self.views = std::mem::take(&mut self.views).into_iter().map(|(k, v)| (shift(k), v)).collect();
        self.project_inputs = std::mem::take(&mut self.project_inputs).into_iter().map(|(k, v)| (shift(k), v)).collect();
        if self.thread_hits.as_ref().is_some_and(|r| r.project >= index) {
            self.thread_hits = None;
        }
        if self.thread_searching.as_ref().is_some_and(|(p, _, _)| *p >= index) {
            self.thread_searching = None;
        }
        self.save_projects();
        cx.notify();
    }
    /// Move a project to another place on the rail. Chats stay first.
    fn move_project(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let len = self.projects.len();
        if from == to || from == 0 || to == 0 || from >= len || to >= len {
            return;
        }
        let project = self.projects.remove(from);
        self.projects.insert(to, project);
        // Everything kept by project index follows its project.
        let moved = |i: usize| match i {
            i if i == from => to,
            i if from < to && i > from && i <= to => i - 1,
            i if from > to && i >= to && i < from => i + 1,
            i => i,
        };
        self.project = moved(self.project);
        self.renaming = self.renaming.map(moved);
        self.views = std::mem::take(&mut self.views).into_iter().map(|(k, v)| (moved(k), v)).collect();
        self.project_inputs = std::mem::take(&mut self.project_inputs).into_iter().map(|(k, v)| (moved(k), v)).collect();
        self.save_projects();
        cx.notify();
    }
    /// Show the rename field over the project, at `position`.
    fn start_rename(&mut self, project: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.projects.get(project).filter(|p| p.path.is_some()).map(|p| p.name.clone()) else {
            return;
        };
        self.renaming = Some(project);
        self.rename_position = position;
        self.rename_input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        // Typing replaces the old name, as in a Finder rename.
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        cx.notify();
    }
    /// Keep the typed name (an empty one goes back to the folder's), or drop it.
    fn finish_rename(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.renaming.take() else {
            return;
        };
        if keep && let Some(project) = self.projects.get_mut(index) {
            let name = self.rename_input.read(cx).value().trim().to_string();
            project.name = match (&project.path, name.is_empty()) {
                (Some(path), true) => crate::model::folder_name(path),
                _ => name,
            };
            self.save_projects();
        }
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    // ── Agent sessions ──────────────────────────────────────────────────────

    /// Start the thread's agent and fold its events into the thread as they arrive.
    fn connect(&mut self, project: usize, thread: usize, cx: &mut Context<Self>) {
        let cwd = self.projects[project].path.clone().unwrap_or_else(Settings::scratch);
        let extra = self.projects[project].folders.clone();
        let roots = self.projects[project].roots();
        let t = &mut self.projects[project].threads[thread];
        let Some(spec) = self.settings.agents.get(t.agent).cloned() else {
            return;
        };
        let id = t.id;
        // Capture the latest choices before clearing the controls for reconnect.
        t.picks = t.current_picks();
        let tool = crate::worktree_tool::socket().map(|s| crate::worktree_tool::args(s, t.id, &roots));
        let (commands, mut events, run) = agent::connect(spec, cwd, extra, t.session_id.clone(), t.picks.clone(), tool);
        let driver = cx.background_executor().spawn(run);
        let listener = cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |s, cx| s.on_event(id, event, cx)).is_err() {
                    break;
                }
            }
        });
        t.ready = false;
        t.supports_steering = false;
        t.problem = None;
        if t.state == ThreadState::Failed && t.is_blank() {
            t.state = ThreadState::Idle;
        }
        t.settings.clear();
        t.session = Some(Session { commands, _driver: driver, _listener: listener });
        self.live.retain(|l| *l != id);
        self.live.push(id);
        self.stop_idle_agents();
    }
    /// Take reports from agents' tools as they come.
    fn listen_for_worktrees(&mut self, cx: &mut Context<Self>) {
        let (tx, mut reports) = futures::channel::mpsc::unbounded();
        if crate::worktree_tool::listen(move |report| {
            let _ = tx.unbounded_send(report);
        })
        .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(report) = reports.next().await {
                let handled = this.update(cx, |s, cx| match report {
                    crate::worktree_tool::Message::Worktree(report) => s.on_worktree(report, cx),
                    crate::worktree_tool::Message::Title { thread, title } => s.on_title(thread, title, cx),
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    /// Bring up the thread whose notification was clicked.
    fn listen_for_notification_clicks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut clicks) = crate::notify::init() else { return };
        cx.spawn_in(window, async move |this, cx| {
            while let Some(id) = clicks.next().await {
                if this.update_in(cx, |s, window, cx| s.reveal_thread(id, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    fn reveal_thread(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.projects.iter().enumerate().find_map(|(p, project)| {
            project.threads.iter().position(|t| t.id == id).map(|t| (p, t))
        });
        let Some((project, thread)) = found else { return };
        cx.activate(true);
        window.activate_window();
        self.select_project(project, cx);
        self.select_thread(thread, cx);
    }
    /// A thread's agent says which worktree it works in for a project folder.
    fn on_worktree(&mut self, report: crate::worktree_tool::Report, cx: &mut Context<Self>) {
        let Some(thread) = self.projects.iter_mut().flat_map(|p| &mut p.threads).find(|t| t.id == report.thread) else {
            return;
        };
        match report.worktree {
            Some(tree) => thread.worktrees.insert(report.folder.clone(), tree),
            None => thread.worktrees.remove(&report.folder),
        };
        // The agent's word is newer than a pick made before it.
        self.git_picks.remove(&(Some(report.thread), report.folder));
        self.save_soon(cx);
        self.refresh_git(cx);
        cx.notify();
    }
    /// A thread's agent renamed it.
    fn on_title(&mut self, id: usize, title: String, cx: &mut Context<Self>) {
        let Some(thread) = self.projects.iter_mut().flat_map(|p| &mut p.threads).find(|t| t.id == id) else {
            return;
        };
        thread.title = title.into();
        self.save_soon(cx);
        cx.notify();
    }
    /// Keep at most `LIVE_SESSIONS` agents running, stopping the ones shown
    /// longest ago. A thread mid-turn, or the one on screen, is never stopped.
    fn stop_idle_agents(&mut self) {
        let showing = self.current_thread().map(|t| t.id);
        while self.live.len() > LIVE_SESSIONS {
            let threads = self.projects.iter_mut().flat_map(|p| &mut p.threads);
            let mut threads: HashMap<usize, &mut Thread> = threads.map(|t| (t.id, t)).collect();
            let victim = self.live.iter().position(|id| {
                Some(*id) != showing && threads.get(id).is_none_or(|t| !t.is_active())
            });
            let Some(at) = victim else { break };
            let id = self.live.remove(at);
            if let Some(t) = threads.get_mut(&id) {
                if !t.settings.is_empty() {
                    t.picks = t.current_picks();
                }
                // Dropping the session kills the process; nothing is reported back.
                t.session = None;
                t.ready = false;
                t.supports_steering = false;
                t.settings.clear();
            }
        }
    }

    // ── Saving ──────────────────────────────────────────────────────────────

    /// Write threads to disk shortly; a burst of streamed chunks costs one write.
    fn save_soon(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.saving, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            let Ok(Some((writer, plan))) = this.update(cx, |s, _| {
                s.saving = false;
                s.save_plan()
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = store::write(&writer, &plan) {
                        eprintln!("chat: couldn't save threads: {e}");
                    }
                })
                .await;
        })
        .detach();
    }
    /// What to write now, with the connection to write it on. Threads put
    /// away a while are let go of here, once what they hold is in the plan.
    fn save_plan(&mut self) -> Option<(std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>, store::Plan)> {
        let store = self.store.as_mut()?;
        let plan = store.plan(&mut self.projects, &self.settings.agents);
        let writer = store.writer();
        self.unload_put_away(&plan.untouched());
        Some((writer, plan))
    }
    fn save_now(&mut self) {
        if let Some((writer, plan)) = self.save_plan()
            && let Err(e) = store::write(&writer, &plan)
        {
            eprintln!("chat: couldn't save threads: {e}");
        }
    }
    /// Let go of the blocks of saved threads nobody's looking at: not on
    /// show, not the one a project goes back to, with no agent running. Only
    /// those with nothing new to write, so a read can't race the write. They're
    /// read back in when next opened.
    fn unload_put_away(&mut self, saved: &HashSet<i64>) {
        let showing = self.current_thread().map(|t| t.id);
        let kept: HashSet<usize> = self.views.values().filter_map(|v| v.thread).chain(showing).collect();
        for t in self.projects.iter_mut().flat_map(|p| &mut p.threads) {
            if t.stored.is_some_and(|id| saved.contains(&id)) && !t.unloaded && t.session.is_none() && !t.is_active() && !kept.contains(&t.id) {
                t.blocks = Vec::new();
                t.saved = Vec::new();
                t.unloaded = true;
            }
        }
    }
    fn on_event(&mut self, id: usize, event: Event, cx: &mut Context<Self>) {
        let seen = self.window_active && self.current_thread().is_some_and(|t| t.id == id);
        let current = self.current_thread().is_some_and(|t| t.id == id);
        let Some(thread) = self.projects.iter_mut().flat_map(|p| &mut p.threads).find(|t| t.id == id) else {
            return;
        };
        // A connection that fails before it opens usually means the agent isn't installed or signed in.
        let failed_to_open = matches!(&event, Event::Closed(Some(_))) && !thread.ready;
        if let Event::RateLimit(limits) = &event {
            if let Some(store) = &mut self.store {
                store.note_limits(self.settings.agents.get(thread.agent).map_or("", |a| a.name.as_str()), limits);
            }
            let known = self.limits.entry(thread.agent).or_default();
            for limit in limits {
                known.insert(limit.window.clone(), limit.clone());
            }
        }
        let was_busy = thread.state.busy();
        if current && matches!(event, Event::Ask { .. }) {
            self.focus_ask = true;
        }
        let asking = match &event {
            Event::Permission { title, .. } => Some(format!("Needs permission: {title}")),
            Event::Ask { message, questions, .. } => Some(
                Some(message.as_str())
                    .filter(|m| !m.trim().is_empty())
                    .or_else(|| questions.first().and_then(|q| q.prompt.as_deref().or(q.title.as_deref())))
                    .map_or("Has a question for you".into(), |m| format!("Asks: {m}")),
            ),
            _ => None,
        };
        // A fresh session with nothing to put back opens on the agent's own
        // defaults. One that saves picks as its defaults won't keep them, so
        // note them first, for projects that never picked.
        if let Event::Ready { settings, .. } = &event
            && thread.session_id.is_none()
            && thread.picks.is_empty()
            && let Some(spec) = self.settings.agents.get(thread.agent)
            && spec.saves_picks()
            && !self.settings.base_picks.contains_key(&spec.name)
        {
            let base = model_and_effort(settings);
            if !base.is_empty() {
                self.settings.base_picks.insert(spec.name.clone(), base);
                let _ = self.settings.save();
            }
        }
        thread.apply(event);
        // A turn that finishes out of sight waits to be read.
        let ended = was_busy && matches!(thread.state, ThreadState::Done | ThreadState::Failed);
        if ended && !seen {
            thread.unread = true;
        }
        // Out of sight, a finished turn or a question for the user is worth a notification.
        let note = (!seen && self.settings.notifications != Some(false))
            .then(|| asking.or_else(|| ended.then(|| thread.ending_summary())))
            .flatten()
            .map(|body| (thread.title.to_string(), body));
        if let Some((title, body)) = note {
            let project = self.projects.iter().find(|p| p.threads.iter().any(|t| t.id == id));
            crate::notify::post(id, &title, project.map_or("", |p| p.name.as_str()), &body);
        }
        // A mission waiting on turns counts this one; an empty mailbox may get one.
        if ended {
            self.mission_turn_ended(cx);
        }
        let Some(thread) = self.projects.iter_mut().flat_map(|p| &mut p.threads).find(|t| t.id == id) else {
            return;
        };
        if failed_to_open && let Some(problem) = &mut thread.problem {
            let name = self.settings.agents.get(thread.agent).map_or("the agent", |a| a.name.as_str());
            problem.push_str(&format!("\nCheck that {name} is installed and signed in, then send your message again."));
        }
        // Follow new output only while pinned, so scrolling back to read isn't
        // undone by the next chunk, nor by a drag on the scrollbar.
        if current && self.follow && self.holding_transcript.is_none() {
            self.transcript.scroll_to_bottom();
        }
        self.save_soon(cx);
        cx.notify();
    }
    /// Switch the current thread to another agent. Only a thread with no history
    /// can: an agent's session holds the conversation, and it doesn't travel.
    fn set_agent(&mut self, agent: usize, cx: &mut Context<Self>) {
        self.settings.agent = agent;
        let _ = self.settings.save();
        let (project, index) = (self.project, self.thread);
        let picks = self.starting_picks(project, agent);
        if let Some(thread) = self.current_thread_mut()
            && thread.blocks.is_empty()
            && thread.agent != agent
        {
            thread.agent = agent;
            // The old agent's session and choices mean nothing to the new one:
            // reconnecting would try to resume that session and carry over its
            // model (Claude's "default", which Codex then runs with).
            thread.session_id = None;
            thread.settings.clear();
            thread.picks = picks;
            self.connect(project, index, cx);
        }
        cx.notify();
    }
    /// Change a session setting (model, effort, mode) on the live session.
    fn set_setting(&mut self, setting: String, choice: String, cx: &mut Context<Self>) {
        if let Some(thread) = self.current_thread_mut() {
            // Show the pick at once; the agent's reply settles what it really is.
            if let Some(s) = thread.settings.iter_mut().find(|s| s.id == setting) {
                s.current = choice.clone();
            }
            thread.send(Command::Set { setting, choice });
        }
        self.remember_picks();
        self.save_soon(cx);
        cx.notify();
    }
    /// Where a project's picks are filed in settings.
    fn picks_key(&self, project: usize) -> PathBuf {
        PathBuf::from(self.projects[project].key())
    }
    /// The model and effort a new thread in `project` starts on: those last
    /// picked there, else what the agent began on before any pick, if it keeps
    /// picks as its default. Nothing means the agent's own default.
    fn starting_picks(&self, project: usize, agent: usize) -> Vec<(String, String)> {
        let Some(name) = self.settings.agents.get(agent).map(|a| &a.name) else { return Vec::new() };
        self.settings
            .project_picks
            .get(&self.picks_key(project))
            .and_then(|by_agent| by_agent.get(name))
            .or_else(|| self.settings.base_picks.get(name))
            .cloned()
            .unwrap_or_default()
    }
    /// Note the current thread's model and effort as its project's, so the
    /// next thread there starts on them, and only there.
    fn remember_picks(&mut self) {
        let key = self.picks_key(self.project);
        let Some(thread) = self.current_thread() else { return };
        let Some(name) = self.settings.agents.get(thread.agent).map(|a| a.name.clone()) else { return };
        let picks = model_and_effort(&thread.settings);
        if picks.is_empty() {
            return;
        }
        self.settings.project_picks.entry(key).or_default().insert(name, picks);
        let _ = self.settings.save();
    }
    fn answer(&mut self, id: u64, answer: Option<(String, String)>, cx: &mut Context<Self>) {
        let Some(thread) = self.current_thread_mut() else {
            return;
        };
        for block in &mut thread.blocks {
            if let Block::Permission(p) = block
                && p.id == id
            {
                p.chosen = Some(answer.as_ref().map_or("Dismissed".into(), |(_, name)| name.clone()));
            }
        }
        thread.send(Command::Answer { id, answer: answer.map(|(id, _)| id) });
        if !thread.awaiting_user() && thread.state == ThreadState::Waiting {
            thread.state = ThreadState::Running;
        }
        cx.notify();
    }
    /// Pick or unpick one choice of a question the agent asked.
    fn pick(&mut self, ask: u64, field: String, value: String, cx: &mut Context<Self>) {
        let Some(thread) = self.current_thread_mut() else { return };
        let id = thread.id;
        for block in &mut thread.blocks {
            if let Block::Ask(a) = block
                && a.id == ask
                && a.outcome.is_none()
            {
                a.toggle(&field, &value);
            }
        }
        self.ask_errors.remove(&(id, ask));
        cx.notify();
    }
    /// Send the answers to a set of questions, or with `skip`, decline them.
    fn reply(&mut self, ask: u64, skip: bool, cx: &mut Context<Self>) {
        let Some(id) = self.current_thread().map(|t| t.id) else { return };
        let typed: HashMap<String, String> = self
            .ask_inputs
            .iter()
            .filter(|((t, a, _), _)| *t == id && *a == ask)
            .map(|((_, _, field), input)| (field.clone(), input.read(cx).value().to_string()))
            .collect();
        let Some(thread) = self.current_thread_mut() else { return };
        let Some(a) = thread.blocks.iter_mut().find_map(|b| match b {
            Block::Ask(a) if a.id == ask && a.outcome.is_none() => Some(a),
            _ => None,
        }) else {
            return;
        };
        let replies = if skip {
            None
        } else {
            match a.replies(|field| typed.get(field).cloned().unwrap_or_default()) {
                Ok(replies) => Some(replies),
                Err(error) => {
                    self.ask_errors.insert((id, ask), error);
                    cx.notify();
                    return;
                }
            }
        };
        a.outcome = Some(match &replies {
            Some(replies) => crate::model::AskOutcome::Answered(a.shown(replies)),
            None => crate::model::AskOutcome::Skipped,
        });
        thread.send(Command::Reply { id: ask, replies });
        if !thread.awaiting_user() && thread.state == ThreadState::Waiting {
            thread.state = ThreadState::Running;
        }
        self.ask_errors.remove(&(id, ask));
        self.ask_inputs.retain(|(t, a, _), _| !(*t == id && *a == ask));
        self.save_soon(cx);
        cx.notify();
    }
    /// The showing thread's first unanswered set of questions, which takes keys.
    fn waiting_ask(&self) -> Option<(usize, &crate::model::Ask)> {
        let thread = self.current_thread()?;
        thread.blocks.iter().find_map(|b| match b {
            Block::Ask(a) if a.outcome.is_none() => Some((thread.id, a)),
            _ => None,
        })
    }
    /// Where the keys are in the waiting card, made to fit it: question and choice.
    fn ask_position(&self) -> Option<(usize, u64, usize, usize)> {
        let (thread, ask) = self.waiting_ask()?;
        let (question, choice) = match self.ask_cursor {
            Some((t, a, q, c)) if t == thread && a == ask.id && q < ask.questions.len() => (q, c),
            _ => (0, 0),
        };
        let choice = choice.min(choice_count(&ask.questions[question].kind).saturating_sub(1));
        Some((thread, ask.id, question, choice))
    }
    /// Put the cursor on the question it's at: its picked choice, or its first,
    /// or for a typed answer, its input.
    fn ask_cursor_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread, id, question, _)) = self.ask_position() else { return };
        let Some((_, ask)) = self.waiting_ask() else { return };
        let q = &ask.questions[question];
        let picked = ask.picks.get(&q.field).and_then(|p| p.first()).cloned();
        let values = choice_values(&q.kind);
        let choice = picked.and_then(|p| values.iter().position(|v| *v == p)).unwrap_or(0);
        let field = q.field.clone();
        self.ask_cursor = Some((thread, id, question, choice));
        self.focus_ask_cursor(&field, window, cx);
    }
    /// Give the keys to the cursor's question: its input when it's typed, else the card.
    fn focus_ask_cursor(&mut self, field: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread, id, _, _)) = self.ask_cursor else { return };
        match self.ask_inputs.get(&(thread, id, field.to_string())) {
            Some(input) => input.update(cx, |i, cx| i.focus(window, cx)),
            None => self.ask_focus.focus(window),
        }
        cx.notify();
    }
    /// The question whose input has the keys, if one does.
    fn ask_input_focused(&self, window: &Window, cx: &App) -> Option<usize> {
        let (thread, ask) = self.waiting_ask()?;
        ask.questions.iter().position(|q| {
            self.ask_inputs.get(&(thread, ask.id, q.field.clone())).is_some_and(|i| i.read(cx).focus_handle(cx).is_focused(window))
        })
    }
    /// Arrows: the next or previous choice, on into the questions around it.
    fn ask_step(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Arrows inside a typed answer stay the input's.
        if !self.ask_focus.is_focused(window) {
            cx.propagate();
            return;
        }
        let Some((thread, id, question, choice)) = self.ask_position() else { return };
        let Some((_, ask)) = self.waiting_ask() else { return };
        // Every place the cursor can be, in order: each choice, and each input.
        let places: Vec<(usize, usize)> = ask
            .questions
            .iter()
            .enumerate()
            .flat_map(|(q, question)| (0..choice_count(&question.kind).max(1)).map(move |c| (q, c)))
            .collect();
        let at = places.iter().position(|p| *p == (question, choice)).unwrap_or(0);
        let to = if down { (at + 1).min(places.len() - 1) } else { at.saturating_sub(1) };
        let (question, choice) = places[to];
        let field = ask.questions[question].field.clone();
        self.ask_cursor = Some((thread, id, question, choice));
        self.focus_ask_cursor(&field, window, cx);
    }
    /// Tab: the next or previous question.
    fn ask_jump(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread, id, question, _)) = self.ask_position() else { return };
        let count = self.waiting_ask().map_or(0, |(_, a)| a.questions.len());
        let question = self.ask_input_focused(window, cx).unwrap_or(question);
        let next = if forward { (question + 1) % count } else { (question + count - 1) % count };
        self.ask_cursor = Some((thread, id, next, 0));
        self.ask_cursor_start(window, cx);
    }
    /// Space or Enter: pick the choice under the cursor.
    fn ask_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ask_focus.is_focused(window) {
            cx.propagate();
            return;
        }
        let Some((_, id, question, choice)) = self.ask_position() else { return };
        let Some((_, ask)) = self.waiting_ask() else { return };
        let q = &ask.questions[question];
        let Some(value) = choice_values(&q.kind).get(choice).cloned() else { return };
        let field = q.field.clone();
        self.pick(id, field, value, cx);
    }
    /// ⌘ Enter or Escape: send the answers, or skip; then back to the composer.
    fn ask_finish(&mut self, skip: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, id, _, _)) = self.ask_position() else { return };
        self.reply(id, skip, cx);
        if self.waiting_ask().is_none_or(|(_, a)| a.id != id) {
            self.ask_cursor = None;
            match self.waiting_ask().map(|(t, a)| (t, a.id)) {
                Some((thread, next)) => {
                    self.ask_cursor = Some((thread, next, 0, 0));
                    self.ask_cursor_start(window, cx);
                }
                None => self.composer.update(cx, |c, cx| c.focus(window, cx)),
            }
        }
    }
    /// Give each text field of the showing thread's waiting questions an input,
    /// and let go of those whose questions are settled.
    fn prepare_ask_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.current_thread() else { return };
        let id = thread.id;
        let mut wanted = Vec::new();
        for block in &thread.blocks {
            if let Block::Ask(a) = block
                && a.outcome.is_none()
            {
                for q in &a.questions {
                    if matches!(q.kind, agent::QuestionKind::Text | agent::QuestionKind::Number { .. }) {
                        wanted.push((a.id, q.field.clone(), q.prompt.clone()));
                    }
                }
            }
        }
        self.ask_inputs.retain(|(t, a, f), _| *t != id || wanted.iter().any(|(w, g, _)| w == a && g == f));
        for (ask, field, prompt) in wanted {
            let key = (id, ask, field);
            if self.ask_inputs.contains_key(&key) {
                continue;
            }
            let placeholder = prompt.unwrap_or_else(|| "Type an answer…".into());
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            cx.subscribe_in(&input, window, move |s, _, e: &gpui_component::input::InputEvent, window, cx| {
                if let gpui_component::input::InputEvent::PressEnter { .. } = e {
                    s.reply(ask, false, cx);
                    if s.waiting_ask().is_none_or(|(_, a)| a.id != ask) {
                        s.ask_cursor = None;
                        s.composer.update(cx, |c, cx| c.focus(window, cx));
                    }
                }
            })
            .detach();
            self.ask_inputs.insert(key, input);
        }
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = self.current_thread() {
            thread.send(Command::Cancel);
        }
        cx.notify();
    }
    /// Send the composer's text, either steering the active turn or queueing a
    /// follow-up according to the saved preference.
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value();
        let mentions = self.file_mentions.borrow();
        let title = chat::replace_file_mentions(draft.trim(), &mentions, chat::Mentions::Names);
        let shown = chat::replace_file_mentions(draft.trim(), &mentions, chat::Mentions::Chips);
        let text = chat::replace_file_mentions(draft.trim(), &mentions, chat::Mentions::Paths);
        drop(mentions);
        let (project, index) = (self.project, self.thread);
        let mode = self.settings.follow_up_mode;
        let Some(thread) = self.current_thread() else {
            return;
        };
        // Nothing typed on a cut-off thread: pick it back up.
        let (shown, text) = if text.is_empty() && thread.interrupted() {
            ("Continue".to_string(), "Continue where you left off.".to_string())
        } else {
            (shown, text)
        };
        if text.is_empty() {
            return;
        }
        if thread.session.is_none() {
            self.connect(project, index, cx);
        }
        let thread = &mut self.projects[project].threads[index];
        if thread.blocks.is_empty() {
            thread.title = title.lines().next().unwrap_or_default().to_string().into();
        }
        thread.submit(shown, text, mode == FollowUpMode::Steer);
        self.file_mentions.borrow_mut().clear();
        self.clear_composer = true;
        self.active = 0;
        self.follow = true;
        self.transcript.scroll_to_bottom();
        self.save_soon(cx);
        cx.notify();
    }

    // ── Tabs and panels ─────────────────────────────────────────────────────

    /// Focus the tab showing `kind`, opening one beside the chat if needed.
    fn open(&mut self, kind: TabKind, cx: &mut Context<Self>) {
        if let TabKind::Terminal(cwd) = &kind && !self.terminals.contains_key(cwd) {
            let view = terminal::TerminalView::new(cwd.clone(), cx).map_err(|e| SharedString::from(e.to_string()));
            self.terminals.insert(cwd.clone(), view);
        }
        if let TabKind::File(path) = &kind {
            if let Some(media) = media::kind(path) {
                // Kept so the tab counts as open, but never read as text.
                self.files.insert(path.clone(), Err("This file isn't text.".into()));
                self.open_media(path.clone(), media, cx);
            } else {
                self.files.insert(path.clone(), read_lines(path));
                self.file_scroll.entry(path.clone()).or_default();
                self.load_file_git(path.clone(), cx);
            }
        }
        // A diff's file goes to its server too, so its lines can be looked up.
        if let TabKind::File(path) | TabKind::Diff(path) | TabKind::GitDiff(path, _) = &kind {
            let path = path.clone();
            self.lsp_open(&path, cx);
        }
        self.active = match self.tabs.iter().position(|t| t.kind == kind) {
            Some(i) => i,
            None => {
                self.tabs.push(Tab { id: self.next_tab, kind, pinned: false });
                self.next_tab += 1;
                self.tabs.len() - 1
            }
        };
        self.refresh_git(cx);
        self.focus_terminal(cx);
        cx.notify();
    }
    /// Read an open file's gutter marks and blame off the main thread.
    pub(super) fn load_file_git(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let read = path.clone();
            let info = cx.background_executor().spawn(async move { crate::git::file_info(&read) }).await;
            let _ = this.update(cx, |s, cx| {
                // The tab may have closed meanwhile.
                if !s.files.contains_key(&path) {
                    return;
                }
                // Changes still there stay open; the rest close.
                if let Some(open) = s.hunks_open.get_mut(&path) {
                    let hunks = info.as_ref().map(|i| i.hunks.as_slice()).unwrap_or_default();
                    open.retain(|start| hunks.iter().any(|h| h.new.start == *start && !h.old.is_empty()));
                }
                match info {
                    Some(info) => s.file_git.insert(path, Rc::new(info)),
                    None => s.file_git.remove(&path),
                };
                cx.notify();
            });
        })
        .detach();
    }
    /// Open `path` in a tab and, given 1-based `lines`, scroll them into view
    /// and light them up for a moment.
    pub(super) fn open_at(&mut self, path: PathBuf, lines: Option<(usize, usize)>, cx: &mut Context<Self>) {
        self.open(TabKind::File(path.clone()), cx);
        let Some((first, last)) = lines else { return };
        let Some(Ok(file)) = self.files.get(&path) else { return };
        let Some(end) = file.lines.len().checked_sub(1) else { return };
        let first = first.saturating_sub(1).min(end);
        let last = last.saturating_sub(1).clamp(first, end);
        if let Some(scroll) = self.file_scroll.get(&path) {
            scroll.scroll_to_item(self.file_list_index(&path, first), ScrollStrategy::Center);
        }
        self.flash = Some(Flash { path, rows: first..=last, at: std::time::Instant::now() });
        cx.notify();
    }
    fn focus_terminal(&self, cx: &mut Context<Self>) {
        if let TabKind::Terminal(cwd) = &self.tabs[self.active].kind
            && let Some(Ok(terminal)) = self.terminals.get(cwd)
        {
            terminal.update(cx, |s, cx| {
                s.request_focus = true;
                cx.notify();
            });
        }
    }
    /// Open a terminal: straight away in a single-folder project, otherwise
    /// asking which folder to start it in.
    fn open_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let roots = self.current_project().roots();
        if roots.len() < 2 {
            let cwd = roots.into_iter().next().unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())));
            return self.open(TabKind::Terminal(cwd), cx);
        }
        let weak = cx.weak_entity();
        let build: popover::Builder = Rc::new(move |cx: &App| {
            let Some(w) = weak.upgrade() else { return Vec::new() };
            let s = w.read(cx);
            let mut entries = vec![crate::menu::Entry::Label("Open terminal in".into())];
            for root in s.current_project().roots() {
                let running = s.terminals.get(&root).is_some_and(|t| t.is_ok());
                let mut item = crate::menu::Item::new(crate::model::folder_name(&root)).icon(gpui_component::IconName::Folder);
                if running {
                    item = item.detail("running");
                }
                let weak = weak.clone();
                entries.push(crate::menu::Entry::Item(item.on_click(move |_, cx| {
                    let _ = weak.update(cx, |s, cx| s.open(TabKind::Terminal(root.clone()), cx));
                })));
            }
            entries
        });
        self.open_popover(TERMINAL_BUTTON, Point::default(), Corner::TopRight, 240., None, build, window, cx);
    }
    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.get(index).is_none_or(Tab::fixed) {
            return;
        }
        if let TabKind::Terminal(cwd) = &self.tabs[index].kind {
            self.terminals.remove(cwd);
        }
        let kind = self.tabs.remove(index).kind;
        if let TabKind::File(path) = &kind {
            self.files.remove(path);
            self.media.remove(path);
            self.file_scroll.remove(path);
            self.file_git.remove(path);
            self.hunks_open.remove(path);
        }
        // Its server forgets the file once no tab shows it.
        if let TabKind::File(path) | TabKind::Diff(path) | TabKind::GitDiff(path, _) = &kind
            && !self.tabs.iter().any(|t| matches!(&t.kind, TabKind::File(p) | TabKind::Diff(p) | TabKind::GitDiff(p, _) if p == path))
        {
            self.lsp_close(path, cx);
        }
        if self.active >= index {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
    }
    /// Close every tab but the one at `index` (and those that stay put), and show it.
    pub(super) fn close_others(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(keep) = self.tabs.get(index).map(|t| t.id) else { return };
        for i in (0..self.tabs.len()).rev() {
            if self.tabs[i].id != keep {
                self.close(i, cx);
            }
        }
        if let Some(i) = self.tabs.iter().position(|t| t.id == keep) {
            self.active = i;
        }
        self.focus_terminal(cx);
        cx.notify();
    }
    /// Pin or unpin the tab at `index`. Pinned tabs gather right after the chat,
    /// so cycling through tabs goes in the order they're shown.
    pub(super) fn toggle_pin(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.get(index).is_none_or(|t| t.kind == TabKind::Chat) {
            return;
        }
        let active = self.tabs.get(self.active).map(|t| t.id);
        let mut tab = self.tabs.remove(index);
        tab.pinned = !tab.pinned;
        // Pinning joins the end of the fixed run; unpinning leads the rest.
        let at = self.tabs.iter().take_while(|t| t.fixed()).count();
        self.tabs.insert(at, tab);
        if let Some(i) = active.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
            self.active = i;
        }
        cx.notify();
    }
    fn cycle(&mut self, step: isize, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        self.active = (self.active as isize + step).rem_euclid(n) as usize;
        self.focus_terminal(cx);
        cx.notify();
    }
    fn show_side(&mut self, side: Side, cx: &mut Context<Self>) {
        // Picking the page that's already showing folds the panel away.
        self.side_open = !(self.side_open && self.side == side);
        self.side = side;
        cx.notify();
    }
    fn toggle_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
            // Read again on every open, so new files show up.
            // A folder git ignores passes that on to what's inside.
            let ignored = path
                .parent()
                .and_then(|parent| self.listings.get(parent))
                .and_then(|entries| entries.iter().find(|e| e.path == path))
                .is_some_and(|e| e.ignored);
            self.listings.insert(path.to_path_buf(), list(path, ignored));
        }
        cx.notify();
    }

    /// Take up what the rows said about the drag's last move. A closed
    /// folder rested on opens after a moment, as in Finder.
    pub(super) fn take_file_drop_probe(&mut self, cx: &mut Context<Self>) {
        let next = self.file_drop_probe.borrow_mut().take().map(|(target, over)| FileDrop { target, over });
        if next == self.file_drop {
            return;
        }
        let over = next.as_ref().and_then(|d| d.over.clone()).filter(|f| !self.expanded.contains(f));
        let changed = self.file_drop.as_ref().and_then(|d| d.over.as_ref()) != next.as_ref().and_then(|d| d.over.as_ref());
        self.file_drop = next;
        if let Some(folder) = over.filter(|_| changed) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SPRING_DELAY).await;
                let _ = this.update(cx, |s, cx| {
                    let resting = s.file_drop.as_ref().is_some_and(|d| d.over.as_ref() == Some(&folder));
                    if resting && cx.has_active_drag() && !s.expanded.contains(&folder) {
                        s.toggle_folder(&folder, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Copy what was dropped from Finder into `dir`, off the main thread, then
    /// show it: the folder opens and the listings are read again.
    pub(super) fn drop_files(&mut self, paths: Vec<PathBuf>, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() || !dir.is_dir() {
            return;
        }
        let point = window.mouse_position();
        cx.spawn(async move |this, cx| {
            let copied = cx
                .background_executor()
                .spawn({
                    let dir = dir.clone();
                    async move { paths.iter().map(|p| crate::copy::copy_into(p, &dir)).collect::<Vec<_>>() }
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                let roots = s.current_project().roots();
                // Open the folder and those above it, so what landed shows.
                for folder in dir.ancestors().take_while(|d| roots.iter().any(|r| d.starts_with(r))) {
                    if !roots.contains(&folder.to_path_buf()) || roots.len() > 1 {
                        s.expanded.insert(folder.to_path_buf());
                    }
                }
                s.refresh_listings(cx);
                let failed: Vec<String> = copied.into_iter().filter_map(Result::err).map(|e| e.to_string()).collect();
                match failed.as_slice() {
                    [] => {}
                    [one] => s.notice(format!("Couldn't copy: {one}"), point, cx),
                    many => s.notice(format!("Couldn't copy {} items: {}", many.len(), many[0]), point, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the Files page's showing folders again: the project's roots and the
    /// folders open beneath them, so files made, deleted or checked out show.
    pub(super) fn refresh_listings(&mut self, cx: &mut Context<Self>) {
        let roots = self.current_project().roots();
        let showing: Vec<(PathBuf, bool)> = self
            .listings
            .keys()
            .filter(|dir| roots.contains(dir) || (self.expanded.contains(*dir) && roots.iter().any(|r| dir.starts_with(r))))
            .map(|dir| {
                // A folder git ignores passes that on to what's inside.
                let ignored = dir
                    .parent()
                    .and_then(|parent| self.listings.get(parent))
                    .and_then(|entries| entries.iter().find(|e| &e.path == dir))
                    .is_some_and(|e| e.ignored);
                (dir.clone(), ignored)
            })
            .collect();
        if showing.is_empty() {
            return;
        }
        self.listings_revision += 1;
        let revision = self.listings_revision;
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { showing.into_iter().map(|(dir, ignored)| { let entries = list(&dir, ignored); (dir, entries) }).collect::<Vec<_>>() })
                .await;
            let _ = this.update(cx, |s, cx| {
                // A later read has started: it knows better.
                if s.listings_revision != revision {
                    return;
                }
                let mut changed = false;
                for (dir, entries) in read {
                    // A folder gone since lists as empty; its parent drops it.
                    if s.listings.get(&dir) != Some(&entries) {
                        s.listings.insert(dir, entries);
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ── Popovers (as in Gitflow) ────────────────────────────────────────────

    /// Open a menu at its trigger: below it, or above when `corner` is a bottom one.
    #[allow(clippy::too_many_arguments)]
    fn open_popover(
        &mut self,
        anchor: &'static str,
        fallback: Point<Pixels>,
        corner: Corner,
        width: f32,
        search: Option<&str>,
        build: popover::Builder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| Menu::new(build, search, px(width), window, cx));
        menu.update(cx, |m, cx| m.focus(window, cx));
        cx.subscribe_in(&menu, window, |s, _, _: &DismissEvent, window, cx| {
            s.popup = None;
            s.dismissed = s.popup_anchor.take().map(|a| (a, Instant::now()));
            // Back to where the menu was opened from.
            if s.prefs_open {
                s.prefs_focus.focus(window);
            } else if s.renaming.is_some() {
                // The menu's Rename item has just put the caret in the name field.
            } else if s.dismissed.is_some_and(|(a, _)| a == lsp::LOCATIONS) {
                // Back to the code the list was opened from.
                s.focus.focus(window);
            } else {
                s.composer.update(cx, |c, cx| c.focus(window, cx));
            }
            cx.notify();
        })
        .detach();
        self.popup_anchor = Some(anchor);
        self.popup_corner = corner;
        self.popup_position = self
            .anchors
            .borrow()
            .get(anchor)
            .map(|b| match corner {
                Corner::TopLeft => b.bottom_left() + point(px(0.), px(4.)),
                Corner::TopRight => b.bottom_right() + point(px(0.), px(4.)),
                Corner::BottomLeft => b.origin - point(px(0.), px(4.)),
                Corner::BottomRight => b.top_right() - point(px(0.), px(4.)),
            })
            .unwrap_or(fallback);
        self.popup = Some(menu);
        cx.notify();
    }
}

/// Instruction files agents conventionally read: the project's own, then the
/// ones in your home folder that Claude Code and Codex read for every project.
fn instruction_files(root: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let project = root.into_iter().flat_map(|r| {
        ["AGENTS.md", "CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"].map(|name| (r.join(name), "project"))
    });
    let global = home.into_iter().flat_map(|h| {
        [(h.join(".claude/CLAUDE.md"), "global · Claude"), (h.join(".codex/AGENTS.md"), "global · Codex")]
    });
    project.chain(global).filter(|(path, _)| path.is_file()).collect()
}

/// A folder's entries: folders first, then files, each by name.
/// A folder's entries, folders first. With `ignored`, the folder is itself
/// ignored, and so is everything in it.
fn list(dir: &Path, ignored: bool) -> Vec<Entry> {
    // What a gitignore-aware walk one level down keeps; the rest git ignores.
    // It reads every `.gitignore` from the repository's root down, the
    // repository's own excludes and the user's global ones.
    let kept: HashSet<PathBuf> = if ignored {
        HashSet::new()
    } else {
        ignore::WalkBuilder::new(dir).max_depth(Some(1)).hidden(false).build().flatten().map(|e| e.into_path()).collect()
    };
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            (!SKIPPED.contains(&name.as_str())).then(|| Entry {
                // A folder git keeps can still hold only what it ignores.
                ignored: ignored || !kept.contains(&path) || (dir && holds_only_ignored(&path)),
                dir,
                path,
                name: name.into(),
            })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    entries
}

/// Whether a folder has something in it, but no file anywhere beneath it that
/// git doesn't ignore. The walk skips ignored folders, and stops at the first
/// file it keeps, so it stays short either way.
fn holds_only_ignored(dir: &Path) -> bool {
    let has_any = std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some());
    has_any
        && !ignore::WalkBuilder::new(dir)
            .hidden(false)
            .filter_entry(|e| !SKIPPED.contains(&e.file_name().to_string_lossy().as_ref()))
            .build()
            .flatten()
            .any(|e| e.file_type().is_some_and(|t| !t.is_dir()))
}

/// Rows of a file lit after a link jumped to them, fading out from `at`.
pub struct Flash {
    pub path: PathBuf,
    pub rows: std::ops::RangeInclusive<usize>,
    pub at: std::time::Instant,
}
impl Flash {
    const HOLD: f32 = 0.7;
    const FADE: f32 = 0.8;
    /// How lit the rows are now, from 1 down to 0.
    pub fn strength(&self) -> f32 {
        let t = self.at.elapsed().as_secs_f32();
        (1. - (t - Self::HOLD).max(0.) / Self::FADE).clamp(0., 1.)
    }
}

/// An open file: its lines and, where a grammar fits, their syntax colours.
pub struct FileView {
    /// The text as read, which language servers count positions in.
    pub text: String,
    pub lines: Vec<SharedString>,
    /// The longest line, which sets how far the view scrolls sideways.
    pub longest: usize,
    pub highlights: Option<syntax::Highlights>,
    /// The indent step in columns, and how many indent guides each line has.
    pub guides: (usize, Vec<u16>),
}

fn read_lines(path: &Path) -> Result<Rc<FileView>, SharedString> {
    let size = std::fs::metadata(path).map_err(|e| SharedString::from(e.to_string()))?.len();
    if size > MAX_FILE {
        return Err("This file is too large to show here.".into());
    }
    let text = std::fs::read_to_string(path).map_err(|_| SharedString::from("This file isn't text."))?;
    let lines = syntax::lines(&text);
    let highlights = syntax::language(&path.to_string_lossy()).and_then(|lang| syntax::highlight(lang, &lines));
    let longest = lines.iter().enumerate().max_by_key(|(_, l)| l.chars().count()).map_or(0, |(i, _)| i);
    let guides = syntax::indent_guides(&lines);
    Ok(Rc::new(FileView { text, lines, longest, highlights, guides }))
}

impl Workspace {
    /// The field a project's new name is typed in, floating by the project.
    fn rename_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        ui::col()
            .id("rename-project")
            .occlude()
            .w(px(240.))
            .p_2()
            .gap_1()
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|s, _, window, cx| s.finish_rename(false, window, cx)))
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    s.finish_rename(false, window, cx);
                }
            }))
            .child(ui::label(colors, "Rename project").px_1())
            .child(gpui_component::Sizable::small(gpui_component::input::Input::new(&self.rename_input)))
            .child(ui::label(colors, "↵ to save · esc to cancel").px_1().text_size(px(crate::theme::scaled_ui(11.))).text_color(rgb(colors.quiet)))
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::resizable::{h_resizable, resizable_panel};
        // With nothing focused, keys would stop at the window root; the workspace takes them instead.
        if window.focused(cx).is_none() {
            self.focus.focus(window);
        }
        self.prepare_ask_inputs(window, cx);
        if std::mem::take(&mut self.focus_ask) {
            // Not while a message is being typed: the card can wait for a click or Tab.
            let typing = self.composer.read(cx).focus_handle(cx).is_focused(window) && !self.composer.read(cx).value().trim().is_empty();
            if !typing && let Some((thread, ask)) = self.waiting_ask().map(|(t, a)| (t, a.id)) {
                self.ask_cursor = Some((thread, ask, 0, 0));
                self.ask_cursor_start(window, cx);
            }
        }
        if std::mem::take(&mut self.clear_composer) {
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if std::mem::take(&mut self.restore_draft) {
            let (text, mentions) = self.draft_owner.and_then(|o| self.drafts.remove(&o)).unwrap_or_default();
            *self.file_mentions.borrow_mut() = mentions;
            self.composer.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        if std::mem::take(&mut self.restore_project_inputs) {
            let (commit, search) = self.project_inputs.remove(&self.project).unwrap_or_default();
            self.commit_message.update(cx, |s, cx| s.set_value(commit, window, cx));
            self.thread_search.update(cx, |s, cx| s.set_value(search, window, cx));
        }
        if let Some(message) = self.restore_commit_message.take() {
            if self.commit_message.read(cx).value().trim().is_empty() {
                self.commit_message.update(cx, |s, cx| s.set_value(message, window, cx));
            }
        }
        if let Some((folder, message)) = self.written_commit_message.take() {
            if self.git_tree().as_deref() == Some(folder.as_path()) {
                self.commit_message.update(cx, |s, cx| s.set_value(message, window, cx));
            }
        }
        if let Some(committed) = self.clear_commit_message.take() {
            if self.commit_message.read(cx).value().as_ref() == committed {
                self.commit_message.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
        // Instruction files are looked for once per project.
        let root = self.current_project().path.clone();
        if !self.instructions.contains_key(&root) {
            self.instructions.insert(root.clone(), instruction_files(root.as_deref()));
        }
        // The Files page lists the project root as soon as it is first shown.
        if let Some(root) = self.current_project().path.clone()
            && !self.listings.contains_key(&root)
        {
            self.listings.insert(root.clone(), list(&root, false));
        }
        let colors = palette(cx);
        let center = ui::col()
            .size_full()
            .min_w_0()
            .child(self.tabs_view(window, cx))
            .child(div().flex_1().min_h_0().child(self.content(window, cx)));
        let body = if self.side_open {
            h_resizable("side-panels")
                .with_state(&self.side_panels)
                .child(resizable_panel().size_range(px(420.)..Pixels::MAX).child(center))
                .child(
                    resizable_panel()
                        .size(px(300.))
                        .size_range(px(240.)..px(480.))
                        .child(self.side_view(cx)),
                )
                .into_any_element()
        } else {
            center.into_any_element()
        };
        let main = if self.dashboard_open() {
            self.dashboard_view(cx)
        } else if self.threads_open {
            h_resizable("thread-panels")
                .with_state(&self.thread_panels)
                .child(
                    resizable_panel()
                        .size(px(260.))
                        .size_range(px(200.)..px(400.))
                        .child(self.threads_view(cx)),
                )
                .child(resizable_panel().size_range(px(480.)..Pixels::MAX).child(body))
                .into_any_element()
        } else {
            body
        };
        ui::col()
            .relative()
            .size_full()
            .bg(rgb(colors.background))
            .text_color(rgb(colors.text))
            .font_family(crate::theme::ui_font())
            .text_size(px(crate::theme::ui_size()))
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_thread))
            .on_action(cx.listener(Self::add_project))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::add_to_chat))
            .on_action(cx.listener(|s, _: &ToggleThreads, _, cx| {
                s.threads_open = !s.threads_open;
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &ToggleSide, _, cx| {
                s.side_open = !s.side_open;
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &CloseTab, _, cx| s.close(s.active, cx)))
            .on_action(cx.listener(|s, _: &NextTab, _, cx| s.cycle(1, cx)))
            .on_action(cx.listener(|s, _: &PreviousTab, _, cx| s.cycle(-1, cx)))
            .on_action(cx.listener(|s, _: &ShowContext, _, cx| s.show_side(Side::Context, cx)))
            .on_action(cx.listener(|s, _: &ShowChanges, _, cx| s.show_side(Side::Changes, cx)))
            .on_action(cx.listener(|s, _: &ShowFiles, _, cx| s.show_side(Side::Files, cx)))
            .on_action(cx.listener(|s, _: &OpenTerminal, window, cx| s.open_terminal(window, cx)))
            .on_action(cx.listener(Self::open_prefs))
            .on_action(cx.listener(|s, _: &GoToDefinition, window, cx| s.go_to_definition(window, cx)))
            .on_action(cx.listener(|s, _: &FindReferences, window, cx| s.find_references(window, cx)))
            .on_action(cx.listener(|s, _: &GoBack, _, cx| s.go_back(cx)))
            .on_action(cx.listener(|s, _: &GoForward, _, cx| s.go_forward(cx)))
            .on_action(cx.listener(|s, _: &AskUp, window, cx| s.ask_step(false, window, cx)))
            .on_action(cx.listener(|s, _: &AskDown, window, cx| s.ask_step(true, window, cx)))
            .on_action(cx.listener(|s, _: &AskNext, window, cx| s.ask_jump(true, window, cx)))
            .on_action(cx.listener(|s, _: &AskPrevious, window, cx| s.ask_jump(false, window, cx)))
            .on_action(cx.listener(|s, _: &AskPick, window, cx| s.ask_pick(window, cx)))
            .on_action(cx.listener(|s, _: &AskSubmit, window, cx| s.ask_finish(false, window, cx)))
            .on_action(cx.listener(|s, _: &AskSkip, window, cx| s.ask_finish(true, window, cx)))
            .on_action(cx.listener(|s, _: &FindInThread, window, cx| s.open_thread_find(window, cx)))
            .on_action(cx.listener(|s, _: &FindNext, _, cx| s.thread_find_step(true, cx)))
            .on_action(cx.listener(|s, _: &FindPrevious, _, cx| s.thread_find_step(false, cx)))
            .on_action(cx.listener(|s, _: &CloseFind, window, cx| s.close_thread_find(window, cx)))
            .on_action(cx.listener(|s, _: &FindInFiles, window, cx| s.open_file_find(window, cx)))
            .on_action(cx.listener(|s, _: &CloseFileFind, window, cx| s.close_file_find(window, cx)))
            .on_action(cx.listener(|s, _: &OpenFound, _, cx| s.open_found(cx)))
            .on_action(cx.listener(|s, _: &FoundUp, _, cx| s.file_find_step(-1, cx)))
            .on_action(cx.listener(|s, _: &FoundDown, _, cx| s.file_find_step(1, cx)))
            // Letting go of ⌘ takes the link away from the word under the pointer.
            .on_modifiers_changed(cx.listener(|s, e: &ModifiersChangedEvent, _, cx| s.lsp_modifiers(e.modifiers.platform, cx)))
            // Escape interrupts the agent, as the transcript says it will.
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
                let key = e.keystroke.key.as_str();
                // ⌘ C copies the transcript's selection once a click there took focus from the composer.
                if key == "c" && e.keystroke.modifiers.platform && s.focus.is_focused(window) {
                    // What's selected in a hover card comes first: it was selected last.
                    let card = s.servers.card_selection.borrow().text().filter(|_| s.servers.hover.is_some());
                    if let Some(text) = card.or_else(|| s.code_text_selected()).or_else(|| s.selection.borrow().text()) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }
                // Typing or pasting with nothing else taking keys starts the message in the composer.
                let m = e.keystroke.modifiers;
                let typing_here = s.focus.is_focused(window) && matches!(s.tabs[s.active].kind, TabKind::Chat);
                if typing_here && key == "v" && m.platform && !(m.control || m.alt || m.function || m.shift) {
                    cx.stop_propagation();
                    s.composer.update(cx, |c, cx| c.focus(window, cx));
                    // The composer's own paste, so it behaves as if it had been focused.
                    window.dispatch_action(Box::new(gpui_component::input::Paste), cx);
                    return;
                }
                if typing_here
                    && !(m.platform || m.control || m.function)
                    && let Some(text) = e.keystroke.key_char.as_ref().filter(|t| !t.chars().any(char::is_control))
                {
                    let text = text.clone();
                    cx.stop_propagation();
                    s.composer.update(cx, |c, cx| {
                        c.focus(window, cx);
                        c.insert(text, window, cx);
                    });
                    return;
                }
                if key != "escape" {
                    return;
                }
                if s.servers.hover.as_ref().is_some_and(|h| h.text.is_some()) {
                    s.close_card(cx);
                } else if s.remote.open {
                    s.close_remote(cx);
                } else if s.effort_open {
                    s.close_effort(cx);
                } else if s.usage_open() {
                    s.close_usage(cx);
                } else if s.dashboard_open() {
                    s.close_dashboard(cx);
                } else if s.line_selection.is_some() {
                    s.line_selection = None;
                    cx.notify();
                } else if !s.code_text.borrow().is_empty() {
                    s.code_text.borrow_mut().clear();
                    cx.notify();
                } else if s.symbol.is_some() {
                    s.symbol = None;
                    cx.notify();
                } else if !s.selection.borrow().is_empty() {
                    s.selection.borrow_mut().clear();
                    cx.notify();
                } else if s.popup.is_none() && s.current_thread().is_some_and(|t| t.state.busy()) {
                    s.stop(cx);
                }
            }))
            .child(self.title(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.rail(cx))
                    .child(div().flex_1().min_w_0().child(main)),
            )
            .child(self.status(cx))
            .when(self.prefs_open, |root| root.child(self.preferences_view(cx)))
            .when_some(self.effort_open.then(|| self.effort_view(cx)).flatten(), |root, view| {
                let at = self.anchors.borrow().get("effort").map(|b| b.top_right() - point(px(0.), px(6.)));
                root.child(
                    deferred(
                        anchored()
                            .anchor(Corner::BottomRight)
                            .position(at.unwrap_or_default())
                            .snap_to_window_with_margin(px(8.))
                            .child(view),
                    )
                    .with_priority(2),
                )
            })
            .when(self.remote.open, |root| {
                let at = self.anchors.borrow().get(remote::ANCHOR).map(|b| b.bottom_right() + point(px(6.), px(0.)));
                root.child(
                    deferred(
                        anchored()
                            .anchor(Corner::BottomLeft)
                            .position(at.unwrap_or_default())
                            .snap_to_window_with_margin(px(8.))
                            .child(self.remote_view(cx)),
                    )
                    .with_priority(2),
                )
            })
            .when_some(self.usage_open().then(|| self.usage_view(cx)).flatten(), |root, view| {
                let at = self.anchors.borrow().get("usage").map(|b| b.top_right() - point(px(0.), px(6.)));
                root.child(
                    deferred(
                        anchored()
                            .anchor(Corner::BottomRight)
                            .position(at.unwrap_or_default())
                            .snap_to_window_with_margin(px(8.))
                            .child(view),
                    )
                    .with_priority(2),
                )
            })
            .when_some(self.renaming.is_some().then(|| self.rename_view(cx)), |root, view| {
                root.child(
                    deferred(
                        anchored()
                            .anchor(Corner::TopLeft)
                            .position(self.rename_position)
                            .snap_to_window_with_margin(px(8.))
                            .child(view),
                    )
                    .with_priority(3),
                )
            })
            .when_some(self.new_project_view(cx), |root, view| root.child(view))
            .when_some(self.lsp_overlay(cx), |root, overlay| root.child(overlay))
            .when_some(self.mission_overlay(cx), |root, card| root.child(card))
            .when_some(self.popup.clone(), |root, menu| {
                root.child(
                    deferred(
                        anchored()
                            .anchor(self.popup_corner)
                            .position(self.popup_position)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(2),
                )
            })
    }
}

#[cfg(test)]
mod listing_tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
    use super::list;
    use std::path::Path;

    fn ignored(entries: &[super::Entry], name: &str) -> bool {
        entries.iter().find(|e| e.name.as_ref() == name).unwrap_or_else(|| panic!("{name} isn't listed")).ignored
    }

    #[test]
    fn marks_what_git_ignores() {
        let dir = std::env::temp_dir().join(format!("chat-ignored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in [
            (".gitignore", "build/\n*.log\n"),
            ("main.rs", ""),
            ("debug.log", ""),
            ("build/out.txt", ""),
            ("nested/.gitignore", "secret.txt\n"),
            ("nested/secret.txt", ""),
            ("nested/kept.txt", ""),
            ("cache/.gitignore", "*\n"),
            ("cache/blob.bin", ""),
            ("logs/today.log", ""),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let git = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&dir).output().unwrap().status.success());
        git(&["init", "-q"]);

        let top = list(&dir, false);
        assert!(!ignored(&top, "main.rs") && !ignored(&top, ".gitignore") && !ignored(&top, "nested"));
        assert!(ignored(&top, "debug.log") && ignored(&top, "build"));
        // Folders git keeps, but whose every file it ignores, fade too.
        assert!(ignored(&top, "cache") && ignored(&top, "logs"));
        // Nested rules count in their own folder.
        let nested = list(&dir.join("nested"), false);
        assert!(ignored(&nested, "secret.txt") && !ignored(&nested, "kept.txt"));
        // Inside an ignored folder, everything is.
        assert!(ignored(&list(&dir.join("build"), true), "out.txt"));
        // Outside a repository nothing is ignored.
        let loose = std::env::temp_dir().join(format!("chat-loose-{}", std::process::id()));
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(loose.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(loose.join("x.log"), "").unwrap();
        assert!(!ignored(&list(Path::new(&loose), false), "x.log"));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&loose).unwrap();
    }
}

/// How many choices a question offers the cursor; none for a typed answer.
fn choice_count(kind: &agent::QuestionKind) -> usize {
    choice_values(kind).len()
}

/// The values of a question's choices, in the order they're drawn.
fn choice_values(kind: &agent::QuestionKind) -> Vec<String> {
    match kind {
        agent::QuestionKind::Single(c) | agent::QuestionKind::Multi(c) => c.iter().map(|c| c.value.clone()).collect(),
        agent::QuestionKind::Yes => vec!["true".into(), "false".into()],
        _ => Vec::new(),
    }
}

/// The model and effort among a session's settings, as picks.
fn model_and_effort(settings: &[Setting]) -> Vec<(String, String)> {
    settings.iter().filter(|s| matches!(s.role, Role::Model | Role::Effort)).map(|s| (s.id.clone(), s.current.clone())).collect()
}
