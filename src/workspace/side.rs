//! The right panel: agent context, Git changes, and the project's files.
use super::ui::*;
use super::*;
use crate::{agent::Step, icons::Glyph, model::Touch};
use gpui_component::{Icon, IconName, Sizable, tooltip::Tooltip};

fn side_glyph(side: Side) -> Icon {
    match side {
        Side::Context => IconName::Asterisk.into(),
        Side::Changes => Glyph::Branch.into(),
        Side::Files => Glyph::Files.into(),
    }
}

/// A change's status letter as shown, and its colour.
fn change_mark(colors: Palette, status: u8) -> (char, u32) {
    match status {
        b'?' => ('U', colors.green),
        b'A' => ('A', colors.green),
        b'D' => ('D', colors.red),
        b'U' => ('!', colors.red),
        b'R' | b'C' => ('R', colors.blue),
        _ => ('M', colors.amber),
    }
}

/// Which change a folder shows when it holds several: conflicts first, then
/// edits, then new files.
fn weight(status: u8) -> u8 {
    match status {
        b'U' => 3,
        b'?' | b'A' => 1,
        _ => 2,
    }
}

/// Files listed before the rest are summed up as "N more".
const FILES_SHOWN: usize = 12;

/// "$0.42", or the amount and currency code for anything but dollars.
fn money(amount: f64, currency: &str) -> String {
    match currency {
        "USD" | "usd" | "$" => format!("${amount:.2} spent"),
        code => format!("{amount:.2} {code} spent"),
    }
}

/// "84.2k" for token counts.
fn tokens(n: u64) -> String {
    if n >= 1000 { format!("{:.1}k", n as f64 / 1000.) } else { n.to_string() }
}

/// A side panel row: leading mark, a name that truncates, a quiet trailing note.
fn item(colors: Palette, lead: impl IntoElement, name: &str, note: impl IntoElement) -> Div {
    let (file, dir) = split_path(name);
    row()
        .h(px(26.))
        .mx_1()
        .px_2()
        .gap_2()
        .rounded(px(5.))
        .child(lead)
        .child(
            line_with_detail(file.to_string(), (!dir.is_empty()).then(|| (dir.to_string().into(), rgb(colors.quiet).into())), 18.)
                .flex_1()
                .min_w_0(),
        )
        .child(note)
}

impl Workspace {
    /// The folders the project spans, each with the branch it has checked out.
    fn folders_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let colors = palette(cx);
        let project = self.current_project();
        project.path.as_ref()?;
        let index = self.project;
        let roots = project.roots();
        let mut rows = col();
        for (i, root) in roots.iter().enumerate() {
            let branch = self.git_snapshots.get(root).and_then(|s| s.as_ref().ok()).map(|s| s.branch.clone());
            let group = SharedString::from(format!("folder-{i}"));
            let folder = root.clone();
            rows = rows.child(
                item(
                    colors,
                    icon(IconName::Folder).text_color(rgb(colors.muted)),
                    &tilde(root),
                    row()
                        .gap_1()
                        .when_some(branch, |s, branch| s.child(branch_chip(colors, branch).max_w(px(120.))))
                        .when(i > 0, |s| {
                            // Only added folders can go; the main one is the project.
                            s.child(
                                div().opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)).child(
                                    icon_button(("remove-folder", i), IconName::Close, "Remove from project")
                                        .xsmall()
                                        .on_click(cx.listener(move |s, _, _, cx| s.remove_folder(index, &folder, cx))),
                                ),
                            )
                        }),
                )
                .id(("folder", i))
                .group(group)
                .hover(|s| s.bg(rgb(colors.hover))),
            );
        }
        Some(
            col()
                .child(
                    row()
                        .h(px(30.))
                        .pl_3()
                        .pr_1()
                        .gap_2()
                        .text_size(px(crate::theme::scaled_ui(12.)))
                        .text_color(rgb(colors.muted))
                        .child("Folders")
                        .child(count_badge(colors, roots.len()))
                        .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border)))
                        .child(
                            icon_button("add-folder", Glyph::FolderPlus, "Add a folder to this project")
                                .xsmall()
                                .on_click(cx.listener(move |s, _, window, cx| s.add_folders(index, window, cx))),
                        ),
                )
                .child(rows),
        )
    }

    fn context_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let Some(thread) = self.current_thread() else {
            return col();
        };
        let project = self.current_project();
        let meter = match thread.usage {
            Some((used, size)) if size > 0 => {
                let share = (used as f32 / size as f32).min(1.);
                // The bar warms as the window fills.
                let hue = if share > 0.85 { colors.red } else if share > 0.6 { colors.amber } else { colors.blue };
                col()
                    .child(
                        row()
                            .items_end()
                            .gap(px(6.))
                            .pb_2()
                            .child(div().text_size(px(crate::theme::scaled_ui(20.))).line_height(relative(1.1)).font_weight(FontWeight::MEDIUM).child(tokens(used)))
                            .child(label(colors, format!("of {} tokens", tokens(size))))
                            .child(div().flex_1())
                            .child(label(colors, format!("{:.0}%", share * 100.))),
                    )
                    .child(
                        div()
                            .h(px(6.))
                            .rounded(px(3.))
                            .overflow_hidden()
                            .bg(rgb(colors.hover))
                            .child(div().h_full().w(relative(share)).bg(rgb(hue))),
                    )
                    .child(
                        row()
                            .pt_2()
                            .gap_2()
                            .text_size(px(crate::theme::scaled_ui(12.)))
                            .text_color(rgb(colors.muted))
                            .child(format!("{} left", tokens(size.saturating_sub(used))))
                            .child(div().flex_1())
                            .when_some(thread.cost.as_ref(), |s, (amount, currency)| s.child(money(*amount, currency))),
                    )
            }
            _ => col(),
        };
        let mut plan = col();
        for (step, text) in &thread.plan {
            plan = plan.child(
                row()
                    .h(px(26.))
                    .px_3()
                    .gap_2()
                    .child(match step {
                        Step::Done => icon(IconName::CircleCheck).text_color(rgb(colors.green)).into_any_element(),
                        // Nothing is in progress once the turn has ended.
                        Step::Doing if thread.state.busy() => spinner(colors).into_any_element(),
                        Step::Doing | Step::Todo => icon(Glyph::CircleDashed).text_color(rgb(colors.quiet)).into_any_element(),
                    })
                    .child(one_line(text.clone(), 18.).flex_1().min_w_0().when(*step == Step::Done, |s| s.text_color(rgb(colors.muted)))),
            );
        }

        // What the agent has looked at and changed, newest first.
        let changes = thread.changes();
        let files = thread.files();
        let mut file_rows = col();
        for (i, (path, touch)) in files.iter().take(FILES_SHOWN).enumerate() {
            let note = match touch {
                Touch::Read => label(colors, "read"),
                Touch::Created => label(colors, "new").text_color(rgb(colors.green)),
                Touch::Edited => match changes.iter().find(|c| &c.path == path) {
                    Some(c) => stat(colors, c.adds as u32, c.dels as u32),
                    None => label(colors, "edited").text_color(rgb(colors.amber)),
                },
            };
            let target = match touch {
                Touch::Read => TabKind::File(path.clone()),
                _ => TabKind::Diff(path.clone()),
            };
            file_rows = file_rows.child(
                item(
                    colors,
                    icon(Glyph::FileCode).text_color(rgb(if *touch == Touch::Read { colors.muted } else { colors.amber })),
                    &project.relative(path).to_string_lossy(),
                    note,
                )
                .id(("context-file", i))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(colors.hover)))
                .on_click(cx.listener(move |s, _, _, cx| s.open(target.clone(), cx))),
            );
        }
        if files.len() > FILES_SHOWN {
            file_rows = file_rows.child(label(colors, format!("{} more", files.len() - FILES_SHOWN)).px_3().py_1());
        }

        // Instruction files agents conventionally load, found on disk.
        let found = self.instructions.get(&project.path).cloned().unwrap_or_default();
        let mut instructions = col();
        for (i, (path, scope)) in found.iter().enumerate() {
            let shown = match project.path.as_deref().and_then(|root| path.strip_prefix(root).ok()) {
                Some(rel) => rel.to_string_lossy().into_owned(),
                None => tilde(path),
            };
            let path = path.clone();
            instructions = instructions.child(
                item(colors, icon(Glyph::FileText).text_color(rgb(colors.muted)), &shown, label(colors, *scope))
                    .id(("instruction", i))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .on_click(cx.listener(move |s, _, _, cx| s.open(TabKind::File(path.clone()), cx))),
            );
        }
        let folders = self.folders_section(cx);
        col()
            .pb_3()
            .children(folders)
            .when(thread.usage.is_some_and(|(_, size)| size > 0), |s| {
                s.child(col().px_3().pt_3().pb_2().child(meter))
            })
            .when(!thread.plan.is_empty(), |s| {
                let total = thread.plan.len();
                let done = thread.plan.iter().filter(|(step, _)| *step == Step::Done).count();
                s.child(section(colors, format!("Plan · {done} of {total} done"), None)).child(plan)
            })
            .when(!files.is_empty(), |s| s.child(section(colors, "Files", Some(files.len()))).child(file_rows))
            .when(!found.is_empty(), |s| s.child(section(colors, "Instructions", Some(found.len()))).child(instructions))
    }

    /// The rows under `dir`, walking into every open folder.
    fn tree_rows(&self, dir: &Path, depth: usize, out: &mut Vec<(usize, Entry)>) {
        for entry in self.listings.get(dir).into_iter().flatten() {
            out.push((depth, entry.clone()));
            if entry.dir && self.expanded.contains(&entry.path) {
                self.tree_rows(&entry.path, depth + 1, out);
            }
        }
    }

    fn files_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let roots = self.current_project().roots();
        if roots.is_empty() {
            return empty(colors, Glyph::Files, "No folder", "Chats aren't tied to a project. Add one from the rail to browse its files.").h(px(220.));
        }
        let open = match &self.tabs[self.active].kind {
            TabKind::File(path) => Some(path.clone()),
            _ => None,
        };
        // Git's view of every folder in the project, by absolute path. Files the
        // agent edited outside any repository count as modified.
        let mut changed: HashMap<PathBuf, u8> = HashMap::new();
        for root in &roots {
            if let Some(Ok(snapshot)) = self.git_snapshots.get(root) {
                for e in &snapshot.entries {
                    let status = if e.unstaged() { e.status(false) } else { e.status(true) };
                    changed.insert(snapshot.root.join(&e.path), status);
                }
            }
        }
        if let Some(thread) = self.current_thread() {
            for change in thread.changes() {
                changed.entry(change.path).or_insert(if change.old.is_none() { b'A' } else { b'M' });
            }
        }
        // A folder shows the weightiest change anywhere beneath it.
        let inside = |folder: &Path| {
            changed.iter().filter(|(p, _)| p.starts_with(folder) && p.as_path() != folder).map(|(_, s)| *s).max_by_key(|s| weight(*s))
        };
        let mut rows = Vec::new();
        if let [root] = roots.as_slice() {
            self.tree_rows(root, 0, &mut rows);
        } else {
            // Several folders: each is a top-level folder of its own.
            for root in &roots {
                let name = crate::model::folder_name(root);
                rows.push((0, Entry { name: name.into(), path: root.clone(), dir: true }));
                if self.expanded.contains(root) {
                    self.tree_rows(root, 1, &mut rows);
                }
            }
        }
        let mut list = col().py_1();
        for (i, (depth, entry)) in rows.into_iter().enumerate() {
            let expanded = self.expanded.contains(&entry.path);
            let active = open.as_ref() == Some(&entry.path);
            let status = if entry.dir { inside(&entry.path) } else { changed.get(&entry.path).copied() };
            let mark = status.map(|s| change_mark(colors, s));
            let (path, dir) = (entry.path.clone(), entry.dir);
            list = list.child(
                row()
                    .id(("node", i))
                    .h(px(26.))
                    .mx_1()
                    .pl(px(8. + depth as f32 * 14.))
                    .pr_2()
                    .gap(px(6.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(active, |s| s.bg(rgb(colors.selected)))
                    .when(!active, |s| s.hover(|s| s.bg(rgb(colors.hover))))
                    .child(if dir {
                        icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(px(12.))
                            .text_color(rgb(colors.quiet))
                            .into_any_element()
                    } else {
                        div().w(px(12.)).flex_shrink_0().into_any_element()
                    })
                    .child(if dir {
                        icon(if expanded { IconName::FolderOpen } else { IconName::Folder }).text_color(rgb(colors.muted))
                    } else {
                        let (glyph, tint) = super::file_kind::file_kind(&entry.path);
                        icon(Icon::empty().path(glyph)).text_color(rgb(tint.color(colors)))
                    })
                    .child(one_line(entry.name.clone(), 18.).flex_1().min_w_0().when_some(mark, |s, (_, hue)| s.text_color(rgb(hue))))
                    .when_some(mark, |s, (letter, hue)| {
                        s.child(if dir {
                            // Folders get a dot: the letters are for the files inside.
                            div().size(px(6.)).mr(px(3.)).flex_shrink_0().rounded_full().bg(rgb(hue)).into_any_element()
                        } else {
                            div()
                                .w(px(12.))
                                .flex_shrink_0()
                                .text_center()
                                .font_family(mono())
                                .text_size(px(crate::theme::scaled_mono(11.)))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(hue))
                                .child(letter.to_string())
                                .into_any_element()
                        })
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if dir {
                            s.toggle_folder(&path, cx)
                        } else {
                            s.open(TabKind::File(path.clone()), cx)
                        }
                    })),
            );
        }
        list
    }

    pub(super) fn side_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let has_changes = self.git_snapshot().is_some_and(|s| !s.entries.is_empty());
        let mut strip = row().h(px(BAR)).flex_shrink_0().bg(rgb(colors.chrome));
        for (i, side) in Side::ALL.into_iter().enumerate() {
            let active = self.side == side;
            let tip = format!("{} · ⌘ {}", side.title(), i + 1);
            strip = strip.child(
                row()
                    .id(("side-tab", i))
                    .relative()
                    .w(px(40.))
                    .h_full()
                    .justify_center()
                    .border_r_1()
                    .border_color(rgb(colors.border))
                    .cursor_pointer()
                    .map(|s| {
                        if active {
                            s.bg(rgb(colors.background)).text_color(rgb(colors.blue))
                        } else {
                            s.border_b_1().text_color(rgb(colors.muted)).hover(|s| s.text_color(rgb(colors.text)))
                        }
                    })
                    .child(icon(side_glyph(side)))
                    .when(side == Side::Changes && has_changes, |s| {
                        // The repository has staged or unstaged changes.
                        s.child(div().absolute().top(px(8.)).right(px(9.)).size(px(5.)).rounded_full().bg(rgb(colors.amber)))
                    })
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.side = side;
                        cx.notify();
                    })),
            );
        }
        strip = strip.child(
            row()
                .flex_1()
                .h_full()
                .px_3()
                .border_b_1()
                .border_color(rgb(colors.border))
                .child(label(colors, self.side.title())),
        );
        if self.side == Side::Changes {
            return col()
                .size_full()
                .bg(rgb(colors.chrome))
                .border_l_1()
                .border_color(rgb(colors.border))
                .child(strip)
                .child(self.changes_page(cx))
                .into_any_element();
        }
        let page = match self.side {
            Side::Context => self.context_page(cx),
            Side::Changes => self.changes_page(cx),
            Side::Files => self.files_page(cx),
        };
        col()
            .size_full()
            .bg(rgb(colors.chrome))
            .border_l_1()
            .border_color(rgb(colors.border))
            .child(strip)
            .child(div().id("side-page").flex_1().min_h_0().overflow_y_scroll().child(page))
            .into_any_element()
    }
}
