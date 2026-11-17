//! The right panel, tabbed by icon: what the agent has in context, the changes
//! it has made, and the project's files.
use super::ui::*;
use super::*;
use crate::{agent::Step, icons::Glyph, model::Touch};
use gpui_component::{Icon, IconName, tooltip::Tooltip};

fn side_glyph(side: Side) -> Icon {
    match side {
        Side::Context => IconName::Asterisk.into(),
        Side::Changes => Glyph::Branch.into(),
        Side::Files => Glyph::Files.into(),
    }
}

/// Files and commands listed before the rest are summed up as "N more".
const FILES_SHOWN: usize = 12;
const COMMANDS_SHOWN: usize = 12;

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

/// A label and its value on one line, for the session's facts.
fn fact(colors: Palette, name: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    row()
        .h(px(24.))
        .px_3()
        .gap_3()
        .text_size(px(12.5))
        .child(div().flex_shrink_0().text_color(rgb(colors.muted)).child(name.into()))
        .child(div().flex_1())
        .child(div().min_w_0().truncate().child(value.into()))
}

impl Workspace {
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
                            .child(div().text_size(px(20.)).line_height(px(22.)).font_weight(FontWeight::MEDIUM).child(tokens(used)))
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
                            .text_size(px(12.))
                            .text_color(rgb(colors.muted))
                            .child(format!("{} left", tokens(size.saturating_sub(used))))
                            .child(div().flex_1())
                            .when_some(thread.cost.as_ref(), |s, (amount, currency)| s.child(money(*amount, currency))),
                    )
            }
            _ => col().child(label(colors, "Context use shows here once the agent reports it.").line_height(relative(1.45))),
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
        if found.is_empty() {
            instructions = instructions.child(
                label(colors, "No AGENTS.md or CLAUDE.md here or in your home folder.").px_3().py_1().line_height(relative(1.45)),
            );
        }

        let mut commands = col();
        for (name, description) in thread.commands.iter().take(COMMANDS_SHOWN) {
            commands = commands.child(
                row().h(px(26.)).px_3().gap_2().child(
                    line_with_detail(format!("/{name}"), Some((description.clone().into(), rgb(colors.quiet).into())), 18.)
                        .flex_1()
                        .min_w_0(),
                ),
            );
        }
        if thread.commands.len() > COMMANDS_SHOWN {
            commands = commands.child(label(colors, format!("{} more", thread.commands.len() - COMMANDS_SHOWN)).px_3().py_1());
        }

        let mut session = col().child(fact(colors, "Agent", self.agent_name(thread).to_string()));
        for setting in &thread.settings {
            session = session.child(fact(colors, setting.name.clone(), setting.current_name().to_string()));
        }
        session = session.child(fact(colors, "Folder", project.path.as_deref().map_or("Scratch".to_string(), tilde)));
        col()
            .pb_3()
            .child(col().px_3().pt_3().pb_2().child(meter))
            .when(!thread.plan.is_empty(), |s| s.child(section(colors, "Plan", None)).child(plan))
            .when(!files.is_empty(), |s| s.child(section(colors, "Files", Some(files.len()))).child(file_rows))
            .child(section(colors, "Instructions", (!found.is_empty()).then_some(found.len())))
            .child(instructions)
            .child(section(colors, "Session", None))
            .child(session)
            .when(!thread.commands.is_empty(), |s| {
                s.child(section(colors, "Commands", Some(thread.commands.len()))).child(commands)
            })
    }

    fn changes_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let changes = self.current_thread().map(|t| t.changes()).unwrap_or_default();
        if changes.is_empty() {
            return empty(colors, Glyph::FileDiff, "No changes yet", "Files this thread's agent edits are listed here.").h(px(220.));
        }
        let open = match &self.tabs[self.active].kind {
            TabKind::Diff(path) => Some(path.clone()),
            _ => None,
        };
        let (adds, dels) = changes.iter().fold((0, 0), |(a, d), c| (a + c.adds, d + c.dels));
        let mut list = col().py_1();
        for (i, c) in changes.iter().enumerate() {
            let active = open.as_ref() == Some(&c.path);
            let path = c.path.clone();
            list = list.child(
                item(
                    colors,
                    status_mark(colors, c.old.is_none()),
                    &self.current_project().relative(&c.path).to_string_lossy(),
                    stat(colors, c.adds as u32, c.dels as u32),
                )
                .id(("change", i))
                .cursor_pointer()
                .when(active, |s| s.bg(rgb(colors.selected)))
                .when(!active, |s| s.hover(|s| s.bg(rgb(colors.hover))))
                .on_click(cx.listener(move |s, _, _, cx| s.open(TabKind::Diff(path.clone()), cx))),
            );
        }
        col()
            .child(
                row()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(colors.row_border))
                    .child(label(colors, format!("{} file{}", changes.len(), if changes.len() == 1 { "" } else { "s" })))
                    .child(div().flex_1())
                    .child(stat(colors, adds as u32, dels as u32)),
            )
            .child(list)
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
        let Some(root) = self.current_project().path.clone() else {
            return empty(colors, Glyph::Files, "No folder", "Chats aren't tied to a project. Add one from the rail to browse its files.").h(px(220.));
        };
        let open = match &self.tabs[self.active].kind {
            TabKind::File(path) => Some(path.clone()),
            _ => None,
        };
        let changed: Vec<PathBuf> = self.current_thread().map(|t| t.changes().into_iter().map(|c| c.path).collect()).unwrap_or_default();
        let mut rows = Vec::new();
        self.tree_rows(&root, 0, &mut rows);
        let mut list = col().py_1();
        for (i, (depth, entry)) in rows.into_iter().enumerate() {
            let expanded = self.expanded.contains(&entry.path);
            let active = open.as_ref() == Some(&entry.path);
            let edited = changed.contains(&entry.path);
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
                    .child(
                        icon(match (dir, expanded) {
                            (true, true) => Icon::from(IconName::FolderOpen),
                            (true, false) => Icon::from(IconName::Folder),
                            _ => Icon::from(IconName::File),
                        })
                        .text_color(rgb(colors.muted)),
                    )
                    .child(one_line(entry.name.clone(), 18.).flex_1().min_w_0().when(edited, |s| s.text_color(rgb(colors.amber))))
                    .when(edited, |s| s.child(status_mark(colors, false)))
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
        let has_changes = self.current_thread().is_some_and(|t| !t.changes().is_empty());
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
                        // The agent has edited files in this thread.
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
