//! Staged / unstaged workflow modeled on Zed's Git panel, adapted to this app.
use super::ui::*;
use super::*;
use crate::{
    git::{self, Operation},
    icons::Glyph,
};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::Input,
};

impl Workspace {
    pub(super) fn git_snapshot(&self) -> Option<&git::Snapshot> {
        self.git_snapshots
            .get(self.current_project().path.as_ref()?)?
            .as_ref()
            .ok()
    }
    pub(super) fn start_git_poll(&mut self, cx: &mut Context<Self>) {
        self.refresh_git(cx);
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
    pub(super) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_refreshing || self.git_busy {
            return;
        }
        let Some(folder) = self.current_project().path.clone() else {
            return;
        };
        self.git_refreshing = true;
        let revision = self.git_revision;
        let selected = match &self.tabs[self.active].kind {
            TabKind::GitDiff(path, staged) => Some((path.clone(), *staged)),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let work_folder = folder.clone();
            let (snapshot, diff) = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = git::snapshot(&work_folder);
                    let diff = selected.and_then(|(path, staged)| {
                        let snapshot = snapshot.as_ref().ok()?;
                        let result = snapshot
                            .entries
                            .iter()
                            .find(|e| {
                                snapshot.root.join(&e.path) == path
                                    && if staged { e.staged() } else { e.unstaged() }
                            })
                            .map(|entry| git::diff(&snapshot.root, entry, staged))
                            .unwrap_or_else(|| {
                                Ok(git::Diff {
                                    patch: "No changes in this section.".into(),
                                })
                            });
                        Some(((path, staged), result))
                    });
                    (snapshot, diff)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.git_refreshing = false;
                if revision != s.git_revision {
                    s.refresh_git(cx);
                    return;
                }
                s.git_snapshots.insert(folder, snapshot);
                if let Some((key, diff)) = diff {
                    s.git_diffs.insert(key, diff);
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
        let Some(folder) = self.current_project().path.clone() else {
            return;
        };
        self.git_busy = true;
        self.git_revision += 1;
        self.git_errors.remove(&folder);
        cx.notify();
        let committed = match &operation {
            Operation::Commit(message) => Some(message.clone()),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { git::operate(&root, operation) })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.git_busy = false;
                s.git_revision += 1;
                match result {
                    Err(error) => {
                        s.git_errors.insert(folder, error);
                    }
                    Ok(()) => {
                        s.clear_commit_message = committed;
                    }
                }
                s.git_diffs.clear();
                s.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn changes_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let mut page = col().flex_1().min_h_0();
        page = page.child(
            row()
                .flex_shrink_0()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(rgb(colors.row_border))
                .child(
                    Checkbox::new("thread-changes-only")
                        .small()
                        .label("Show only changes from this thread")
                        .checked(self.thread_changes_only)
                        .on_click(cx.listener(|s, checked, _, cx| {
                            s.thread_changes_only = *checked;
                            cx.notify();
                        })),
                ),
        );
        let Some(folder) = self.current_project().path.as_ref() else {
            return page.child(empty(
                colors,
                Glyph::Branch,
                "No project",
                "Open a project to see Git changes.",
            ));
        };
        let Some(snapshot) = self.git_snapshots.get(folder) else {
            return page.child(label(colors, "Loading Git changes…").p_3());
        };
        let snapshot = match snapshot {
            Ok(s) => s,
            Err(error) => {
                return page.child(empty(
                    colors,
                    Glyph::Branch,
                    "Git unavailable",
                    error.clone(),
                ));
            }
        };
        let touched: HashSet<_> = self
            .current_thread()
            .map(|t| {
                t.files()
                    .into_iter()
                    .filter(|(_, touch)| *touch != crate::model::Touch::Read)
                    .map(|(p, _)| if p.is_absolute() { p } else { folder.join(p) })
                    .collect()
            })
            .unwrap_or_default();
        let visible: Vec<_> = snapshot
            .entries
            .iter()
            .filter(|e| {
                !self.thread_changes_only
                    || e.paths()
                        .iter()
                        .any(|p| touched.contains(&snapshot.root.join(p)))
            })
            .collect();
        page = page.child(
            row()
                .flex_shrink_0()
                .h(px(34.))
                .px_3()
                .gap_2()
                .child(branch_chip(colors, snapshot.branch.clone()))
                .child(div().flex_1())
                .child(
                    icon_button("refresh-git", IconName::Redo, "Refresh changes")
                        .disabled(self.git_busy || self.git_refreshing)
                        .on_click(cx.listener(|s, _, _, cx| s.refresh_git(cx))),
                ),
        );
        if let Some(error) = self.git_errors.get(folder) {
            page = page.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.))
                    .text_color(rgb(colors.red))
                    .child(error.clone()),
            );
        }
        let mut list = col();
        for staged in [true, false] {
            let entries: Vec<_> = visible
                .iter()
                .filter(|e| if staged { e.staged() } else { e.unstaged() })
                .collect();
            let paths: Vec<_> = entries
                .iter()
                .flat_map(|e| if staged { e.paths() } else { e.stage_paths() })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let operation = if staged {
                Operation::Unstage(paths)
            } else {
                Operation::Stage(paths)
            };
            list = list.child(
                row()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .child(label(colors, if staged { "Staged" } else { "Unstaged" }))
                    .child(count_badge(colors, entries.len()))
                    .child(div().flex_1())
                    .child(
                        Button::new(if staged { "unstage-all" } else { "stage-all" })
                            .ghost()
                            .xsmall()
                            .label(if self.thread_changes_only {
                                if staged {
                                    "Unstage shown"
                                } else {
                                    "Stage shown"
                                }
                            } else if staged {
                                "Unstage all"
                            } else {
                                "Stage all"
                            })
                            .disabled(self.git_busy || entries.is_empty())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.git_operation(operation.clone(), cx)
                            })),
                    ),
            );
            for (i, entry) in entries.iter().enumerate() {
                let path = snapshot.root.join(&entry.path);
                let status = if entry.conflict() {
                    "!".into()
                } else {
                    char::from(if staged { entry.index } else { entry.worktree }).to_string()
                };
                let operation = if staged {
                    Operation::Unstage(entry.paths())
                } else {
                    Operation::Stage(entry.stage_paths())
                };
                let active = self.tabs[self.active].kind == TabKind::GitDiff(path.clone(), staged);
                list = list.child(
                    row()
                        .mx_1()
                        .rounded(px(5.))
                        .when(active, |s| s.bg(rgb(colors.selected)))
                        .child(
                            row()
                                .id(SharedString::from(format!("git-{staged}-{i}")))
                                .flex_1()
                                .min_w_0()
                                .h(px(28.))
                                .px_2()
                                .gap_2()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(colors.hover)))
                                .child(label(colors, status).text_color(rgb(if entry.conflict() {
                                    colors.red
                                } else {
                                    colors.amber
                                })))
                                .child(
                                    one_line(entry.path.to_string_lossy().into_owned(), 18.)
                                        .flex_1()
                                        .min_w_0(),
                                )
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.open(TabKind::GitDiff(path.clone(), staged), cx)
                                })),
                        )
                        .child(
                            icon_button(
                                SharedString::from(format!("stage-{staged}-{i}")),
                                if staged {
                                    IconName::Minus
                                } else {
                                    IconName::Plus
                                },
                                if staged { "Unstage file" } else { "Stage file" },
                            )
                            .disabled(self.git_busy)
                            .on_click(cx.listener(
                                move |s, _, _, cx| s.git_operation(operation.clone(), cx),
                            )),
                        ),
                );
            }
        }
        if visible.is_empty() {
            list = list.child(empty(
                colors,
                Glyph::FileDiff,
                if self.thread_changes_only {
                    "No changes from this thread"
                } else {
                    "Working tree clean"
                },
                "Git changes appear here as files are edited.",
            ));
        }
        page = page.child(
            div()
                .id("git-changes-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        );
        let staged_count = snapshot.entries.iter().filter(|e| e.staged()).count();
        let can_commit = !self.git_busy
            && staged_count > 0
            && !self.commit_message.read(cx).value().trim().is_empty()
            && !snapshot.entries.iter().any(|e| e.conflict());
        page.child(
            col()
                .flex_shrink_0()
                .border_t_1()
                .border_color(rgb(colors.row_border))
                .p_3()
                .gap_2()
                .child(Input::new(&self.commit_message))
                .child(
                    Button::new("commit-staged")
                        .primary()
                        .small()
                        .label(if self.git_busy {
                            "Working…".to_owned()
                        } else {
                            format!(
                                "Commit {staged_count} staged file{}",
                                if staged_count == 1 { "" } else { "s" }
                            )
                        })
                        .disabled(!can_commit)
                        .on_click(cx.listener(|s, _, _, cx| {
                            let message = s.commit_message.read(cx).value().to_string();
                            s.git_operation(Operation::Commit(message), cx);
                        })),
                ),
        )
    }
    pub(super) fn git_diff_view(
        &self,
        path: &Path,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let text = match self.git_diffs.get(&(path.to_owned(), staged)) {
            Some(Ok(diff)) => diff.patch.clone(),
            Some(Err(error)) => error.clone(),
            None => "Loading diff…".into(),
        };
        let mut lines = col()
            .font_family(mono())
            .text_size(px(crate::theme::MONO_SIZE));
        for line in text.lines() {
            let (fg, bg) = if line.starts_with('+') && !line.starts_with("+++") {
                (colors.green, Some(colors.green_tint))
            } else if line.starts_with('-') && !line.starts_with("---") {
                (colors.red, Some(colors.red_tint))
            } else if line.starts_with("@@") {
                (colors.blue, Some(colors.blue_tint))
            } else {
                (colors.code, None)
            };
            lines = lines.child(
                div()
                    .min_h(px(20.))
                    .px_3()
                    .text_color(rgb(fg))
                    .when_some(bg, |s, bg| s.bg(rgb(bg)))
                    .child(line.replace('\t', "    ")),
            );
        }
        col()
            .size_full()
            .child(
                row()
                    .flex_shrink_0()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .child(label(
                        colors,
                        if staged {
                            "Staged · HEAD → Index"
                        } else {
                            "Unstaged · Index → Working tree"
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("git-view-file")
                            .ghost()
                            .xsmall()
                            .label("Open file")
                            .on_click(cx.listener({
                                let path = path.to_owned();
                                move |s, _, _, cx| s.open(TabKind::File(path.clone()), cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("git-diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .child(lines),
            )
            .into_any_element()
    }
}
