//! The Changes page, after Zed's Git panel: changed files under Staged and
//! Unstaged (a file staged in part is under both), each with a checkbox that
//! moves it across, its line counts on that side, and a commit box underneath.
use super::ui::*;
use super::*;
use crate::{
    git::{self, Operation},
    icons::Glyph,
};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
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
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        for path in if staged { entry.paths() } else { entry.stage_paths() } {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    if staged { Operation::Unstage(paths) } else { Operation::Stage(paths) }
}

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
                            s.entries
                                .iter()
                                .find(|e| s.root.join(&e.path) == path && if staged { e.staged() } else { e.unstaged() })
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
                if revision != s.git_revision {
                    s.refresh_git(cx);
                    return;
                }
                s.git_snapshots.extend(snapshots);
                if let Some((key, diff)) = diff {
                    s.git_diffs.insert(key, diff);
                }
                s.sync_git_watch();
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
            Operation::Commit(_) => GitRunning::Commit,
            Operation::Push => GitRunning::Push,
            Operation::UndoCommit => GitRunning::Undo,
            _ => GitRunning::Other,
        });
        self.git_revision += 1;
        self.git_errors.remove(&folder);
        cx.notify();
        let committed = match &operation {
            Operation::Commit(message) => Some(message.clone()),
            _ => None,
        };
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

        // Conflicts are settled in the editor first, then staged from Unstaged.
        let sections: [(&str, bool, Vec<&git::Entry>); 3] = [
            ("Conflicts", false, visible.iter().copied().filter(|e| e.conflict()).collect()),
            ("Staged", true, visible.iter().copied().filter(|e| e.staged()).collect()),
            ("Unstaged", false, visible.iter().copied().filter(|e| e.unstaged() && !e.conflict()).collect()),
        ];
        let open = match &self.tabs[self.active].kind {
            TabKind::GitDiff(path, staged) => Some((path.clone(), *staged)),
            _ => None,
        };
        let mut list = col().pb_2();
        for (n, (title, staged, entries)) in sections.iter().enumerate() {
            let (title, staged) = (*title, *staged);
            if entries.is_empty() {
                continue;
            }
            let conflicts = title == "Conflicts";
            let can_toggle = !self.git_busy && !conflicts;
            let operation = toggle_operation(entries, staged);
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
                                    .on_click(cx.listener(move |s, _, _, cx| s.git_operation(operation.clone(), cx)))
                            }),
                    )
                    .child(div().text_size(px(crate::theme::scaled_ui(12.))).font_weight(FontWeight::MEDIUM).text_color(rgb(if conflicts { colors.red } else { colors.muted })).child(title))
                    .child(count_badge(colors, entries.len()))
                    .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border)))
                    .when(!conflicts, |s| s.child(stat(colors, adds, dels))),
            );
            for (i, entry) in entries.iter().enumerate() {
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
                            cx.listener(move |s, e: &MouseDownEvent, window, cx| s.diff_menu(path.clone(), e.position, window, cx))
                        })
                        .on_click(cx.listener(move |s, _, _, cx| s.open(TabKind::GitDiff(path.clone(), staged), cx))),
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
        page = page.child(div().id("git-changes-list").flex_1().min_h_0().overflow_y_scroll().child(list));

        let staged = snapshot.entries.iter().filter(|e| e.staged()).count();
        let message = !self.commit_message.read(cx).value().trim().is_empty();
        let conflicted = snapshot.entries.iter().any(|e| e.conflict());
        // Only what is staged is committed; nothing is staged on the way.
        let can_commit = !self.git_busy && message && !conflicted && staged > 0;
        let commit_tip: SharedString = if conflicted {
            "Resolve conflicts before committing".into()
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
                    .child(
                        bar_button(colors, "push", Some(IconName::ArrowUp), push_label, pushable)
                            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(push_tip.clone()).build(window, cx))
                            .when(pushable, |s| s.on_click(cx.listener(|s, _, _, cx| s.git_operation(Operation::Push, cx)))),
                    )
                    .child(
                        row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.))
                            .when(staged > 0, |s| {
                                s.child(label(colors, format!("{staged} file{} staged", if staged == 1 { "" } else { "s" })).truncate())
                            }),
                    )
                    .child(
                        bar_button(
                            colors,
                            "commit",
                            None,
                            if running == Some(GitRunning::Commit) { "Committing…" } else { "Commit" },
                            can_commit,
                        )
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(commit_tip.clone()).build(window, cx))
                        .when(can_commit, |s| {
                            s.on_click(cx.listener(|s, _, _, cx| {
                                let message = s.commit_message.read(cx).value().to_string();
                                s.git_operation(Operation::Commit(message), cx);
                            }))
                        }),
                    ),
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
                    Button::new("undo-commit")
                        .icon(IconName::Undo2)
                        .ghost()
                        .small()
                        .loading(running == Some(GitRunning::Undo))
                        .disabled(!undoable)
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
            });
        page.child(summary).child(commit_box).children(footer)
    }

    pub(super) fn git_diff_view(&self, path: &Path, staged: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let id = SharedString::from(format!("git-diff-{staged}-{}", path.display()));
        let body = match self.git_diffs.get(&(path.to_owned(), staged)) {
            Some(Ok(git::Diff::Text { old, new })) => self.diff_pane(id, super::panes::LineSource::GitDiff(path.to_owned(), staged), old, new, window, cx),
            Some(Ok(git::Diff::Image { old, new })) => super::media::image_diff(colors, old.as_ref(), new.as_ref()),
            Some(Ok(git::Diff::Binary)) => empty(colors, Glyph::FileDiff, "Binary file", "Its change can't be shown as text.").into_any_element(),
            Some(Ok(git::Diff::Large)) => empty(colors, Glyph::FileDiff, "Large file", "This file is too large to diff here.").into_any_element(),
            Some(Err(error)) => empty(colors, Glyph::FileDiff, "No diff", error.clone()).into_any_element(),
            None => label(colors, "Loading diff…").p_3().into_any_element(),
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
