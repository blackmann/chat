//! The window's state: which project and thread are showing, the open tabs and
//! the panels around them. Views live in the submodules.
mod chat;
mod commands;
mod effort;
mod file_kind;
mod chrome;
mod md;
mod mentions;
mod panes;
mod preferences;
mod select;
mod side;
mod git_panel;
mod syntax;
mod terminal;
pub use terminal::init_terminal_keys;
mod ui;

use crate::{
    agent::{self, Command, Event, Role, Setting},
    store,
    menu::{self as popover, Menu},
    model::{Block, Project, Session, Thread, ThreadState, tilde},
    settings::{ActivityMode, FollowUpMode, Settings},
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
    ]
);

#[derive(Clone, PartialEq)]
pub enum TabKind {
    Chat,
    File(PathBuf),
    Diff(PathBuf),
    /// One side of a file's change, from the Git panel: staged (HEAD → index) or not.
    GitDiff(PathBuf, bool),
    Terminal,
}

pub struct Tab {
    pub id: usize,
    pub kind: TabKind,
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

/// What a project had showing when you left it, put back when you return. The
/// terminal is kept here too, so its shell goes on running in the background.
struct ProjectView {
    tabs: Vec<Tab>,
    active: usize,
    expanded: HashSet<PathBuf>,
    terminal: Option<Result<Entity<terminal::TerminalView>, SharedString>>,
    /// The thread that was showing, by id: indexes shift as threads are added.
    thread: Option<usize>,
}

/// A file or folder in the Files page.
#[derive(Clone)]
pub struct Entry {
    pub name: SharedString,
    pub path: PathBuf,
    pub dir: bool,
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
    /// Folder listings, read once when a folder is first opened.
    listings: HashMap<PathBuf, Vec<Entry>>,
    /// The lines of each file open in a tab, or why it couldn't be shown.
    files: HashMap<PathBuf, Result<Rc<FileView>, SharedString>>,
    /// Where each open file is scrolled, so a link can jump to a line.
    file_scroll: HashMap<PathBuf, UniformListScrollHandle>,
    /// Lines a link jumped to, lit briefly so the eye finds them.
    flash: Option<Flash>,
    terminal: Option<Result<Entity<terminal::TerminalView>, SharedString>>,
    /// Projects not showing, by index, as they were left.
    views: HashMap<usize, ProjectView>,
    thread_search: Entity<InputState>,
    composer: Entity<InputState>,
    /// Dropped files displayed as basename chips in the composer. Their paths
    /// are substituted only in the prompt sent to the agent.
    file_mentions: Rc<RefCell<Vec<FileMention>>>,
    command_index: usize,
    command_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
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
    git_snapshots: HashMap<PathBuf, Result<crate::git::Snapshot, String>>,
    git_diffs: HashMap<(PathBuf, bool), Result<crate::git::Diff, String>>,
    git_refreshing: bool,
    /// Something changed during a refresh: read again once it lands.
    git_queued: bool,
    git_watch: Option<crate::git_watch::Watcher>,
    git_busy: bool,
    git_revision: usize,
    git_errors: HashMap<PathBuf, String>,
    thread_changes_only: bool,
    commit_message: Entity<InputState>,
    clear_commit_message: Option<String>,
    /// An undone commit's message, to put back in an empty commit box.
    restore_commit_message: Option<String>,
    /// Which Git operation is running, so its button can say so.
    git_running: Option<GitRunning>,
    /// The repository the Changes page shows, when a project spans several folders.
    git_repo: Option<PathBuf>,
    /// Text selected in the transcript.
    selection: select::Shared,
    /// Lines selected in a file or diff pane.
    line_selection: Option<panes::LineSelection>,
    /// The reasoning effort slider is showing.
    effort_open: bool,
    /// Where the effort slider's track was last painted, to map the pointer to a step.
    effort_track: Rc<std::cell::Cell<Bounds<Pixels>>>,
    effort_dragging: bool,
    /// The project being renamed, and the field its new name is typed in.
    renaming: Option<usize>,
    rename_input: Entity<InputState>,
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
    _quit: Option<Subscription>,
}

/// A Git operation in progress, as far as the commit box cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitRunning {
    Commit,
    Push,
    Undo,
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
        cx.observe(&thread_search, |_, _, cx| cx.notify()).detach();
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        cx.subscribe_in(&rename_input, window, |s, _, e: &gpui_component::input::InputEvent, window, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = e {
                s.finish_rename(true, window, cx);
            }
        })
        .detach();
        let settings = Settings::load();
        let commit_message = git_panel::commit_input(settings.commit_box_height.is_some(), window, cx);
        let mut projects = vec![Project::chats()];
        projects.extend(settings.projects.iter().filter(|p| p.path.is_dir()).cloned().map(Project::open));
        let mut next_thread = 0;
        for project in &mut projects {
            project.threads = store::load(project, &settings.agents, settings.agent, &mut next_thread);
        }
        let mut workspace = Self {
            focus: cx.focus_handle(),
            // Open on the first real project when there is one.
            project: projects.len().min(2) - 1,
            settings,
            projects,
            thread: 0,
            next_thread,
            tabs: vec![Tab { id: 0, kind: TabKind::Chat }],
            active: 0,
            next_tab: 1,
            side: Side::Context,
            threads_open: true,
            side_open: true,
            expanded: HashSet::new(),
            listings: HashMap::new(),
            files: HashMap::new(),
            file_scroll: HashMap::new(),
            flash: None,
            terminal: None,
            views: HashMap::new(),
            thread_search,
            composer,
            file_mentions,
            command_index: 0,
            tabs_scroll: ScrollHandle::new(),
            tab_in_view: std::cell::Cell::new(None),
            command_scroll: ScrollHandle::new(),
            thread_scroll: ScrollHandle::new(),
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
            git_queued: false,
            git_watch: None,
            git_busy: false,
            git_revision: 0,
            git_errors: HashMap::new(),
            thread_changes_only: false,
            commit_message,
            clear_commit_message: None,
            restore_commit_message: None,
            git_running: None,
            git_repo: None,
            selection: Rc::default(),
            line_selection: None,
            effort_open: false,
            effort_track: Rc::default(),
            effort_dragging: false,
            renaming: None,
            rename_input,
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
            commit_height_unsaved: false,
            window_active: window.is_window_active(),
            _quit: None,
        };
        // Whatever hasn't been written yet is written on the way out.
        workspace._quit = Some(cx.on_app_quit(|s, _| {
            s.save_now();
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
        // Reopen where things were left: the project's latest thread.
        if workspace.current_project().threads.is_empty() {
            workspace.start_thread(cx);
        } else {
            workspace.select_thread(0, cx);
        }
        workspace.start_git_watch(window, cx);
        cx.observe_window_activation(window, |s, window, cx| {
            s.window_active = window.is_window_active();
            // Coming back to the window is seeing the thread on show.
            if s.window_active && let Some(t) = s.current_thread_mut() && t.unread {
                t.unread = false;
                s.save_soon(cx);
                cx.notify();
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
        if self.project == index {
            return;
        }
        // Tabs, the file tree and the terminal belong to the project that was
        // showing: put them away as they are, and bring back the new one's.
        let leaving = ProjectView {
            tabs: std::mem::take(&mut self.tabs),
            active: self.active,
            expanded: std::mem::take(&mut self.expanded),
            terminal: self.terminal.take(),
            thread: self.current_thread().map(|t| t.id),
        };
        self.views.insert(self.project, leaving);
        let inputs = (self.commit_message.read(cx).value().to_string(), self.thread_search.read(cx).value().to_string());
        self.project_inputs.insert(self.project, inputs);
        self.restore_project_inputs = true;
        self.project = index;
        self.git_repo = None;
        let back = self.views.remove(&index);
        let thread = back.as_ref().and_then(|v| v.thread);
        match back {
            Some(view) => {
                self.tabs = view.tabs;
                self.expanded = view.expanded;
                self.terminal = view.terminal;
                self.active = view.active.min(self.tabs.len().saturating_sub(1));
            }
            None => {
                self.tabs = vec![Tab { id: self.next_tab, kind: TabKind::Chat }];
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
            None => self.select_thread(0, cx),
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
    fn select_thread(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.current_project().threads.get(index).map(|t| t.id);
        if id != self.draft_owner {
            self.stash_draft(cx);
            self.draft_owner = id;
            // The composer is refilled on the next render, which has the window it needs.
            self.restore_draft = true;
        }
        self.thread = index;
        self.selection.borrow_mut().clear();
        self.active = 0;
        self.follow = true;
        self.transcript.scroll_to_bottom();
        if let Some(t) = self.current_thread_mut() {
            t.unread = false;
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
        if let Some(blank) = self.current_project().threads.iter().position(|t| t.blocks.is_empty()) {
            return self.select_thread(blank, cx);
        }
        self.next_thread += 1;
        let thread = Thread::new(self.next_thread, self.settings.agent);
        self.projects[self.project].threads.insert(0, thread);
        self.select_thread(0, cx);
    }
    fn new_thread(&mut self, _: &NewThread, window: &mut Window, cx: &mut Context<Self>) {
        self.start_thread(cx);
        self.composer.update(cx, |s, cx| s.focus(window, cx));
    }
    fn add_project(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |s, cx| {
                for path in paths {
                    match s.projects.iter().position(|p| p.path.as_deref() == Some(&path)) {
                        Some(existing) => s.select_project(existing, cx),
                        None => {
                            let entry = crate::settings::ProjectEntry { path, name: None, folders: Vec::new() };
                            let mut project = Project::open(entry);
                            // A folder added back keeps the threads it had.
                            project.threads = store::load(&project, &s.settings.agents, s.settings.agent, &mut s.next_thread);
                            s.projects.push(project);
                            s.select_project(s.projects.len() - 1, cx);
                        }
                    }
                }
                s.save_projects();
                cx.notify();
            });
        })
        .detach();
    }
    /// Write the rail back to settings: paths, names and folders.
    fn save_projects(&mut self) {
        self.settings.projects = self.projects.iter().filter_map(Project::entry).collect();
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
        let t = &mut self.projects[project].threads[thread];
        let Some(spec) = self.settings.agents.get(t.agent).cloned() else {
            return;
        };
        let id = t.id;
        // Capture the latest choices before clearing the controls for reconnect.
        t.picks = t.current_picks();
        let (commands, mut events, run) = agent::connect(spec, cwd, extra, t.session_id.clone(), t.picks.clone());
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
        if t.state == ThreadState::Failed && t.blocks.is_empty() {
            t.state = ThreadState::Idle;
        }
        t.settings.clear();
        t.session = Some(Session { commands, _driver: driver, _listener: listener });
        self.live.retain(|l| *l != id);
        self.live.push(id);
        self.stop_idle_agents();
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
            let Ok(snapshots) = this.update(cx, |s, _| {
                s.saving = false;
                s.snapshots()
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move {
                    for (path, bytes) in snapshots {
                        let _ = store::write(&path, &bytes);
                    }
                })
                .await;
        })
        .detach();
    }
    fn snapshots(&self) -> Vec<(PathBuf, Vec<u8>)> {
        self.projects.iter().filter_map(|p| store::snapshot(p, &self.settings.agents)).collect()
    }
    fn save_now(&mut self) {
        for (path, bytes) in self.snapshots() {
            let _ = store::write(&path, &bytes);
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
        let was_busy = thread.state.busy();
        thread.apply(event);
        // A turn that finishes out of sight waits to be read.
        if was_busy && matches!(thread.state, ThreadState::Done | ThreadState::Failed) && !seen {
            thread.unread = true;
        }
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
        if let Some(thread) = self.current_thread_mut()
            && thread.blocks.is_empty()
            && thread.agent != agent
        {
            thread.agent = agent;
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
        self.save_soon(cx);
        cx.notify();
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
        let waiting = thread.blocks.iter().any(|b| matches!(b, Block::Permission(p) if p.chosen.is_none()));
        if !waiting && thread.state == ThreadState::Waiting {
            thread.state = ThreadState::Running;
        }
        cx.notify();
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
        if kind == TabKind::Terminal && self.terminal.is_none() {
            let cwd = self.current_project().path.clone().unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())));
            self.terminal = Some(terminal::TerminalView::new(cwd, cx).map_err(|e| SharedString::from(e.to_string())));
        }
        if let TabKind::File(path) = &kind {
            self.files.insert(path.clone(), read_lines(path));
            self.file_scroll.entry(path.clone()).or_default();
        }
        self.active = match self.tabs.iter().position(|t| t.kind == kind) {
            Some(i) => i,
            None => {
                self.tabs.push(Tab { id: self.next_tab, kind });
                self.next_tab += 1;
                self.tabs.len() - 1
            }
        };
        self.refresh_git(cx);
        self.focus_terminal(cx);
        cx.notify();
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
            scroll.scroll_to_item(first, ScrollStrategy::Center);
        }
        self.flash = Some(Flash { path, rows: first..=last, at: std::time::Instant::now() });
        cx.notify();
    }
    fn focus_terminal(&self, cx: &mut Context<Self>) {
        if self.tabs[self.active].kind == TabKind::Terminal {
            if let Some(Ok(terminal)) = &self.terminal { terminal.update(cx, |s, cx| { s.request_focus = true; cx.notify(); }); }
        }
    }
    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.get(index).is_none_or(|t| t.kind == TabKind::Chat) {
            return;
        }
        if self.tabs[index].kind == TabKind::Terminal { self.terminal = None; }
        if let TabKind::File(path) = self.tabs.remove(index).kind {
            self.files.remove(&path);
            self.file_scroll.remove(&path);
        }
        if self.active >= index {
            self.active = self.active.saturating_sub(1);
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
            self.listings.insert(path.to_path_buf(), list(path));
        }
        cx.notify();
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
fn list(dir: &Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!SKIPPED.contains(&name.as_str())).then(|| Entry {
                dir: e.file_type().is_ok_and(|t| t.is_dir()),
                path: e.path(),
                name: name.into(),
            })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    entries
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
    Ok(Rc::new(FileView { lines, longest, highlights, guides }))
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
            self.listings.insert(root.clone(), list(&root));
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
        let main = if self.threads_open {
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
            .on_action(cx.listener(|s, _: &OpenTerminal, _, cx| s.open(TabKind::Terminal, cx)))
            .on_action(cx.listener(Self::open_prefs))
            // Escape interrupts the agent, as the transcript says it will.
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
                let key = e.keystroke.key.as_str();
                // ⌘ C copies the transcript's selection once a click there took focus from the composer.
                if key == "c" && e.keystroke.modifiers.platform && s.focus.is_focused(window) {
                    if let Some(text) = s.selection.borrow().text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }
                if key != "escape" {
                    return;
                }
                if s.effort_open {
                    s.close_effort(cx);
                } else if s.line_selection.is_some() {
                    s.line_selection = None;
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
