//! The frame around a thread: title bar, project rail, thread list, tab strip
//! and status bar.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::{
    Icon, IconName, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    input::Input,
    tooltip::Tooltip,
};

pub fn tab_glyph(kind: &TabKind) -> Icon {
    match kind {
        TabKind::Chat => Glyph::Message.into(),
        TabKind::File(_) => Glyph::FileCode.into(),
        TabKind::Diff(_) | TabKind::GitDiff(..) => Glyph::FileDiff.into(),
        TabKind::Terminal => Glyph::Terminal.into(),
    }
}

/// An agent's mark: Claude's or OpenAI's for theirs, a robot for anything else.
pub fn agent_glyph(spec: Option<&crate::settings::AgentSpec>) -> Icon {
    match spec.and_then(|s| s.brand()) {
        Some(glyph) => glyph.into(),
        None => IconName::Bot.into(),
    }
}

/// A thread's state as a small leading mark.
fn state_mark(colors: Palette, state: ThreadState) -> AnyElement {
    match state {
        ThreadState::Running => spinner(colors).into_any_element(),
        ThreadState::Waiting => icon(Glyph::CircleDot).size(px(13.)).text_color(rgb(colors.blue)).into_any_element(),
        ThreadState::Done => icon(IconName::Check).size(px(13.)).text_color(rgb(colors.quiet)).into_any_element(),
        ThreadState::Idle => icon(Glyph::CircleDashed).size(px(13.)).text_color(rgb(colors.quiet)).into_any_element(),
        ThreadState::Failed => icon(IconName::TriangleAlert).size(px(13.)).text_color(rgb(colors.red)).into_any_element(),
    }
}

/// The widest a tab's label grows before it ends in an ellipsis.
const TAB_LABEL: f32 = 200.;

fn initials(name: &str) -> String {
    name.chars().take(2).collect::<String>().to_uppercase()
}

/// A project's colour, from the same seed as its avatar, so it stays put when
/// projects move.
fn project_hue(colors: Palette, path: &Path) -> u32 {
    colors.lane(crate::avatar::hash(path) as usize)
}

/// A project's face: its avatar where one is picked and on disk, else its
/// initials on its colour.
fn face(name: &str, hue: u32, file: Option<PathBuf>, size: f32, strong: bool) -> AnyElement {
    match file {
        Some(file) => img(file).size(px(size)).rounded_full().into_any_element(),
        None => div()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(Hsla::from(rgb(hue)).opacity(if strong { 0.24 } else { 0.14 }))
            .text_color(rgb(hue))
            .text_size(px(size * 0.4))
            .font_weight(FontWeight::SEMIBOLD)
            .child(initials(name))
            .into_any_element(),
    }
}

/// A project being dragged along the rail, to a new place.
#[derive(Clone)]
struct DraggedProject {
    index: usize,
    name: String,
    hue: u32,
    file: Option<PathBuf>,
}
impl Render for DraggedProject {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().opacity(0.85).child(face(&self.name, self.hue, self.file.clone(), 30., true))
    }
}

impl Workspace {
    pub(super) fn agent_glyph(&self, agent: usize) -> Icon {
        agent_glyph(self.settings.agents.get(agent))
    }
    /// What a right-click on a project offers.
    fn project_menu(&mut self, index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // The avatar choices are fetched as the menu opens, so they show as it does.
        if let Some(path) = self.projects.get(index).and_then(|p| p.path.clone()) {
            for avatar in crate::settings::Avatar::ALL {
                self.ensure_avatar(avatar, &path, cx);
            }
        }
        let weak = cx.weak_entity();
        let build: crate::menu::Builder = Rc::new(move |cx: &App| {
            let Some(this) = weak.upgrade() else { return Vec::new() };
            let Some(project) = this.read(cx).projects.get(index) else { return Vec::new() };
            let mut entries = vec![crate::menu::Entry::Label(project.name.clone().into())];
            let rename = weak.clone();
            entries.push(crate::menu::Entry::Item(
                crate::menu::Item::new("Rename…").icon(Glyph::Pencil).on_click(move |window, cx| {
                    let _ = rename.update(cx, |s, cx| s.start_rename(index, position, window, cx));
                }),
            ));
            let add = weak.clone();
            entries.push(crate::menu::Entry::Item(
                crate::menu::Item::new("Add folder…").icon(Glyph::FolderPlus).on_click(move |window, cx| {
                    let _ = add.update(cx, |s, cx| s.add_folders(index, window, cx));
                }),
            ));
            // At the very bottom: how the project shows on the rail.
            if let Some(path) = project.path.clone() {
                let colors = palette(cx);
                let hue = project_hue(colors, &path);
                let workspace = this.read(cx);
                let swatches = crate::settings::Avatar::ALL
                    .into_iter()
                    .map(|avatar| {
                        let file = workspace.avatar_file(avatar, &path);
                        let content = match (avatar.style(), file) {
                            (None, _) => face(&project.name, hue, None, 24., true),
                            (Some(_), Some(file)) => face(&project.name, hue, Some(file), 24., true),
                            // Still downloading, or it couldn't be.
                            (Some(_), None) => div().size(px(24.)).rounded_full().bg(rgb(colors.hover)).into_any_element(),
                        };
                        let pick = weak.clone();
                        crate::menu::Swatch::new(avatar.label(), content).checked(project.avatar == avatar).on_click(
                            move |_, cx| {
                                let _ = pick.update(cx, |s, cx| s.set_avatar(index, avatar, cx));
                            },
                        )
                    })
                    .collect();
                entries.push(crate::menu::Entry::Separator);
                entries.push(crate::menu::Entry::Swatches(swatches));
            }
            entries
        });
        self.open_popover("project-menu", position, Corner::TopLeft, 200., None, build, window, cx);
    }

    pub(super) fn title(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let project = self.current_project();
        TitleBar::new()
            .bg(rgb(colors.chrome))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                row()
                    .w_full()
                    .min_w_0()
                    .pr_2()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .child(icon(Glyph::Sparkle).text_color(rgb(colors.blue)))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Chat"))
                    .child(div().text_color(rgb(colors.quiet)).child("/"))
                    .child(div().text_color(rgb(colors.muted)).child(project.name.clone()))
                    .when_some(self.current_thread(), |s, t| {
                        s.child(div().text_color(rgb(colors.quiet)).child("/"))
                            .child(div().min_w_0().truncate().child(t.title.clone()))
                    })
                    .child(div().flex_1())
                    .child(
                        toggle(colors, "toggle-side", IconName::PanelRight, "Toggle side panel · ⌘ ⌥ B", self.side_open)
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.side_open = !s.side_open;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Projects pinned down the far left; regular chat sits above them.
    pub(super) fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let mut list = col().items_center().pt_2().gap(px(2.));
        for (i, p) in self.projects.iter().enumerate() {
            let active = self.project == i;
            let hue = p.path.as_deref().map_or(colors.muted, |path| project_hue(colors, path));
            let file = p.path.as_deref().and_then(|path| self.avatar_file(p.avatar, path));
            // Where a dragged project would land: a dash above this one (gap `i`),
            // or below the last (gap `len`). Gaps either side of the dragged
            // project would leave it where it is, so they show nothing.
            let dash = self.drop_slot.filter(|_| cx.has_active_drag()).and_then(|(from, slot)| {
                let still = slot == from || slot == from + 1;
                match () {
                    _ if still || p.path.is_none() => None,
                    _ if slot == i => Some(false),
                    _ if slot == i + 1 && i + 1 == self.projects.len() => Some(true),
                    _ => None,
                }
            });
            let running = p.threads.iter().any(|t| t.state == ThreadState::Running);
            let review = p.threads.iter().any(|t| t.state == ThreadState::Waiting);
            let tip = match &p.path {
                Some(path) if p.folders.is_empty() => format!("{} · {}", p.name, tilde(path)),
                Some(path) => format!("{} · {} and {} more", p.name, tilde(path), p.folders.len()),
                None => "Chats · not tied to a project".to_string(),
            };
            list = list.child(
                row()
                    .id(("project", i))
                    .relative()
                    .w(px(RAIL))
                    .h(px(42.))
                    .justify_center()
                    .cursor_pointer()
                    .when(active, |s| {
                        s.child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(11.))
                                .w(px(3.))
                                .h(px(20.))
                                .rounded(px(2.))
                                .bg(rgb(colors.blue)),
                        )
                    })
                    .child(
                        div()
                            .id(("project-face", i))
                            .relative()
                            .size(px(34.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_2()
                            .border_color(if active { Hsla::from(rgb(colors.blue)) } else { transparent_black() })
                            .text_color(rgb(hue))
                            .map(|s| match (&p.path, file.clone()) {
                                // An avatar fills the ring; hovering lifts it a little.
                                (Some(_), Some(file)) => {
                                    s.when(!active, |s| s.opacity(0.9).hover(|s| s.opacity(1.))).child(face(&p.name, hue, Some(file), 30., active))
                                }
                                (Some(_), None) => s
                                    .bg(Hsla::from(rgb(hue)).opacity(if active { 0.24 } else { 0.14 }))
                                    .when(!active, |s| s.hover(|s| s.bg(Hsla::from(rgb(hue)).opacity(0.24))))
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(initials(&p.name)),
                                (None, _) => s
                                    .bg(Hsla::from(rgb(hue)).opacity(if active { 0.24 } else { 0.14 }))
                                    .when(!active, |s| s.hover(|s| s.bg(Hsla::from(rgb(hue)).opacity(0.24))))
                                    .child(icon(Glyph::Message).size(px(15.))),
                            })
                            .when(running || review, |s| {
                                s.child(
                                    div()
                                        .absolute()
                                        .top(px(-3.))
                                        .right(px(-3.))
                                        .size(px(10.))
                                        .rounded_full()
                                        .border_2()
                                        .border_color(rgb(colors.chrome))
                                        .bg(rgb(if running { colors.amber } else { colors.blue })),
                                )
                            }),
                    )
                    .map(|s| match dash {
                        Some(below) => s.child(
                            div()
                                .absolute()
                                .left(px((RAIL - 24.) / 2.))
                                .when(below, |s| s.bottom(px(-2.5)))
                                .when(!below, |s| s.top(px(-2.5)))
                                .w(px(24.))
                                .h(px(3.))
                                .rounded_full()
                                .bg(rgb(colors.blue)),
                        ),
                        None => s,
                    })
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_project(i, cx)))
                    .when(p.path.is_some(), |s| {
                        let dragged = DraggedProject { index: i, name: p.name.clone(), hue, file: file.clone() };
                        s.on_mouse_down(MouseButton::Right, cx.listener(move |s, e: &MouseDownEvent, window, cx| {
                            s.project_menu(i, e.position, window, cx);
                        }))
                        // Drag a project onto another to take its place.
                        .on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                        // The gap the cursor is nearest: above this project, or below it.
                        .on_drag_move(cx.listener(move |s, e: &DragMoveEvent<DraggedProject>, _, cx| {
                            if !e.bounds.contains(&e.event.position) {
                                return;
                            }
                            let below = e.event.position.y > e.bounds.center().y;
                            let slot = Some((e.drag(cx).index, i + below as usize));
                            if s.drop_slot != slot {
                                s.drop_slot = slot;
                                cx.notify();
                            }
                        }))
                    }),
            );
            // Regular chat is its own thing: a rule sets it apart from projects.
            if p.path.is_none() {
                list = list.child(div().w(px(24.)).h(px(1.)).my(px(4.)).bg(rgb(colors.border)));
            }
        }
        list = list.child(
            row().w(px(RAIL)).h(px(42.)).justify_center().child(
                div()
                    .id("add-project")
                    .size(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(colors.border))
                    .text_color(rgb(colors.muted))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
                    .child(icon(IconName::Plus))
                    .tooltip(|w, cx| Tooltip::new("Add project · ⌘ O").build(w, cx))
                    .on_click(cx.listener(|s, _, w, cx| s.add_project(&AddProject, w, cx))),
            ),
        );
        col()
            .w(px(RAIL))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(colors.chrome))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .id("rail-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    // Dropped anywhere on the rail, even between two projects:
                    // it lands in the gap the dash showed.
                    .on_drop(cx.listener(|s, d: &DraggedProject, _, cx| {
                        if let Some((_, slot)) = s.drop_slot.take() {
                            // Taking the project out shifts the gaps below it up by one.
                            s.move_project(d.index, if slot > d.index { slot - 1 } else { slot }, cx);
                        }
                        cx.notify();
                    }))
                    .child(list),
            )
            .child(
                row()
                    .h(px(BAR + 4.))
                    .flex_shrink_0()
                    .justify_center()
                    .child(
                        icon_button("open-prefs", IconName::Settings, "Settings · ⌘ ,")
                            .on_click(cx.listener(|s, _, w, cx| s.open_prefs(&OpenPreferences, w, cx))),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn threads_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let project = self.current_project();
        let q = self.thread_search.read(cx).value().trim().to_lowercase();
        let mut list = col().py_1();
        let mut shown = 0;
        for (i, t) in project.threads.iter().enumerate() {
            if !q.is_empty() && !t.title.to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let active = self.thread == i;
            // The row is just state and title; the rest waits in the tooltip.
            let tip = format!(
                "{} · {}",
                self.agent_name(t),
                match t.state {
                    ThreadState::Running => "Working",
                    ThreadState::Waiting => "Waiting for your answer",
                    ThreadState::Done => "Finished",
                    ThreadState::Failed => "Failed",
                    ThreadState::Idle => "Idle",
                }
            );
            list = list.child(
                row()
                    .id(("thread", i))
                    .h(px(30.))
                    .mx_1()
                    .px_2()
                    .gap_2()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(active, |s| s.bg(rgb(colors.selected)))
                    .when(!active, |s| s.hover(|s| s.bg(rgb(colors.hover))))
                    .child(row().w(px(14.)).flex_shrink_0().justify_center().child(state_mark(colors, t.state)))
                    .child(one_line(t.title.clone(), 18.).flex_1().min_w_0())
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_thread(i, cx))),
            );
        }
        if shown == 0 {
            list = list.child(div().p_3().text_color(rgb(colors.muted)).child(if q.is_empty() { "No threads yet" } else { "No matches" }));
        }
        col()
            .size_full()
            .bg(rgb(colors.chrome))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                row()
                    .h(px(BAR))
                    .flex_shrink_0()
                    .pl(px(6.))
                    .pr_1()
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        Input::new(&self.thread_search)
                            .flex_1()
                            .small()
                            .appearance(false)
                            .prefix(icon(IconName::Search).text_color(rgb(colors.muted)))
                            .cleanable(true),
                    )
                    .child(
                        icon_button("new-thread", Glyph::Compose, "New thread · ⌘ N")
                            .on_click(cx.listener(|s, _, w, cx| s.new_thread(&NewThread, w, cx))),
                    ),
            )
            .child(div().id("thread-list").flex_1().min_h_0().overflow_y_scroll().child(list))
            .into_any_element()
    }

    pub(super) fn tabs_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        // The chat stays put at the start; the other tabs scroll behind it.
        let mut pinned_tab = None;
        let mut tabs = row().id("tabs-scroll").h_full().overflow_x_scroll();
        for (i, t) in self.tabs.iter().enumerate() {
            let active = self.active == i;
            let pinned = t.kind == TabKind::Chat;
            let group = SharedString::from(format!("tab-{}", t.id));
            let (name, detail) = match &t.kind {
                TabKind::Chat => ("Chat".to_string(), String::new()),
                TabKind::File(path) | TabKind::Diff(path) | TabKind::GitDiff(path, _) => {
                    let relative = self.current_project().relative(path).to_string_lossy().into_owned();
                    let (name, dir) = split_path(&relative);
                    let detail = match &t.kind {
                        TabKind::GitDiff(_, staged) => {
                            let section = if *staged { "Staged" } else { "Unstaged" };
                            if dir.is_empty() { section.to_string() } else { format!("{section} · {dir}") }
                        }
                        _ => dir.to_string(),
                    };
                    (name.to_string(), detail)
                }
                TabKind::Terminal => ("Terminal".to_string(), String::new()),
            };
            let tab = row()
                    .id(("tab", t.id))
                    .group(group.clone())
                    .h_full()
                    .pl_3()
                    .pr_1()
                    .gap(px(6.))
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(rgb(colors.border))
                    .cursor_pointer()
                    .map(|s| {
                        if active {
                            s.bg(rgb(colors.background)).text_color(rgb(colors.text))
                        } else {
                            s.border_b_1()
                                .text_color(rgb(colors.muted))
                                .hover(|s| s.text_color(rgb(colors.text)))
                        }
                    })
                    .child(icon(match (&t.kind, self.current_thread()) {
                        // The chat wears its agent's mark.
                        (TabKind::Chat, Some(thread)) => self.agent_glyph(thread.agent),
                        (kind, _) => tab_glyph(kind),
                    }).size(px(13.)).text_color(rgb(match (&t.kind, active) {
                        (TabKind::Diff(_) | TabKind::GitDiff(..), _) => colors.amber,
                        (_, true) => colors.blue,
                        _ => colors.muted,
                    })))
                    .child({
                        // Sized to the label, up to a cap past which it ends in an ellipsis.
                        let shown = if detail.is_empty() { name.clone() } else { format!("{name}  {detail}") };
                        let width = (text_width(window, &shown, 13.) + px(2.)).min(px(TAB_LABEL));
                        let detail = (!detail.is_empty()).then(|| (SharedString::from(detail), Hsla::from(rgb(colors.quiet))));
                        line_with_detail(name, detail, 18.).w(width).flex_shrink_0()
                    })
                    .child(if pinned {
                        // The chat can't be closed: a pin sits where the close button would.
                        row()
                            .size(px(22.))
                            .justify_center()
                            .child(icon(Glyph::Pin).size(px(11.)).text_color(rgb(colors.quiet)))
                    } else {
                        row().when(!active, |s| s.opacity(0.).group_hover(group, |s| s.opacity(1.))).child(
                            Button::new(("close", t.id))
                                .icon(icon(IconName::Close))
                                .ghost()
                                .xsmall()
                                .tooltip("Close tab · ⌘ W")
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    cx.stop_propagation();
                                    s.close(i, cx);
                                })),
                        )
                    })
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |s, _, _, cx| s.close(i, cx)))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.active = i;
                        s.focus_terminal(cx);
                        cx.notify();
                    }));
            if pinned {
                pinned_tab = Some(tab);
            } else {
                tabs = tabs.child(tab);
            }
        }
        row()
            .h(px(BAR))
            .flex_shrink_0()
            .bg(rgb(colors.chrome))
            .children(pinned_tab.map(|tab| tab.flex_shrink_0()))
            .child(div().min_w_0().h_full().child(tabs))
            .child(div().flex_1().h_full().border_b_1().border_color(rgb(colors.border)))
            .child(
                row()
                    .h_full()
                    .flex_shrink_0()
                    .px_1()
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(
                        icon_button("open-terminal", Glyph::Terminal, "Terminal · ⌃ `")
                            .on_click(cx.listener(|s, _, _, cx| s.open(TabKind::Terminal, cx))),
                    )
                    .child(icon_button("new-tab", IconName::Plus, "Open a file").on_click(cx.listener(|s, _, _, cx| {
                        s.side = Side::Files;
                        s.side_open = true;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    pub(super) fn content(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        match &self.tabs[self.active].kind {
            TabKind::Chat => self.chat_view(window, cx),
            TabKind::File(path) => self.file_view(path.clone(), window, cx),
            TabKind::Diff(path) => self.diff_view(path.clone(), window, cx),
            TabKind::GitDiff(path, staged) => self.git_diff_view(path, *staged, window, cx),
            TabKind::Terminal => self.terminal_view(cx),
        }
    }

    pub(super) fn status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let working = self.projects.iter().flat_map(|p| &p.threads).filter(|t| t.state.busy()).count();
        row()
            .h(px(28.))
            .flex_shrink_0()
            .pl_1()
            .pr_2()
            .gap(px(6.))
            .bg(rgb(colors.chrome))
            .border_t_1()
            .border_color(rgb(colors.border))
            .text_size(px(12.))
            .text_color(rgb(colors.muted))
            .child(
                toggle(colors, "toggle-threads", IconName::PanelLeft, "Toggle threads · ⌘ B", self.threads_open)
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.threads_open = !s.threads_open;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .when(working > 0, |s| {
                s.child(
                    row()
                        .gap(px(6.))
                        .child(spinner(colors))
                        .child(format!("{working} thread{} working", if working == 1 { "" } else { "s" })),
                )
                .child(divider(colors))
            })
            .when_some(self.current_thread(), |s, t| {
                s.child(match self.setting(Role::Model) {
                    Some(model) => format!("{} · {}", self.agent_name(t), model.current_name()),
                    None => self.agent_name(t).to_string(),
                })
            })
            .into_any_element()
    }
}
