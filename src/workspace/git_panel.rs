//! The Changes page, after Zed's Git panel: changed files under Staged and
//! Unstaged (a file staged in part is under both), each with a checkbox that
//! moves it across, its line counts on that side, and a commit box underneath.
use super::ui::*;
use super::*;
use crate::{
    git::{self, Operation},
    icons::Glyph,
};
use gpui_component::{Selectable, 
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    scroll::ScrollableElement,
    input::{Input, InputState},
};

/// The commit message field. It grows with its text until the box is given a
/// height by dragging its top edge; then it fills that height and scrolls.
pub(super) fn commit_input(fixed: bool, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<InputState> {
    let input = cx.new(|cx| {
        let input = InputState::new(window, cx).multi_line(true).placeholder("Commit message…");
        if fixed { input.rows(2) } else { input.auto_grow(2, 5) }
    });
    cx.observe(&input, |_, _, cx| cx.notify()).detach();
    input
}

/// The commit box's top edge, while it is dragged. Nothing follows the cursor.
#[derive(Clone)]
struct CommitEdge;
impl Render for CommitEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A raised, bordered button with an optional icon, as in a native toolbar.
fn bar_button(colors: Palette, id: &'static str, glyph: Option<IconName>, text: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    row()
        .id(id)
        .h(px(24.))
        .px(px(9.))
        .gap(px(5.))
        .flex_shrink_0()
        .rounded(px(6.))
        .border_1()
        .border_color(rgb(colors.popover_border))
        .bg(rgb(colors.hover))
        .shadow_xs()
        .text_size(px(crate::theme::scaled_ui(12.5)))
        .text_color(rgb(colors.text))
        .when_some(glyph, |s, g| s.child(icon(g).size(px(13.)).text_color(rgb(colors.muted))))
        .child(text.into())
        .when(enabled, |s| s.cursor_pointer().hover(|s| s.bg(rgb(colors.popover_hover))).active(|s| s.bg(rgb(colors.border))))
        .when(!enabled, |s| s.opacity(0.5))
}

/// What the agent is asked, around the changes.
fn commit_prompt(changes: &str) -> String {
    format!(
        "Write a git commit message for the changes below, following Conventional Commits: \
a `type(scope): summary` subject line (types such as feat, fix, refactor, perf, docs, test, build, ci, chore, style), \
in the imperative, lowercase, no trailing period, at most 72 characters; the scope only when one area clearly owns the change. \
Add a body after a blank line only when the subject can't carry the why, wrapped at 72 columns. \
Answer from the diff alone; don't run tools or read files. Reply with the commit message only: no preamble, \
no quotes, no code fences.\n\n<changes>\n{changes}\n</changes>"
    )
}

/// The reply without fences or a preamble an agent may add anyway.
fn tidy_commit_message(reply: &str) -> Option<String> {
    let mut text = reply.trim();
    if let Some(inner) = text.strip_prefix("```") {
        // Past the fence's language tag, up to the closing fence.
        let inner = inner.split_once('\n').map_or("", |(_, rest)| rest);
        text = inner.rsplit_once("```").map_or(inner, |(body, _)| body).trim();
    }
    let text = text.trim_matches('`').trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A checkbox: on under Staged, off under Unstaged.
fn tick(colors: Palette, id: impl Into<ElementId>, on: bool, enabled: bool) -> Stateful<Div> {
    row()
        .id(id)
        .size(px(14.))
        .flex_shrink_0()
        .justify_center()
        .rounded(px(4.))
        .map(|s| {
            if on {
                s.bg(rgb(colors.blue)).text_color(rgb(colors.primary_text)).child(icon(IconName::Check).size(px(11.)))
            } else {
                s.border_1().border_color(rgb(colors.quiet)).bg(rgb(colors.background))
            }
        })
        .map(|s| if enabled { s.cursor_pointer() } else { s.opacity(0.5) })
}

/// A file's status letter in its change's colour.
fn status_color(colors: Palette, status: u8) -> u32 {
    match status {
        b'A' | b'?' => colors.green,
        b'D' | b'U' => colors.red,
        b'R' | b'C' => colors.blue,
        _ => colors.amber,
    }
}

/// Whether two paths name the same folder, however they are spelled.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
}

/// What a checkbox does: unstage what is under Staged, stage what is under Unstaged.
fn toggle_operation(entries: &[&git::Entry], staged: bool) -> Operation {
    let mut seen = std::collections::HashSet::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        for path in if staged { entry.paths() } else { entry.stage_paths() } {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }
    if staged { Operation::Unstage(paths) } else { Operation::Stage(paths) }
}

/// The Changes page's sections: conflicts are settled in the editor first, then staged from Unstaged.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Conflicts,
    Staged,
    Unstaged,
}
impl Section {
    const ALL: [Section; 3] = [Section::Conflicts, Section::Staged, Section::Unstaged];
    fn title(self) -> &'static str {
        match self {
            Section::Conflicts => "Conflicts",
            Section::Staged => "Staged",
            Section::Unstaged => "Unstaged",
        }
    }
    fn holds(self, e: &git::Entry) -> bool {
        match self {
            Section::Conflicts => e.conflict(),
            Section::Staged => e.staged(),
            Section::Unstaged => e.unstaged() && !e.conflict(),
        }
    }
    fn entries(self, snapshot: &git::Snapshot) -> Vec<&git::Entry> {
        snapshot.entries.iter().filter(|e| self.holds(e)).collect()
    }
}

/// Rows a section draws; an unignored build folder can put tens of thousands
/// of files in one, and drawing them all stalls every frame.
const MAX_ROWS: usize = 300;

impl Workspace {
    /// The folder whose repository the Changes page shows.
    pub(super) fn git_folder(&self) -> Option<PathBuf> {
        let roots = self.current_project().roots();
        self.git_repo.clone().filter(|r| roots.contains(r)).or_else(|| roots.into_iter().next())
    }
    /// The checkout the Changes page shows for a project folder: one picked
    /// here, else the worktree the thread's agent reported, else the folder.
    pub(super) fn git_tree_for(&self, folder: &Path) -> PathBuf {
        let thread = self.current_thread();
        let key = (thread.map(|t| t.id), folder.to_path_buf());
        self.git_picks
            .get(&key)
            .or_else(|| thread?.worktrees.get(folder))
            // A worktree removed since falls back to the folder.
            .filter(|tree| tree.is_dir())
            .cloned()
            .unwrap_or_else(|| folder.to_path_buf())
    }
    pub(super) fn git_tree(&self) -> Option<PathBuf> {
        self.git_folder().map(|f| self.git_tree_for(&f))
    }
    /// Every checkout the Changes page may show: the project's folders and the worktrees chosen for them.
    fn git_trees(&self) -> Vec<PathBuf> {
        let mut trees = self.current_project().roots();
        for folder in trees.clone() {
            let tree = self.git_tree_for(&folder);
            if !trees.contains(&tree) {
                trees.push(tree);
            }
        }
        trees
    }
    pub(super) fn git_snapshot(&self) -> Option<&git::Snapshot> {
        self.git_snapshots.get(&self.git_tree()?)?.as_ref().ok()
    }
    /// Keep the Changes page current: refresh when the file watcher sees
    /// something `git status` would, and whenever the window comes forward, in
    /// case an event was missed. Without a watcher, poll instead.
    pub(super) fn start_git_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match crate::git_watch::Watcher::new() {
            Ok((watcher, mut changes)) => {
                self.git_watch = Some(watcher);
                cx.spawn(async move |this, cx| {
                    while changes.next().await.is_some() {
                        // A save, checkout or build is a burst of events: let it
                        // settle a moment and refresh once for all of it.
                        cx.background_executor().timer(Duration::from_millis(150)).await;
                        let Ok(cost) = this.update(cx, |s, _| s.git_refresh_cost) else { break };
                        let wait = (cost * 2).min(Duration::from_secs(3));
                        if wait > Duration::from_millis(50) {
                            cx.background_executor().timer(wait).await;
                        }
                        while changes.try_recv().is_ok() {}
                        if this.update(cx, |s, cx| s.refresh_git(cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(_) => {
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                        if this.update(cx, |s, cx| s.refresh_git(cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
        }
        cx.observe_window_activation(window, |s, window, cx| {
            if window.is_window_active() {
                s.refresh_git(cx);
            }
        })
        .detach();
        self.refresh_git(cx);
    }
    /// Watch the folders showing, and the repositories found in them.
    fn sync_git_watch(&mut self) {
        let folders = self.git_trees();
        let repos = folders
            .iter()
            .filter_map(|f| self.git_snapshots.get(f)?.as_ref().ok())
            .map(|s| (s.root.clone(), s.git_dir.clone()))
            .collect();
        if let Some(watch) = &mut self.git_watch {
            watch.set(folders, repos);
        }
    }
    /// Read every folder of the project again (each shows its branch), and the
    /// diff in the open tab.
    pub(super) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        // The watcher that wakes this also catches open files changing.
        self.lsp_sync();
        if self.git_refreshing || self.git_busy {
            self.git_queued = true;
            return;
        }
        let roots = self.git_trees();
        if roots.is_empty() {
            self.sync_git_watch();
            return;
        }
        self.git_refreshing = true;
        self.git_queued = false;
        let revision = self.git_revision;
        let selected = match &self.tabs[self.active].kind {
            TabKind::GitDiff(path, staged) => Some((path.clone(), *staged)),
            _ => None,
        };
        let started = std::time::Instant::now();
        cx.spawn(async move |this, cx| {
            let (snapshots, diff) = cx
                .background_executor()
                .spawn(async move {
                    let snapshots: Vec<_> = roots.into_iter().map(|root| {
                        let snapshot = git::snapshot(&root);
                        (root, snapshot)
                    }).collect();
                    let diff = selected.map(|(path, staged)| {
                        let found = snapshots.iter().filter_map(|(_, s)| s.as_ref().ok()).find_map(|s| {
                            let relative = path.strip_prefix(&s.root).ok()?;
                            s.entries
                                .iter()
                                .find(|e| e.path == relative && if staged { e.staged() } else { e.unstaged() })
                                .map(|e| (s, e))
                        });
                        let result = match found {
                            Some((snapshot, entry)) => git::diff(&snapshot.root, entry, staged),
                            None => Err(format!("Nothing {} in this file now.", if staged { "staged" } else { "unstaged" })),
                        };
                        ((path, staged), result)
                    });
                    (snapshots, diff)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.git_refreshing = false;
                s.git_refresh_cost = started.elapsed();
                if revision != s.git_revision {
                    s.refresh_git(cx);
                    return;
                }
                s.git_snapshots.extend(snapshots);
                if let Some((key, diff)) = diff {
                    s.git_diffs.insert(key, diff);
                }
                s.sync_git_watch();
                // The same events add and remove files in the Files page.
                s.refresh_listings(cx);
                // Commits and edits move open files' marks and blame.
                s.diff_blame.borrow_mut().clear();
                for path in s.files.keys().cloned().collect::<Vec<_>>() {
                    s.load_file_git(path, cx);
                }
                if s.git_queued {
                    s.refresh_git(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn git_operation(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.git_busy {
            return;
        }
        let Some(root) = self.git_snapshot().map(|s| s.root.clone()) else {
            return;
        };
        let Some(folder) = self.git_tree() else {
            return;
        };
        self.git_busy = true;
        self.git_running = Some(match &operation {
            Operation::Commit(_) | Operation::Amend { .. } => GitRunning::Commit,
            Operation::Push => GitRunning::Push,
            Operation::UndoCommit => GitRunning::Undo,
            Operation::Stash { .. } | Operation::StashPop(_) | Operation::StashApply(_) | Operation::StashDrop(_) => GitRunning::Stash,
            _ => GitRunning::Other,
        });
        self.stash_drop_armed = None;
        self.discard_armed = None;
        self.git_revision += 1;
        self.git_errors.remove(&folder);
        cx.notify();
        let committed = match &operation {
            Operation::Commit(message) | Operation::Amend { message, .. } => Some(message.clone()),
            _ => None,
        };
        let amended = matches!(operation, Operation::Amend { .. });
        let undone = matches!(operation, Operation::UndoCommit)
            .then(|| self.git_snapshot().and_then(|s| s.last_commit.as_ref()).map(|c| c.message.clone()))
            .flatten();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { git::operate(&root, operation) }).await;
            let _ = this.update(cx, |s, cx| {
                s.git_busy = false;
                s.git_running = None;
                s.git_revision += 1;
                match result {
                    Err(error) => {
                        s.git_errors.insert(folder, error);
                    }
                    Ok(()) => {
                        if amended {
                            s.amend = false;
                            s.amend_drop.clear();
                        }
                        s.clear_commit_message = committed;
                        s.restore_commit_message = undone;
                    }
                }
                s.git_diffs.clear();
                s.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Have the default agent write a Conventional Commits message for what is
    /// staged (or every change, when nothing is) and put it in the commit box.
    /// Asking again while it writes stops it.
    fn write_commit_message(&mut self, cx: &mut Context<Self>) {
        if self.commit_writing.take().is_some() {
            cx.notify();
            return;
        }
        let (Some(root), Some(folder)) = (self.git_snapshot().map(|s| s.root.clone()), self.git_tree()) else {
            return;
        };
        let Some(spec) = self.settings.agents.get(self.settings.agent).cloned() else {
            return;
        };
        self.git_errors.remove(&folder);
        let task = cx.spawn({
            let folder = folder.clone();
            async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let changes = git::changes_for_message(&root, 60_000)?;
                        let reply = crate::agent::ask_once(spec, root, commit_prompt(&changes)).await?;
                        tidy_commit_message(&reply).ok_or_else(|| "The agent didn't write a message".to_string())
                    })
                    .await;
                let _ = this.update(cx, |s, cx| {
                    s.commit_writing = None;
                    match result {
                        Ok(message) => s.written_commit_message = Some((folder, message)),
                        Err(error) => {
                            s.git_errors.insert(folder, error);
                        }
                    }
                    cx.notify();
                });
            }
        });
        self.commit_writing = Some((folder, task));
        cx.notify();
    }

    /// With several folders, which one the Changes page shows.
    fn folder_picker(&self, colors: Palette, cx: &mut Context<Self>) -> Option<Div> {
        let roots = self.current_project().roots();
        if roots.len() < 2 {
            return None;
        }
        let folder = self.git_folder().map(|f| crate::model::folder_name(&f)).unwrap_or_default();
        let trigger = row()
            .id("repo-button")
            .h(px(22.))
            .px(px(6.))
            .gap(px(5.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(colors.hover)))
            .child(icon(IconName::Folder).size(px(13.)).text_color(rgb(colors.muted)))
            .child(div().font_weight(FontWeight::MEDIUM).child(folder))
            .child(icon(IconName::ChevronDown).size(px(11.)).text_color(rgb(colors.quiet)));
        Some(self.popover_trigger("repo", trigger, Corner::TopLeft, 280., None, move |s, weak, _| {
            let current = s.git_folder();
            let mut entries = vec![crate::menu::Entry::Label("Repository".into())];
            for root in s.current_project().roots() {
                let snapshot = s.git_snapshots.get(&s.git_tree_for(&root)).and_then(|r| r.as_ref().ok());
                let mut item = crate::menu::Item::new(crate::model::folder_name(&root))
                    .icon(IconName::Folder)
                    .checked(current.as_ref() == Some(&root));
                if let Some(snapshot) = snapshot {
                    let n = snapshot.entries.len();
                    item = item.detail(if n == 0 { snapshot.branch.clone() } else { format!("{} · {n}", snapshot.branch) });
                }
                let weak = weak.clone();
                entries.push(crate::menu::Entry::Item(item.on_click(move |_, cx| {
                    let _ = weak.update(cx, |s, cx| {
                        s.git_repo = Some(root.clone());
                        s.refresh_git(cx);
                        cx.notify();
                    });
                })));
            }
            entries
        }, cx))
    }

    /// Which checkout of the folder's repository to show. The thread's agent
    /// moves this when it reports working in another worktree.
    fn worktree_picker(&self, colors: Palette, folder: &Path, snapshot: &git::Snapshot, cx: &mut Context<Self>) -> Div {
        let reported = self.current_thread().and_then(|t| t.worktrees.get(folder)).cloned();
        let main = snapshot.worktrees.first().is_none_or(|w| w.path == snapshot.root);
        let name = if main { "main".to_string() } else { crate::model::folder_name(&snapshot.root) };
        let following = reported.as_ref().is_some_and(|r| same_dir(r, &snapshot.root));
        let trigger = row()
            .id("worktree-button")
            .h(px(22.))
            .px(px(6.))
            .gap(px(5.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(colors.hover)))
            .child(icon(Glyph::Worktree).size(px(13.)).text_color(rgb(if main { colors.muted } else { colors.blue })))
            .child(div().font_weight(FontWeight::MEDIUM).text_color(rgb(if main { colors.text } else { colors.blue })).child(name))
            .when(following, |s| s.child(label(colors, "· this thread").text_size(px(crate::theme::scaled_ui(11.)))))
            .child(icon(IconName::ChevronDown).size(px(11.)).text_color(rgb(colors.quiet)));
        let folder = folder.to_path_buf();
        self.popover_trigger("worktree", trigger, Corner::TopRight, 300., None, move |s, weak, _| {
            let Some(snapshot) = s.git_snapshots.get(&s.git_tree_for(&folder)).and_then(|r| r.as_ref().ok()) else {
                return Vec::new();
            };
            let thread = s.current_thread().map(|t| t.id);
            let reported = s.current_thread().and_then(|t| t.worktrees.get(&folder)).cloned();
            let mut entries = vec![crate::menu::Entry::Label("Worktrees".into())];
            for (i, tree) in snapshot.worktrees.iter().enumerate() {
                let theirs = reported.as_ref().is_some_and(|r| same_dir(r, &tree.path)) || (i == 0 && reported.is_none());
                let branch = tree.branch.clone().unwrap_or_else(|| "detached".into());
                let detail = if theirs && reported.is_some() { format!("{branch} · this thread") } else { branch };
                let title = if i == 0 { "main".to_string() } else { crate::model::folder_name(&tree.path) };
                // The main checkout is the folder itself, keyed as the project knows it.
                let target = if i == 0 { folder.clone() } else { tree.path.clone() };
                let (weak, folder) = (weak.clone(), folder.clone());
                let item = crate::menu::Item::new(title)
                    .icon(Glyph::Worktree)
                    .detail(detail)
                    .checked(same_dir(&tree.path, &snapshot.root))
                    .on_click(move |_, cx| {
                        let _ = weak.update(cx, |s, cx| {
                            // Picking what the thread would show anyway drops the pick.
                            let default = s.current_thread().and_then(|t| t.worktrees.get(&folder)).cloned().unwrap_or_else(|| folder.clone());
                            if same_dir(&default, &target) {
                                s.git_picks.remove(&(thread, folder.clone()));
                            } else {
                                s.git_picks.insert((thread, folder.clone()), target.clone());
                            }
                            s.refresh_git(cx);
                            cx.notify();
                        });
                    });
                entries.push(crate::menu::Entry::Item(item));
            }
            entries
        }, cx)
    }

    /// Give the commit box a height (`None` lets it grow with its text again),
    /// swapping the field's mode to match while keeping what was typed.
    fn set_commit_height(&mut self, height: Option<f32>, window: &mut Window, cx: &mut Context<Self>) {
        if height.is_some() != self.settings.commit_box_height.is_some() {
            let text = self.commit_message.read(cx).value().to_string();
            let focused = self.commit_message.read(cx).focus_handle(cx).is_focused(window);
            self.commit_message = commit_input(height.is_some(), window, cx);
            self.commit_message.update(cx, |s, cx| {
                s.set_value(text, window, cx);
                if focused {
                    s.focus(window, cx);
                }
            });
        }
        self.settings.commit_box_height = height;
        self.commit_height_unsaved = true;
        cx.notify();
    }

    fn save_commit_height(&mut self) {
        if std::mem::take(&mut self.commit_height_unsaved) {
            let _ = self.settings.save();
        }
    }

    /// While amending, the files the last commit changed, ticked to keep.
    /// Unticked ones leave the commit on Amend; their changes stay uncommitted.
    fn amend_section(&self, colors: Palette, snapshot: &git::Snapshot, cx: &mut Context<Self>) -> Option<Div> {
        let last = snapshot.last_commit.as_ref().filter(|_| self.amend && !snapshot.pushed())?;
        if last.files.is_empty() {
            return None;
        }
        let enabled = !self.git_busy;
        let kept = last.files.iter().filter(|f| !self.amend_drop.contains(&f.path)).count();
        let all: Vec<PathBuf> = last.files.iter().map(|f| f.path.clone()).collect();
        let (adds, dels) = last
            .files
            .iter()
            .filter(|f| !self.amend_drop.contains(&f.path))
            .filter_map(|f| f.stat)
            .fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
        let mut section = col().child(
            row()
                .h(px(30.))
                .px_3()
                .gap_2()
                .child(tick(colors, "amend-section-tick", kept == all.len(), enabled).when(enabled, |s| {
                    s.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(if kept == 0 { "Keep all" } else { "Take all out" }).build(w, cx))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if kept == 0 {
                                s.amend_drop.clear();
                            } else {
                                s.amend_drop.extend(all.iter().cloned());
                            }
                            cx.notify();
                        }))
                }))
                .child(div().text_size(px(crate::theme::scaled_ui(12.))).font_weight(FontWeight::MEDIUM).text_color(rgb(colors.muted)).child("In last commit"))
                .child(count_badge(colors, kept))
                .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border)))
                .child(stat(colors, adds, dels)),
        );
        for (i, file) in last.files.iter().enumerate() {
            let dropped = self.amend_drop.contains(&file.path);
            let relative = file.path.to_string_lossy().into_owned();
            let (name, dir) = split_path(&relative);
            let path = file.path.clone();
            let tip = if dropped { "Keep in the commit" } else { "Take out of the commit, keeping its changes" };
            section = section.child(
                row()
                    .id(("amend-file", i))
                    .h(px(26.))
                    .mx_1()
                    .px_2()
                    .gap_2()
                    .rounded(px(5.))
                    .when(dropped, |s| s.opacity(0.6))
                    .child(tick(colors, ("amend-tick", i), !dropped, enabled).when(enabled, |s| {
                        s.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if !s.amend_drop.remove(&path) {
                                    s.amend_drop.insert(path.clone());
                                }
                                cx.notify();
                            }))
                    }))
                    .child(
                        line_with_detail(
                            name.to_string(),
                            (!dir.is_empty()).then(|| (SharedString::from(dir.to_string()), Hsla::from(rgb(colors.quiet)))),
                            18.,
                        )
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(if dropped || file.status == b'D' { colors.muted } else { colors.text })),
                    )
                    .when_some(file.stat, |s, (adds, dels)| s.child(stat(colors, adds, dels)))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_shrink_0()
                            .text_center()
                            .font_family(mono())
                            .text_size(px(crate::theme::scaled_mono(11.)))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(status_color(colors, file.status)))
                            .child(char::from(file.status).to_string()),
                    ),
            );
        }
        Some(section)
    }

    /// Throws away a whole section's changes. The first click arms it; the
    /// second discards.
    fn discard_button(&self, id: impl Into<ElementId>, section: Section, cx: &mut Context<Self>) -> Button {
        let staged = section == Section::Staged;
        let armed = self.discard_armed == Some(staged);
        let tip = match (armed, staged) {
            (true, _) => "Click again to discard. Files Git never kept go to the Trash",
            (false, false) => "Discard all unstaged changes",
            (false, true) => "Discard all changes to these files, staged and unstaged",
        };
        Button::new(id)
            .ghost()
            .xsmall()
            .map(|b| if armed { b.label("Discard?").danger() } else { b.icon(IconName::Undo2) })
            .tooltip(tip)
            .disabled(self.git_busy)
            .on_click(cx.listener(move |s, _, window, cx| {
                // Read on click, not every frame: a section can hold thousands.
                let Some(snapshot) = s.git_snapshot() else { return };
                let entries: Vec<git::Entry> = section.entries(snapshot).into_iter().cloned().collect();
                // Files bound for the Trash get an alert, which is confirmation enough.
                let trashes = entries.iter().any(|e| e.discard_trashes(staged));
                if trashes || s.discard_armed == Some(staged) {
                    s.discard_armed = None;
                    s.confirm_discard(entries, staged, window, cx);
                } else {
                    s.discard_armed = Some(staged);
                    cx.notify();
                }
            }))
    }

    /// Discards, first asking with an alert when files would go to the Trash.
    fn confirm_discard(&mut self, entries: Vec<git::Entry>, staged: bool, window: &mut Window, cx: &mut Context<Self>) {
        let trashed: Vec<&git::Entry> = entries.iter().filter(|e| e.discard_trashes(staged)).collect();
        if trashed.is_empty() {
            self.git_operation(Operation::Discard { entries, staged }, cx);
            return;
        }
        let message = match trashed.as_slice() {
            [one] => format!("Move “{}” to the Trash?", one.path.file_name().map_or_else(|| one.path.to_string_lossy(), |n| n.to_string_lossy())),
            many => format!("Move {} files to the Trash?", many.len()),
        };
        let others = entries.len() - trashed.len();
        let detail = match (trashed.len(), others) {
            (1, 0) => "Git has no copy of this file. You can get it back from the Trash.".to_string(),
            (_, 0) => "Git has no copy of these files. You can get them back from the Trash.".to_string(),
            (_, n) => format!("Git has no copy of them. You can get them back from the Trash. Changes to {n} other file{} are discarded too.", if n == 1 { "" } else { "s" }),
        };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &["Move to Trash", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |s, cx| s.git_operation(Operation::Discard { entries, staged }, cx));
            }
        })
        .detach();
    }

    /// A changed file's right-click menu: the diff menu, and Discard changes,
    /// which asks before throwing anything away.
    fn change_menu(&mut self, path: PathBuf, entry: git::Entry, staged: bool, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let armed = std::rc::Rc::new(std::cell::Cell::new(false));
        let extra: crate::menu::Builder = std::rc::Rc::new(move |cx: &App| {
            let busy = weak.upgrade().is_none_or(|w| w.read(cx).git_busy);
            // Files bound for the Trash are confirmed with an alert; others take a second click.
            let ask = entry.discard_trashes(staged) || armed.get();
            let item = crate::menu::Item::new(if armed.get() { "Click again to discard" } else { "Discard changes" })
                .icon(IconName::Undo2)
                .disabled(busy);
            let item = if ask {
                let weak = weak.clone();
                let entry = entry.clone();
                item.on_click(move |window, cx| {
                    let _ = weak.update(cx, |s, cx| s.confirm_discard(vec![entry.clone()], staged, window, cx));
                })
            } else {
                // Stays open, asking again.
                let armed = armed.clone();
                item.keep_open().on_click(move |_, _| armed.set(true))
            };
            vec![crate::menu::Entry::Item(item)]
        });
        self.diff_menu_with(path, position, Some(extra), window, cx);
    }

    /// Puts changes aside: all of them, untracked files included, or only what is staged.
    fn stash_button(&self, colors: Palette, any: bool, staged: bool, cx: &mut Context<Self>) -> Div {
        let enabled = any && !self.git_busy;
        let trigger = row()
            .id("stash-button")
            .size(px(22.))
            .flex_shrink_0()
            .justify_center()
            .rounded(px(5.))
            .text_color(rgb(colors.muted))
            .child(icon(Glyph::Archive).size(px(13.)))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(if enabled { "Stash changes" } else { "Nothing to stash" }).build(window, cx)
            })
            .map(|s| if enabled { s.cursor_pointer().hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text))) } else { s.opacity(0.5) });
        if !enabled {
            return div().child(trigger);
        }
        self.popover_trigger("stash", trigger, Corner::BottomRight, 240., None, move |_, weak, _| {
            let all = weak.clone();
            let only = weak.clone();
            vec![
                crate::menu::Entry::Label("Stash".into()),
                crate::menu::Entry::Item(
                    crate::menu::Item::new("Stash all changes")
                        .icon(Glyph::Archive)
                        .description("Untracked files included")
                        .on_click(move |_, cx| {
                            let _ = all.update(cx, |s, cx| s.git_operation(Operation::Stash { staged_only: false }, cx));
                        }),
                ),
                crate::menu::Entry::Item(
                    crate::menu::Item::new("Stash staged changes")
                        .icon(Glyph::Archive)
                        .description("Leaves unstaged and untracked files as they are")
                        .disabled(!staged)
                        .on_click(move |_, cx| {
                            let _ = only.update(cx, |s, cx| s.git_operation(Operation::Stash { staged_only: true }, cx));
                        }),
                ),
            ]
        }, cx)
    }

    /// Stashed changes, newest first: pop one back (removing it), apply it
    /// (keeping it), or drop it, which takes a second click.
    fn stashes_section(&self, colors: Palette, snapshot: &git::Snapshot, cx: &mut Context<Self>) -> Option<Div> {
        if snapshot.stashes.is_empty() {
            return None;
        }
        let busy = self.git_busy;
        let mut section = col().child(
            row()
                .h(px(30.))
                .px_3()
                .gap_2()
                .child(icon(Glyph::Archive).size(px(13.)).text_color(rgb(colors.muted)))
                .child(div().text_size(px(crate::theme::scaled_ui(12.))).font_weight(FontWeight::MEDIUM).text_color(rgb(colors.muted)).child("Stashes"))
                .child(count_badge(colors, snapshot.stashes.len()))
                .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border))),
        );
        for stash in &snapshot.stashes {
            let index = stash.index;
            let armed = self.stash_drop_armed == Some(index);
            let group = SharedString::from(format!("stash-{index}"));
            let tip = SharedString::from(match &stash.branch {
                Some(branch) => format!("{} · on {branch} · {}", stash.message, stash.when),
                None => format!("{} · {}", stash.message, stash.when),
            });
            let action = |id: &'static str, text: &'static str, tip: &'static str, operation: Operation| {
                Button::new((id, index))
                    .ghost()
                    .xsmall()
                    .label(text)
                    .tooltip(tip)
                    .disabled(busy)
                    .on_click(cx.listener(move |s, _, _, cx| s.git_operation(operation.clone(), cx)))
            };
            section = section.child(
                row()
                    .id(("stash", index))
                    .group(group.clone())
                    .h(px(28.))
                    .mx_1()
                    .pl_2()
                    .pr_1()
                    .gap_2()
                    .rounded(px(5.))
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                    .child(
                        line_with_detail(
                            if stash.message.is_empty() { "Stashed changes".to_string() } else { stash.message.clone() },
                            stash.branch.clone().map(|b| (SharedString::from(b), Hsla::from(rgb(colors.quiet)))),
                            18.,
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    // Its age gives way to the actions on hover.
                    .child(
                        label(colors, stash.when.clone())
                            .flex_shrink_0()
                            .text_size(px(crate::theme::scaled_ui(11.)))
                            .when(!armed, |s| s.group_hover(group.clone(), |s| s.opacity(0.)).absolute().right(px(8.))),
                    )
                    .child(
                        row()
                            .gap(px(2.))
                            .flex_shrink_0()
                            .when(!armed, |s| s.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)))
                            .child(action("stash-pop", "Pop", "Restore these changes and remove the stash", Operation::StashPop(index)))
                            .child(action("stash-apply", "Apply", "Restore these changes and keep the stash", Operation::StashApply(index)))
                            .child(
                                Button::new(("stash-drop", index))
                                    .ghost()
                                    .xsmall()
                                    .map(|b| if armed { b.label("Drop?").danger() } else { b.icon(IconName::Delete) })
                                    .tooltip(if armed { "Click again to delete this stash for good" } else { "Delete this stash" })
                                    .disabled(busy)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        if s.stash_drop_armed == Some(index) {
                                            s.git_operation(Operation::StashDrop(index), cx);
                                        } else {
                                            s.stash_drop_armed = Some(index);
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .relative(),
            );
        }
        Some(section)
    }

    pub(super) fn changes_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let mut page = col().flex_1().min_h_0();
        let Some(folder) = self.git_folder() else {
            return page.child(empty(colors, Glyph::Branch, "No project", "Open a project to see Git changes."));
        };
        let tree = self.git_tree_for(&folder);
        let snapshot = match self.git_snapshots.get(&tree) {
            None => return page.child(row().h(px(34.)).pl_3().pr_1().children(self.folder_picker(colors, cx))).child(label(colors, "Loading Git changes…").p_3()),
            Some(Err(error)) => {
                return page
                    .child(row().h(px(34.)).pl_3().pr_1().children(self.folder_picker(colors, cx)))
                    .child(empty(colors, Glyph::Branch, "Git unavailable", error.clone()));
            }
            Some(Ok(s)) => s,
        };
        let visible: Vec<&git::Entry> = snapshot.entries.iter().collect();
        // Each side's lines count once: a file staged in part shows both halves.
        let (adds, dels) = visible
            .iter()
            .flat_map(|e| [e.staged_stat, e.unstaged_stat])
            .flatten()
            .fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
        page = page.child(
            row()
                .flex_shrink_0()
                .h(px(34.))
                .pl_3()
                .pr_1()
                .gap_2()
                .border_b_1()
                .border_color(rgb(colors.row_border))
                .children(self.folder_picker(colors, cx))
                .child(div().flex_1())
                .child(self.worktree_picker(colors, &folder, snapshot, cx)),
        );
        let folder = tree;
        if let Some(error) = self.git_errors.get(&folder) {
            page = page.child(div().px_3().py_2().text_size(px(crate::theme::scaled_ui(12.))).text_color(rgb(colors.red)).child(error.clone()));
        }

        let sections = Section::ALL.map(|section| (section, section.entries(snapshot)));
        let open = match &self.tabs[self.active].kind {
            TabKind::GitDiff(path, staged) => Some((path.clone(), *staged)),
            _ => None,
        };
        let mut list = col().pb_2();
        list = list.children(self.amend_section(colors, snapshot, cx));
        for (n, (section, entries)) in sections.iter().enumerate() {
            let (section, title, staged) = (*section, section.title(), *section == Section::Staged);
            if entries.is_empty() {
                continue;
            }
            let conflicts = section == Section::Conflicts;
            let can_toggle = !self.git_busy && !conflicts;
            let group_tip = if staged { "Unstage all" } else { "Stage all" };
            let (adds, dels) = entries.iter().filter_map(|e| e.stat(staged)).fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
            list = list.child(
                row()
                    .h(px(30.))
                    .px_3()
                    .gap_2()
                    .child(
                        tick(colors, ("section-tick", n), staged, can_toggle)
                            .when(can_toggle, |s| {
                                s.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(group_tip).build(w, cx))
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        let Some(snapshot) = s.git_snapshot() else { return };
                                        let operation = toggle_operation(&section.entries(snapshot), staged);
                                        s.git_operation(operation, cx)
                                    }))
                            }),
                    )
                    .child(div().text_size(px(crate::theme::scaled_ui(12.))).font_weight(FontWeight::MEDIUM).text_color(rgb(if conflicts { colors.red } else { colors.muted })).child(title))
                    .child(count_badge(colors, entries.len()))
                    .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border)))
                    .when(!conflicts, |s| {
                        s.child(stat(colors, adds, dels))
                            .child(self.discard_button(("discard-section", n), section, cx))
                    }),
            );
            for (i, entry) in entries.iter().take(MAX_ROWS).enumerate() {
                let path = snapshot.root.join(&entry.path);
                let status = entry.status(staged);
                let hue = status_color(colors, status);
                let relative = entry.path.to_string_lossy().into_owned();
                let (name, dir) = split_path(&relative);
                let active = open.as_ref() == Some(&(path.clone(), staged));
                let operation = toggle_operation(&[*entry], staged);
                let tip = if staged { "Unstage" } else { "Stage" };
                list = list.child(
                    row()
                        .id(SharedString::from(format!("git-{n}-{i}")))
                        .h(px(26.))
                        .mx_1()
                        .px_2()
                        .gap_2()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(active, |s| s.bg(rgb(colors.selected)))
                        .when(!active, |s| s.hover(|s| s.bg(rgb(colors.hover))))
                        .child(tick(colors, SharedString::from(format!("tick-{n}-{i}")), staged, can_toggle).when(can_toggle, |s| {
                            s.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    // The row behind opens the diff; the box only stages.
                                    cx.stop_propagation();
                                    s.git_operation(operation.clone(), cx)
                                }))
                        }))
                        .child(
                            line_with_detail(
                                name.to_string(),
                                (!dir.is_empty()).then(|| (SharedString::from(dir.to_string()), Hsla::from(rgb(colors.quiet)))),
                                18.,
                            )
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(if status == b'D' { colors.muted } else { colors.text })),
                        )
                        .map(|s| match entry.stat(staged) {
                            Some((adds, dels)) => s.child(stat(colors, adds, dels)),
                            None if status == b'D' || conflicts => s,
                            None => s.child(label(colors, "binary").text_size(px(crate::theme::scaled_ui(11.)))),
                        })
                        .child(
                            div()
                                .w(px(12.))
                                .flex_shrink_0()
                                .text_center()
                                .font_family(mono())
                                .text_size(px(crate::theme::scaled_mono(11.)))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(hue))
                                .child(if status == b'?' { "U".to_string() } else { char::from(status).to_string() }),
                        )
                        .on_mouse_down(MouseButton::Right, {
                            let path = path.clone();
                            // Conflicts are settled in the editor, not thrown away from here.
                            let entry = (!conflicts).then(|| (*entry).clone());
                            cx.listener(move |s, e: &MouseDownEvent, window, cx| match &entry {
                                Some(entry) => s.change_menu(path.clone(), entry.clone(), staged, e.position, window, cx),
                                None => s.diff_menu(path.clone(), e.position, window, cx),
                            })
                        })
                        .on_click(cx.listener(move |s, _, _, cx| s.open(TabKind::GitDiff(path.clone(), staged), cx))),
                );
            }
            if entries.len() > MAX_ROWS {
                let more = entries.len() - MAX_ROWS;
                list = list.child(
                    label(colors, format!("{more} more file{} not shown", if more == 1 { "" } else { "s" }))
                        .px_3()
                        .py_1()
                        .text_size(px(crate::theme::scaled_ui(11.))),
                );
            }
        }
        if visible.is_empty() {
            list = list.child(empty(
                colors,
                Glyph::FileDiff,
                "Working tree clean",
                "Git changes appear here as files are edited.",
            ));
        }
        list = list.children(self.stashes_section(colors, snapshot, cx));
        page = page.child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(div().id("git-changes-list").size_full().overflow_y_scroll().track_scroll(&self.git_changes_scroll).child(list))
                .vertical_scrollbar(&self.git_changes_scroll),
        );

        let staged = snapshot.entries.iter().filter(|e| e.staged()).count();
        let message = !self.commit_message.read(cx).value().trim().is_empty();
        let conflicted = snapshot.entries.iter().any(|e| e.conflict());
        // A pushed commit is left alone, as with undo: amending it would need a force push.
        let can_amend = snapshot.last_commit.is_some() && !snapshot.pushed();
        let amending = self.amend && can_amend;
        // Only what is staged is committed; nothing is staged on the way. An
        // amend may only reword the message.
        let can_commit = !self.git_busy && message && !conflicted && (staged > 0 || amending);
        let commit_tip: SharedString = if conflicted {
            "Resolve conflicts before committing".into()
        } else if amending && !message {
            "Enter a commit message".into()
        } else if amending {
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let dropped = snapshot.last_commit.as_ref().map_or(0, |c| c.files.iter().filter(|f| self.amend_drop.contains(&f.path)).count());
            match (staged, dropped) {
                (0, 0) => "Reword the last commit".into(),
                (n, 0) => format!("Add {n} staged file{} to the last commit", plural(n)).into(),
                (0, d) => format!("Take {d} file{} out of the last commit", plural(d)).into(),
                (n, d) => format!("Add {n} staged file{} and take {d} out of the last commit", plural(n)).into(),
            }
        } else if staged == 0 {
            "Nothing staged. Tick the files to commit".into()
        } else if !message {
            "Enter a commit message".into()
        } else {
            format!("Commit {staged} staged file{}", if staged == 1 { "" } else { "s" }).into()
        };
        let height = self.settings.commit_box_height;
        let running = self.git_running;
        let pushable = !self.git_busy && snapshot.can_push();
        let push_tip: SharedString = match (&snapshot.upstream, snapshot.ahead) {
            _ if snapshot.last_commit.is_none() => "Nothing to push yet".into(),
            (None, _) => "Push this branch and track it on the remote".into(),
            (Some(up), 0) => format!("Up to date with {up}").into(),
            (Some(up), n) => format!("Push {n} commit{} to {up}", if n == 1 { "" } else { "s" }).into(),
        };
        let push_label = match running {
            Some(GitRunning::Push) => "Pushing…".to_string(),
            _ if snapshot.upstream.is_some() && snapshot.ahead > 0 => format!("Push {}", snapshot.ahead),
            _ => "Push".to_string(),
        };
        let last = snapshot.last_commit.clone();
        let pushed = snapshot.pushed();
        let commit_box = col()
            .id("commit-box")
            .relative()
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.background))
            .min_h(px(crate::settings::COMMIT_BOX_MIN))
            .when_some(height, |s, h| s.h(px(h)))
            // The box's bottom stays put, so its height is the distance from
            // the cursor down to it. It can take most of the panel, no more.
            .on_drag_move(cx.listener(|s, e: &DragMoveEvent<CommitEdge>, window, cx| {
                let max = (f32::from(window.viewport_size().height) * 0.7).max(crate::settings::COMMIT_BOX_MIN);
                let h = f32::from(e.bounds.bottom() - e.event.position.y).clamp(crate::settings::COMMIT_BOX_MIN, max).round();
                if s.settings.commit_box_height != Some(h) {
                    s.set_commit_height(Some(h), window, cx);
                }
            }))
            // The drag ends wherever the button comes up.
            .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, _| s.save_commit_height()))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, _| s.save_commit_height()))
            // The top edge: drag to resize, double-click to fit the text again.
            .child(
                div()
                    .id("commit-box-edge")
                    .absolute()
                    .top(px(-3.))
                    .left_0()
                    .right_0()
                    .h(px(6.))
                    .cursor_row_resize()
                    .on_drag(CommitEdge, |edge, _, _, cx| cx.new(|_| edge.clone()))
                    .on_click(cx.listener(|s, e: &ClickEvent, window, cx| {
                        if e.click_count() == 2 {
                            s.set_commit_height(None, window, cx);
                            s.save_commit_height();
                        }
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(14.))
                    .pt(px(12.))
                    .pb(px(6.))
                    // Commit messages are set in the code font, as in a terminal.
                    .font_family(mono())
                    .text_size(px(crate::theme::mono_size()))
                    .child(
                        Input::new(&self.commit_message)
                            .appearance(false)
                            .p_0()
                            .font_family(mono())
                            .text_size(px(crate::theme::mono_size()))
                            .when(height.is_some(), |s| s.h(relative(1.))),
                    ),
            )
            .child(
                row()
                    .h(px(40.))
                    .px(px(10.))
                    .gap(px(8.))
                    // While amending, Push gives way to Amend and Commit to Cancel.
                    .map(|s| if amending {
                        let tip = commit_tip.clone();
                        s.child(
                            bar_button(colors, "amend", Some(IconName::Check), if running == Some(GitRunning::Commit) { "Amending…" } else { "Amend" }, can_commit)
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                                .when(can_commit, |s| {
                                    s.on_click(cx.listener(|s, _, _, cx| {
                                        let message = s.commit_message.read(cx).value().to_string();
                                        // Only files still in the commit can be taken out of it.
                                        let files = s.git_snapshot().and_then(|s| s.last_commit.as_ref()).map(|c| c.files.clone()).unwrap_or_default();
                                        let drop = files.into_iter().map(|f| f.path).filter(|p| s.amend_drop.contains(p)).collect();
                                        s.git_operation(Operation::Amend { message, drop }, cx);
                                    }))
                                }),
                        )
                    } else {
                        s.child(
                            bar_button(colors, "push", Some(IconName::ArrowUp), push_label, pushable)
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(push_tip.clone()).build(window, cx))
                                .when(pushable, |s| s.on_click(cx.listener(|s, _, _, cx| s.git_operation(Operation::Push, cx)))),
                        )
                    })
                    .child(
                        row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.))
                            .when(staged > 0, |s| {
                                s.child(label(colors, format!("{staged} file{} staged", if staged == 1 { "" } else { "s" })).truncate())
                            }),
                    )
                    .map(|s| if amending {
                        let last = snapshot.last_commit.as_ref().map(|c| c.message.clone()).unwrap_or_default();
                        s.child(
                            bar_button(colors, "amend-cancel", None, "Cancel", !self.git_busy)
                                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Stop amending").build(window, cx))
                                .when(!self.git_busy, |s| {
                                    s.on_click(cx.listener(move |s, _, _, cx| {
                                        s.amend = false;
                                        s.amend_drop.clear();
                                        // The last commit's message goes again if it wasn't touched.
                                        s.clear_commit_message = Some(last.clone());
                                        cx.notify();
                                    }))
                                }),
                        )
                    } else {
                        let writing = self.commit_writing.is_some();
                        let can_write = writing || (!self.git_busy && !conflicted && !snapshot.entries.is_empty());
                        let write_tip = if writing {
                            "Writing a commit message… Click to stop"
                        } else if staged > 0 {
                            "Write a commit message for the staged changes"
                        } else {
                            "Write a commit message for all changes"
                        };
                        s.child(
                            div()
                                .id("write-commit")
                                .size(px(24.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .child({
                                    let glyph = icon(Glyph::Sparkle).size(px(13.)).text_color(rgb(if writing { colors.blue } else { colors.muted }));
                                    if writing {
                                        div()
                                            .child(glyph)
                                            .with_animation(
                                                "write-commit-pulse",
                                                Animation::new(std::time::Duration::from_millis(900)).repeat().with_easing(pulsating_between(0.4, 1.)),
                                                |s, t| s.opacity(t),
                                            )
                                            .into_any_element()
                                    } else {
                                        glyph.into_any_element()
                                    }
                                })
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(write_tip).build(window, cx))
                                .when(can_write, |s| {
                                    s.cursor_pointer()
                                        .hover(|s| s.bg(rgb(colors.hover)))
                                        .active(|s| s.bg(rgb(colors.border)))
                                        .on_click(cx.listener(|s, _, _, cx| s.write_commit_message(cx)))
                                })
                                .when(!can_write, |s| s.opacity(0.5)),
                        )
                        .child(
                            bar_button(colors, "commit", None, if running == Some(GitRunning::Commit) { "Committing…" } else { "Commit" }, can_commit)
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(commit_tip.clone()).build(window, cx))
                                .when(can_commit, |s| {
                                    s.on_click(cx.listener(|s, _, _, cx| {
                                        let message = s.commit_message.read(cx).value().to_string();
                                        s.git_operation(Operation::Commit(message), cx);
                                    }))
                                }),
                        )
                    })
            );
        // The last commit, with a way to take it back while it is still local.
        let footer = last.map(|last| {
            let undoable = !self.git_busy && !pushed;
            row()
                .flex_shrink_0()
                .h(px(34.))
                .pl(px(14.))
                .pr(px(8.))
                .gap(px(6.))
                .border_t_1()
                .border_color(rgb(colors.border))
                .bg(rgb(colors.panel))
                .child(
                    div()
                        .id("last-commit")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(crate::theme::scaled_ui(12.5)))
                        .text_color(rgb(colors.text))
                        .child(last.subject().to_string())
                        .tooltip({
                            let message = SharedString::from(last.message.clone());
                            move |window, cx| gpui_component::tooltip::Tooltip::new(message.clone()).build(window, cx)
                        }),
                )
                .child(
                    Button::new("amend-commit")
                        .icon(Glyph::Pencil)
                        .ghost()
                        .small()
                        .disabled(self.git_busy || !can_amend || amending)
                        .tooltip(if !can_amend {
                            "Already pushed, so it can't be amended here"
                        } else if amending {
                            "Amending: edit the message, then Amend"
                        } else {
                            "Amend last commit: reword it, or add what is staged"
                        })
                        .on_click({
                            let message = last.message.clone();
                            cx.listener(move |s, _, _, cx| {
                                s.amend = true;
                                s.amend_drop.clear();
                                // Its message comes up to edit, unless one is being written.
                                s.restore_commit_message = Some(message.clone());
                                cx.notify();
                            })
                        }),
                )
                .child(
                    Button::new("undo-commit")
                        .icon(IconName::Undo2)
                        .ghost()
                        .small()
                        .loading(running == Some(GitRunning::Undo))
                        .disabled(!undoable || amending)
                        .tooltip(if pushed { "Already pushed, so it can't be undone here" } else { "Undo last commit, keeping its changes staged" })
                        .on_click(cx.listener(|s, _, _, cx| s.git_operation(Operation::UndoCommit, cx))),
                )
        });
        // The branch and what changed on it, pinned over the commit message.
        let summary = row()
            .flex_shrink_0()
            .h(px(30.))
            .px_3()
            .gap_2()
            .border_t_1()
            .border_color(rgb(colors.row_border))
            .child(branch_chip(colors, snapshot.branch.clone()))
            .child(div().flex_1())
            .when(!visible.is_empty(), |s| {
                s.child(label(colors, format!("{} change{}", visible.len(), if visible.len() == 1 { "" } else { "s" })))
                    .child(stat(colors, adds, dels))
            })
            .child(self.stash_button(colors, !visible.is_empty() && !conflicted, staged > 0, cx));
        page.child(summary).child(commit_box).children(footer)
    }

    pub(super) fn git_diff_view(&self, path: &Path, staged: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let id = SharedString::from(format!("git-diff-{staged}-{}", path.display()));
        let body = match self.git_diffs.get(&(path.to_owned(), staged)) {
            Some(Ok(git::Diff::Text { old, new })) => self.diff_pane(id, super::panes::LineSource::GitDiff(path.to_owned(), staged), old, new, window, cx),
            Some(Ok(git::Diff::Image { old, new })) => super::media::image_diff(colors, old.as_ref(), new.as_ref(), self.settings.image_diff_stacked),
            Some(Ok(git::Diff::Binary)) => empty(colors, Glyph::FileDiff, "Binary file", "Its change can't be shown as text.").into_any_element(),
            Some(Ok(git::Diff::Large)) => empty(colors, Glyph::FileDiff, "Large file", "This file is too large to diff here.").into_any_element(),
            Some(Err(error)) => empty(colors, Glyph::FileDiff, "No diff", error.clone()).into_any_element(),
            None => label(colors, "Loading diff…").p_3().into_any_element(),
        };
        let image = matches!(self.git_diffs.get(&(path.to_owned(), staged)), Some(Ok(git::Diff::Image { .. })));
        let stacked = self.settings.image_diff_stacked;
        let layout = |id: &'static str, glyph: Glyph, tip: &'static str, value: bool| {
            Button::new(id).icon(glyph).ghost().xsmall().selected(stacked == value).tooltip(tip).on_click(cx.listener(move |s, _, _, cx| {
                if s.settings.image_diff_stacked != value {
                    s.settings.image_diff_stacked = value;
                    let _ = s.settings.save();
                    cx.notify();
                }
            }))
        };
        let entry = self.git_snapshots.values().filter_map(|s| s.as_ref().ok()).find_map(|s| s.entries.iter().find(|e| s.root.join(&e.path) == path));
        col()
            .size_full()
            .child(
                row()
                    .flex_shrink_0()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(colors.row_border))
                    .child(label(colors, if staged { "Staged · HEAD → Index" } else { "Unstaged · Index → Working tree" }))
                    .when_some(entry.and_then(|e| e.stat(staged)), |s, (adds, dels)| s.child(stat(colors, adds, dels)))
                    .child(div().flex_1())
                    .when(image, |s| {
                        s.child(
                            row()
                                .gap(px(2.))
                                .child(layout("image-diff-beside", Glyph::Columns, "Side by side", false))
                                .child(layout("image-diff-stacked", Glyph::Rows, "Stacked", true)),
                        )
                    })
                    .child(
                        Button::new("git-view-file").ghost().xsmall().label("Open file").on_click(cx.listener({
                            let path = path.to_owned();
                            move |s, _, _, cx| s.open(TabKind::File(path.clone()), cx)
                        })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::tidy_commit_message;

    #[test]
    fn commit_messages_lose_their_fences() {
        assert_eq!(tidy_commit_message("  feat: add x\n").as_deref(), Some("feat: add x"));
        assert_eq!(tidy_commit_message("```text\nfix(git): y\n\nbody\n```").as_deref(), Some("fix(git): y\n\nbody"));
        assert_eq!(tidy_commit_message("`chore: z`").as_deref(), Some("chore: z"));
        assert_eq!(tidy_commit_message(" \n"), None);
    }
}
