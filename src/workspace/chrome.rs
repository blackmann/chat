//! The frame around a thread: title bar, project rail, thread list, tab strip
//! and status bar.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui::{Animation, AnimationExt};
use std::time::Duration;
use gpui_component::{
    Icon, IconName, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    input::Input,
    scroll::ScrollableElement,
    tooltip::Tooltip,
};

/// Threads with nothing new for this long fade back in the list: three days.
const STALE_THREAD_MS: i64 = 3 * 24 * 60 * 60 * 1000;

/// The thread list's sections, by the day a thread's sort time falls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Day {
    Today,
    Yesterday,
    Older,
}
impl Day {
    fn title(self) -> &'static str {
        match self {
            Day::Today => "Today",
            Day::Yesterday => "Yesterday",
            Day::Older => "Older",
        }
    }
}

/// The order the list shows `threads` in, newest first by `sort`, each with
/// its section. A thread with nothing in it yet is new: it leads. One with no
/// times to go on goes last, among the older ones. Ties keep the stored order.
pub(super) fn thread_order(threads: &[Thread], sort: ThreadSort, now: chrono::DateTime<chrono::Local>) -> Vec<(Day, usize)> {
    use chrono::TimeZone;
    let key = |t: &Thread| {
        if t.is_blank() {
            return Some(now.timestamp_millis());
        }
        match sort {
            ThreadSort::LastActive => t.last_active(),
            ThreadSort::Created => t.started(),
        }
    };
    let today = now.date_naive();
    let day = |at: Option<i64>| {
        let Some(date) = at.and_then(|ms| chrono::Local.timestamp_millis_opt(ms).single()).map(|d| d.date_naive()) else {
            return Day::Older;
        };
        if date >= today {
            Day::Today
        } else if Some(date) == today.pred_opt() {
            Day::Yesterday
        } else {
            Day::Older
        }
    };
    let mut order: Vec<(Option<i64>, usize)> = threads.iter().enumerate().map(|(i, t)| (key(t), i)).collect();
    // `None` sorts below every time.
    order.sort_by(|a, b| b.0.cmp(&a.0));
    order.into_iter().map(|(at, i)| (day(at), i)).collect()
}

pub fn tab_glyph(kind: &TabKind) -> Icon {
    match kind {
        TabKind::Chat => Glyph::Message.into(),
        TabKind::File(_) => Glyph::FileCode.into(),
        TabKind::Diff(_) | TabKind::GitDiff(..) => Glyph::FileDiff.into(),
        TabKind::Terminal(_) => Glyph::Terminal.into(),
        TabKind::ServerLog(..) => Glyph::Zap.into(),
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
fn state_mark(colors: Palette, state: ThreadState, unread: bool) -> AnyElement {
    match state {
        ThreadState::Running => spinner(colors).into_any_element(),
        ThreadState::Waiting => icon(Glyph::CircleDot).size(px(13.)).text_color(rgb(colors.blue)).into_any_element(),
        ThreadState::Done => div()
            .size(px(13.))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(6.)).rounded_full().bg(rgb(if unread { colors.blue } else { colors.quiet })))
            .into_any_element(),
        ThreadState::Idle => icon(Glyph::CircleDashed).size(px(13.)).text_color(rgb(colors.quiet)).into_any_element(),
        ThreadState::Failed => icon(IconName::TriangleAlert)
            .size(px(13.))
            .text_color(rgb(if unread { colors.red } else { colors.quiet }))
            .into_any_element(),
    }
}

/// The widest a tab's label grows before it ends in an ellipsis.
const TAB_LABEL: f32 = 200.;

fn initials(name: &str) -> String {
    name.chars().take(2).collect::<String>().to_uppercase()
}

/// How long a project goes without activity before its colour greys out.
const STALE_MS: i64 = 24 * 60 * 60 * 1000;

/// A project's colour, from its parent folder and name, so it stays put when
/// projects move. FNV-1a, so it doesn't change between Rust releases. Any hue
/// on the wheel, with a little play in saturation and lightness, kept bright
/// enough to read on the theme.
fn project_hue(colors: Palette, path: &Path) -> u32 {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let key = format!("{}/{}", path.parent().map(name).unwrap_or_default(), name(path));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let h = (hash % 360) as f32 / 360.;
    let s = 0.55 + ((hash >> 16) % 30) as f32 / 100.;
    let l = if colors.dark { 0.62 } else { 0.40 } + ((hash >> 32) % 10) as f32 / 100.;
    let c = Rgba::from(hsla(h, s, l, 1.));
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    byte(c.r) << 16 | byte(c.g) << 8 | byte(c.b)
}

/// A project's initials on a folder of its colour: a tab over a body, the
/// body squared off where the tab joins it so the two read as one shape.
/// `fill` is the body's opacity; `hover` lifts it under the group "project".
fn face(name: &str, hue: u32, size: f32, fill: f32, hover: Option<f32>) -> Div {
    let paint = |d: Div| {
        d.bg(Hsla::from(rgb(hue)).opacity(fill))
            .when_some(hover, |d, h| d.group_hover("project", move |s| s.bg(Hsla::from(rgb(hue)).opacity(h))))
    };
    let tab = size * 0.1;
    div()
        .relative()
        .size(px(size))
        .child(paint(div().absolute().top(px(size * 0.16)).left_0().w(px(size * 0.4)).h(px(tab)).rounded_t(px(3.))))
        .child(
            paint(
                div()
                    .absolute()
                    .top(px(size * 0.16 + tab))
                    .left_0()
                    .right_0()
                    .bottom(px(size * 0.06))
                    .rounded_tr(px(5.))
                    .rounded_b(px(5.)),
            )
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(hue))
            .text_size(px(crate::theme::scaled_ui(11.)))
            .font_weight(FontWeight::SEMIBOLD)
            .child(initials(name)),
        )
}

/// `core` breathing while an agent works: a halo swells out of it, fading as
/// it goes, once every 2.4s. The halo is painted rather than laid out, so it
/// grows by fractions of a pixel instead of stepping a whole one at a time, and
/// it fades over the whole beat, so there's no still gap before the next one.
fn pulse(id: impl Into<ElementId>, color: u32, core: Div) -> Div {
    const REACH: f32 = 4.;
    let halo = div().absolute().top(px(-REACH)).left(px(-REACH)).right(px(-REACH)).bottom(px(-REACH)).with_animation(
        id,
        Animation::new(Duration::from_millis(2400)).repeat(),
        move |halo, t| {
            let inset = REACH * (1. - t).powi(3);
            let alpha = 0.5 * (1. - t).powf(1.5);
            halo.child(
                canvas(
                    |_, _, _| {},
                    move |b, _, window, _| {
                        let ring = Bounds::new(
                            point(b.left() + px(inset), b.top() + px(inset)),
                            size(b.size.width - px(2. * inset), b.size.height - px(2. * inset)),
                        );
                        window.paint_quad(fill(ring, Hsla::from(rgb(color)).opacity(alpha)).corner_radii(ring.size.height / 2.));
                    },
                )
                .size_full(),
            )
        },
    );
    div().relative().child(halo).child(core)
}

/// A project being dragged along the rail, to a new place.
#[derive(Clone)]
struct DraggedProject {
    index: usize,
    name: String,
    hue: u32,
}
impl Render for DraggedProject {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().opacity(0.85).child(face(&self.name, self.hue, 34., 0.24, None))
    }
}

impl Workspace {
    pub(super) fn agent_glyph(&self, agent: usize) -> Icon {
        agent_glyph(self.settings.agents.get(agent))
    }
    /// What a right-click on a project offers.
    fn project_menu(&mut self, index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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
            if project.path.is_some() {
                let remove = weak.clone();
                entries.push(crate::menu::Entry::Separator);
                entries.push(crate::menu::Entry::Item(
                    crate::menu::Item::new("Remove project…").icon(IconName::Delete).on_click(move |window, cx| {
                        let _ = remove.update(cx, |s, cx| s.confirm_remove_project(index, window, cx));
                    }),
                ));
            }
            entries
        });
        self.open_popover("project-menu", position, Corner::TopLeft, 200., None, build, window, cx);
    }

    /// A right-click on a file or folder in the Files page.
    pub(super) fn file_menu(&mut self, path: PathBuf, dir: bool, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let editors = self.editors.clone();
        let weak = cx.weak_entity();
        let build: crate::menu::Builder = Rc::new(move |_: &App| {
            let mut entries = vec![open_with_item(&editors, &path, dir)];
            entries.push(reveal_item(&editors, &path));
            if !dir {
                entries.extend(link_entries(&weak, &path, None, position));
            }
            entries
        });
        self.open_popover("file-menu", position, Corner::TopLeft, 200., None, build, window, cx);
    }

    /// The right-click menu on a diff — a row that opens one, or its tab.
    pub(super) fn diff_menu(&mut self, path: PathBuf, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_menu_with(path, position, None, window, cx);
    }

    /// The diff menu, with `extra` entries after a separator.
    pub(super) fn diff_menu_with(&mut self, path: PathBuf, position: Point<Pixels>, extra: Option<crate::menu::Builder>, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let editors = self.editors.clone();
        let build: crate::menu::Builder = Rc::new(move |cx: &App| {
            let open = weak.clone();
            let target = path.clone();
            let mut entries = vec![crate::menu::Entry::Item(
                crate::menu::Item::new("View file").icon(Glyph::FileCode).disabled(!path.is_file()).on_click(move |_, cx| {
                    let _ = open.update(cx, |s, cx| s.open(TabKind::File(target.clone()), cx));
                }),
            )];
            if path.is_file() {
                entries.push(open_with_item(&editors, &path, false));
                entries.extend(link_entries(&weak, &path, None, position));
            }
            if let Some(extra) = &extra {
                entries.push(crate::menu::Entry::Separator);
                entries.extend(extra(cx));
            }
            entries
        });
        self.open_popover("diff-menu", position, Corner::TopLeft, 180., None, build, window, cx);
    }

    /// A tab's right-click menu. The tab is found by id when an item is picked,
    /// as its place in the strip may have changed since the menu opened.
    fn tab_menu(&mut self, id: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id) else { return };
        let (kind, pinned) = (tab.kind.clone(), tab.pinned);
        let others = self.tabs.iter().any(|t| t.id != id && !t.fixed());
        let weak = cx.weak_entity();
        let editors = self.editors.clone();
        let build: crate::menu::Builder = Rc::new(move |_: &App| {
            let act = |f: fn(&mut Workspace, usize, &mut Context<Workspace>)| {
                let weak = weak.clone();
                move |_: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |s, cx| {
                        if let Some(i) = s.tabs.iter().position(|t| t.id == id) {
                            f(s, i, cx);
                        }
                    });
                }
            };
            let mut entries = Vec::new();
            if let TabKind::Diff(path) | TabKind::GitDiff(path, _) = &kind {
                let open = weak.clone();
                let target = path.clone();
                entries.push(crate::menu::Entry::Item(
                    crate::menu::Item::new("View file").icon(Glyph::FileCode).disabled(!path.is_file()).on_click(move |_, cx| {
                        let _ = open.update(cx, |s, cx| s.open(TabKind::File(target.clone()), cx));
                    }),
                ));
            }
            if let TabKind::File(path) | TabKind::Diff(path) | TabKind::GitDiff(path, _) = &kind
                && path.is_file()
            {
                entries.push(open_with_item(&editors, path, false));
                entries.extend(link_entries(&weak, path, None, position));
            }
            if matches!(kind, TabKind::Terminal(_)) {
                entries.push(crate::menu::Entry::Item(
                    crate::menu::Item::new(if pinned { "Unpin" } else { "Pin" }).icon(Glyph::Pin).on_click(act(Workspace::toggle_pin)),
                ));
            }
            if !entries.is_empty() {
                entries.push(crate::menu::Entry::Separator);
            }
            entries.push(crate::menu::Entry::Item(
                crate::menu::Item::new("Close others").icon(IconName::Close).disabled(!others).on_click(act(Workspace::close_others)),
            ));
            entries
        });
        self.open_popover("tab-menu", position, Corner::TopLeft, 180., None, build, window, cx);
    }

    /// A right-click on a file's code: links to the lines picked, or the one under the pointer.
    pub(super) fn code_menu(&mut self, path: PathBuf, lines: (usize, usize), position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let build: crate::menu::Builder = Rc::new(move |_: &App| {
            let mut entries = link_entries(&weak, &path, Some(lines), position);
            if entries.is_empty() {
                entries.push(crate::menu::Entry::Item(crate::menu::Item::new("Not in a Git repository").disabled(true)));
            }
            entries
        });
        self.open_popover("code-menu", position, Corner::TopLeft, 200., None, build, window, cx);
    }

    /// Copy a link to `path` on its remote, saying how it went by the pointer.
    fn copy_link(&mut self, path: &Path, lines: Option<(usize, usize)>, permalink: bool, point: Point<Pixels>, cx: &mut Context<Self>) {
        match crate::git::remote_link(path, lines, permalink) {
            Ok(url) => {
                cx.write_to_clipboard(ClipboardItem::new_string(url));
                self.notice(if permalink { "Permalink copied" } else { "Link copied" }, point, cx);
            }
            Err(why) => self.notice(why, point, cx),
        }
    }

    pub(super) fn title(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let project = self.current_project();
        let dashboard = self.dashboard_open();
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
                    .text_size(px(crate::theme::scaled_ui(13.)))
                    .child(
                        // "Chat" opens the dashboard, and closes it again.
                        row()
                            .id("open-dashboard")
                            .debug_selector(|| "open-dashboard".into())
                            .gap(px(6.))
                            .px(px(5.))
                            .ml(px(-5.))
                            .py(px(1.))
                            .rounded(px(5.))
                            .when(dashboard, |s| s.bg(rgb(colors.hover)))
                            .hover(|s| s.bg(rgb(colors.hover)))
                            .on_click(cx.listener(|s, _, _, cx| s.toggle_dashboard(cx)))
                            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(if dashboard { "Back to threads" } else { "Usage dashboard" }).build(w, cx))
                            .child(icon(Glyph::Sparkle).text_color(rgb(colors.blue)))
                            .child(div().font_weight(FontWeight::MEDIUM).child("Chat")),
                    )
                    .child(div().text_color(rgb(colors.quiet)).child("/"))
                    .when(dashboard, |s| s.child(div().text_color(rgb(colors.muted)).child("Overview")))
                    .when(!dashboard, |s| s.child(div().text_color(rgb(colors.muted)).child(project.name.clone())))
                    .when_some(self.current_thread().filter(|_| !dashboard), |s, t| {
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
            // Projects untouched for a day fade to grey. One with no times at
            // all, like one just added, keeps its colour.
            let last = p.threads.iter().filter_map(|t| t.last_active()).max();
            let stale = last.is_some_and(|at| crate::model::now_ms() - at > STALE_MS);
            let hue = match p.path.as_deref() {
                Some(path) if !stale => project_hue(colors, path),
                _ => colors.muted,
            };
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
            let unread = p.threads.iter().filter(|t| t.unread).count();
            let tip = match &p.path {
                Some(path) if p.folders.is_empty() => format!("{} · {}", p.name, tilde(path)),
                Some(path) => format!("{} · {} and {} more", p.name, tilde(path), p.folders.len()),
                None => "Chats · not tied to a project".to_string(),
            };
            let tip = match unread {
                0 => tip,
                n => format!("{tip} · {n} unread"),
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
                            .relative()
                            .group("project")
                            .size(px(34.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .map(|s| match p.path {
                                Some(_) => s.child(face(
                                    &p.name,
                                    hue,
                                    34.,
                                    if active { 0.24 } else { 0.14 },
                                    (!active).then_some(0.24),
                                )),
                                None => s
                                    .rounded_full()
                                    .border_2()
                                    .border_color(if active { Hsla::from(rgb(colors.blue)) } else { transparent_black() })
                                    .bg(Hsla::from(rgb(hue)).opacity(if active { 0.24 } else { 0.14 }))
                                    .text_color(rgb(hue))
                                    .when(!active, |s| s.hover(|s| s.bg(Hsla::from(rgb(hue)).opacity(0.24))))
                                    .child(icon(Glyph::Message).size(px(15.))),
                            })
                            // One mark: finished threads you haven't looked at, counted on
                            // a pill, orange and breathing while an agent still works there;
                            // otherwise an orange breathing dot for one at work, or a blue
                            // one for one waiting on you.
                            .map(|s| {
                                let at = |d: Div| d.absolute().top(px(if p.path.is_some() { 0. } else { -3. })).right(px(-4.));
                                let count = |bg: u32| {
                                    div()
                                        .min_w(px(14.))
                                        .h(px(14.))
                                        .px(px(3.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(rgb(bg))
                                        .text_color(rgb(colors.primary_text))
                                        .text_size(px(crate::theme::scaled_ui(9.)))
                                        .font_weight(FontWeight::BOLD)
                                        .child(if unread > 9 { "9+".to_string() } else { unread.to_string() })
                                };
                                let dot = |bg: u32| div().size(px(10.)).rounded_full().border_2().border_color(rgb(colors.chrome)).bg(rgb(bg));
                                match (unread > 0, running, review) {
                                    (true, true, _) => s.child(at(pulse(("project-pulse", i), colors.orange, count(colors.orange)))),
                                    (true, false, _) => s.child(at(count(colors.blue))),
                                    // A plain dot: the halo needs no ring to stand apart.
                                    (false, true, _) => s.child(at(pulse(("project-pulse", i), colors.orange, div().size(px(8.)).rounded_full().bg(rgb(colors.orange)))).mt(px(1.)).mr(px(1.))),
                                    (false, false, true) => s.child(at(dot(colors.blue))),
                                    _ => s,
                                }
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
                        let dragged = DraggedProject { index: i, name: p.name.clone(), hue };
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
                col()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(2.))
                    .pb(px(6.))
                    .child(self.remote_button(cx))
                    .child(
                        icon_button("open-prefs", IconName::Settings, "Settings · ⌘ ,")
                            .on_click(cx.listener(|s, _, w, cx| s.open_prefs(&OpenPreferences, w, cx))),
                    ),
            )
            .into_any_element()
    }

    /// The thread at the top of the list as it's sorted.
    pub(super) fn first_listed(&self) -> usize {
        thread_order(&self.current_project().threads, self.settings.thread_sort, chrono::Local::now()).first().map_or(0, |(_, i)| *i)
    }

    /// The button that picks what orders the thread list.
    fn thread_sort_menu(&self, colors: Palette, cx: &mut Context<Self>) -> Div {
        let tip = match self.settings.thread_sort {
            ThreadSort::LastActive => "Sorted by last active",
            ThreadSort::Created => "Sorted by created",
        };
        let trigger = row()
            .id("thread-sort-button")
            .size(px(24.))
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .text_color(rgb(colors.muted))
            .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
            .child(icon(IconName::SortDescending))
            .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx));
        self.popover_trigger("thread-sort", trigger, Corner::TopLeft, 180., None, |s, weak, _| {
            let item = |label: &'static str, sort: ThreadSort| {
                let weak = weak.clone();
                crate::menu::Entry::Item(crate::menu::Item::new(label).checked(s.settings.thread_sort == sort).on_click(move |_, cx| {
                    let _ = weak.update(cx, |s, cx| {
                        s.settings.thread_sort = sort;
                        let _ = s.settings.save();
                        cx.notify();
                    });
                }))
            };
            vec![
                crate::menu::Entry::Label("Sort threads by".into()),
                item("Last active", ThreadSort::LastActive),
                item("Created", ThreadSort::Created),
            ]
        }, cx)
    }

    pub(super) fn threads_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let project = self.current_project();
        let query = self.thread_search.read(cx).value().trim().to_string();
        let q = query.to_lowercase();
        // Content hits for this query, once the background search is back.
        let hits = self.thread_hits.as_ref().filter(|r| r.project == self.project && r.query == query);
        let searching = !q.is_empty() && hits.is_none();
        let mut list = col().py_1();
        let mut shown = 0;
        let mut section = None;
        for (day, i) in thread_order(&project.threads, self.settings.thread_sort, chrono::Local::now()) {
            let t = &project.threads[i];
            let hit = hits.and_then(|h| h.get(t.id));
            if !q.is_empty() && hit.is_none() && search::find(&t.title, &q).is_empty() {
                continue;
            }
            shown += 1;
            if section != Some(day) {
                section = Some(day);
                list = list.child(
                    div()
                        .px_3()
                        .pt(px(if shown == 1 { 6. } else { 14. }))
                        .pb(px(4.))
                        .text_size(px(crate::theme::scaled_ui(11.)))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(colors.quiet))
                        .child(day.title()),
                );
            }
            let active = self.thread == i;
            // Threads left alone for a while fade back, unless showing or asking
            // for attention. One with no times to go on stays as it is.
            let stale = !active
                && !t.unread
                && !t.state.busy()
                && t.last_active().is_some_and(|at| crate::model::now_ms() - at > STALE_THREAD_MS);
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
                    .when(stale, |s| s.opacity(0.5))
                    .child(row().w(px(14.)).flex_shrink_0().justify_center().child(state_mark(colors, t.state, t.unread)))
                    .child(one_line(t.title.clone(), 18.).flex_1().min_w_0())
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_thread(i, cx))),
            );
            let Some(hit) = hit else { continue };
            // The paragraphs that matched, under the thread they're from.
            let mut group = col().mx_1().mb_1().pl(px(30.)).pr_2().gap(px(2.));
            for (n, snippet) in hit.snippets.iter().enumerate() {
                let marks = snippet
                    .marks
                    .iter()
                    .map(|m| (m.clone(), HighlightStyle { color: Some(rgb(colors.text).into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }))
                    .collect::<Vec<_>>();
                group = group.child(
                    div()
                        .id(SharedString::from(format!("hit-{}-{n}", t.id)))
                        .px_1()
                        .py(px(2.))
                        .rounded(px(4.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(rgb(colors.muted))
                        .line_clamp(3)
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(colors.hover)))
                        .child(StyledText::new(snippet.text.clone()).with_highlights(marks))
                        .on_click(cx.listener({
                            let (block, nth) = (snippet.block, snippet.nth);
                            move |s, _, _, cx| s.reveal_hit(i, block, nth, cx)
                        })),
                );
            }
            if hit.more > 0 {
                group = group.child(
                    div()
                        .px_1()
                        .text_size(px(11.))
                        .text_color(rgb(colors.quiet))
                        .child(format!("{} more match{}", hit.more, if hit.more == 1 { "" } else { "es" })),
                );
            }
            list = list.child(group);
        }
        if searching {
            list = list.child(
                row().h(px(30.)).mx_1().px_2().gap_2().text_color(rgb(colors.muted)).child(row().w(px(14.)).justify_center().child(spinner(colors))).child("Searching…"),
            );
        } else if shown == 0 {
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
                    .child(self.thread_sort_menu(colors, cx))
                    .child(
                        icon_button("new-thread", Glyph::Compose, "New thread · ⌘ N")
                            .on_click(cx.listener(|s, _, w, cx| s.new_thread(&NewThread, w, cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(div().id("thread-list").size_full().overflow_y_scroll().track_scroll(&self.thread_scroll).child(list))
                    .vertical_scrollbar(&self.thread_scroll),
            )
            .into_any_element()
    }

    pub(super) fn tabs_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        // The chat stays put at the start; the other tabs scroll behind it.
        let mut fixed_tabs = Vec::new();
        // Fixed heights, not `h_full`: percentages through the scroll strip can
        // resolve against an indefinite height and collapse the tabs to their content.
        let mut tabs = row().id("tabs-scroll").h(px(BAR)).overflow_x_scroll().track_scroll(&self.tabs_scroll);
        // Bring the active tab into view when it changes. Children of the strip
        // are the unpinned tabs, so count those that come before it.
        if let Some(active) = self.tabs.get(self.active) {
            if self.tab_in_view.replace(Some(active.id)) != Some(active.id) && !active.fixed() {
                let index = self.tabs[..self.active].iter().filter(|t| !t.fixed()).count();
                self.tabs_scroll.scroll_to_item(index);
            }
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let active = self.active == i;
            let pinned = t.fixed();
            let stale = self.stale(&t.kind).is_some();
            let group = SharedString::from(format!("tab-{}", t.id));
            let (name, detail) = match &t.kind {
                TabKind::Chat => ("Chat".to_string(), String::new()),
                TabKind::File(path) | TabKind::Diff(path) | TabKind::GitDiff(path, _) => {
                    let relative = self.current_project().relative(path);
                    // Outside the project (a library's source), from home.
                    let relative = if relative.is_absolute() { crate::model::tilde(&relative) } else { relative.to_string_lossy().into_owned() };
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
                TabKind::Terminal(cwd) => {
                    // With several folders, which one it's in tells the terminals apart.
                    let many = self.current_project().roots().len() > 1;
                    ("Terminal".to_string(), if many { crate::model::folder_name(cwd) } else { String::new() })
                }
                TabKind::ServerLog(name, root) => {
                    let folder = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (format!("{name} logs"), folder)
                }
            };
            // What the tab holds in full: the label is cut short, the tooltip isn't.
            let tip = {
                let full = |path: &Path| crate::model::tilde(path);
                let what = match &t.kind {
                    TabKind::Chat => self.current_thread().map_or_else(|| "Chat".to_string(), |thread| thread.title.to_string()),
                    TabKind::File(path) => full(path),
                    TabKind::Diff(path) => format!("This thread's changes · {}", full(path)),
                    TabKind::GitDiff(path, staged) => format!("{} changes · {}", if *staged { "Staged" } else { "Unstaged" }, full(path)),
                    TabKind::Terminal(cwd) => format!("Terminal · {}", full(cwd)),
                    TabKind::ServerLog(name, root) => format!("{name} logs · {}", full(root)),
                };
                SharedString::from(if stale { format!("{what}\nOut of date — open it to see why") } else { what })
            };
            let tab = row()
                    .id(("tab", t.id))
                    .group(group.clone())
                    .h(px(BAR))
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
                        // A stale tab's name is struck through (its text style reaches the label's runs).
                        line_with_detail(name, detail, 18.).w(width).flex_shrink_0().when(stale, |s| s.line_through())
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(if pinned {
                        // The chat (or a pinned tab) can't be closed: a pin sits where the close button would.
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
                    .on_mouse_down(MouseButton::Right, cx.listener({
                        let id = t.id;
                        move |s, e: &MouseDownEvent, window, cx| s.tab_menu(id, e.position, window, cx)
                    }))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.active = i;
                        s.focus_terminal(cx);
                        cx.notify();
                    }));
            if pinned {
                fixed_tabs.push(tab.flex_shrink_0());
            } else {
                tabs = tabs.child(tab);
            }
        }
        row()
            .h(px(BAR))
            .flex_shrink_0()
            .bg(rgb(colors.chrome))
            .children(fixed_tabs)
            .child(div().min_w_0().h(px(BAR)).child(tabs))
            .child(div().flex_1().h_full().border_b_1().border_color(rgb(colors.border)))
            .child(
                row()
                    .h_full()
                    .flex_shrink_0()
                    .px_1()
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(self.anchor(
                        TERMINAL_BUTTON,
                        icon_button(TERMINAL_BUTTON, Glyph::Terminal, "Terminal · ⌃ `").on_click(cx.listener(|s, _, window, cx| {
                            // Mouse-down outside already closed the folder picker: the click only closes it.
                            if s.dismissed.take().is_some_and(|(a, at)| a == TERMINAL_BUTTON && at.elapsed() < Duration::from_millis(400)) {
                                return;
                            }
                            s.open_terminal(window, cx)
                        })),
                    ))
                    .child(icon_button("new-tab", IconName::Plus, "Open a file").on_click(cx.listener(|s, _, _, cx| {
                        s.side = Side::Files;
                        s.side_open = true;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }

    /// Why a tab no longer shows anything current — its file is gone, or the
    /// change it shows has been undone or committed — if it doesn't.
    pub(super) fn stale(&self, kind: &TabKind) -> Option<&'static str> {
        match kind {
            TabKind::File(path) => (!path.exists()).then_some("This file has been deleted. Showing it as it was when opened."),
            TabKind::Diff(path) => {
                let changed = self.current_thread().is_some_and(|t| t.changes().iter().any(|c| &c.path == path));
                (!changed).then_some("This thread hasn't changed this file.")
            }
            TabKind::GitDiff(path, staged) => {
                // Only once the file's repository has been read; until then it's unknown.
                let snapshot = self.git_snapshots.values().filter_map(|s| s.as_ref().ok()).find(|s| path.starts_with(&s.root))?;
                let relative = path.strip_prefix(&snapshot.root).ok()?;
                let current = snapshot
                    .entries
                    .iter()
                    .any(|e| e.path == relative && if *staged { e.staged() } else { e.unstaged() });
                (!current).then_some(if *staged {
                    "Nothing is staged in this file anymore. This diff is out of date."
                } else {
                    "Nothing is unstaged in this file anymore. This diff is out of date."
                })
            }
            TabKind::Chat | TabKind::Terminal(_) | TabKind::ServerLog(..) => None,
        }
    }

    pub(super) fn content(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let kind = &self.tabs[self.active].kind;
        let body = match kind {
            TabKind::Chat => self.chat_view(window, cx),
            TabKind::File(path) => self.file_view(path.clone(), window, cx),
            // With no change to show, the page itself already says why.
            TabKind::Diff(path) => return self.diff_view(path.clone(), window, cx),
            TabKind::GitDiff(path, staged) => self.git_diff_view(path, *staged, window, cx),
            TabKind::Terminal(cwd) => return self.terminal_view(cwd, cx),
            TabKind::ServerLog(name, root) => return self.server_log_view(name, root, cx),
        };
        // A Git diff that failed to load already says why in its place.
        let explained = match kind {
            TabKind::GitDiff(path, staged) => matches!(self.git_diffs.get(&(path.clone(), *staged)), Some(Err(_))),
            _ => false,
        };
        let Some(reason) = self.stale(kind).filter(|_| !explained) else { return body };
        let colors = palette(cx);
        col()
            .size_full()
            .child(
                row()
                    .flex_shrink_0()
                    .px_3()
                    .py(px(6.))
                    .gap(px(6.))
                    .bg(rgb(colors.amber_tint))
                    .border_b_1()
                    .border_color(rgb(colors.row_border))
                    .text_size(px(crate::theme::scaled_ui(12.)))
                    .child(icon(IconName::TriangleAlert).size(px(13.)).text_color(rgb(colors.amber)))
                    .child(div().text_color(rgb(colors.text)).child(reason)),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
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
            .text_size(px(crate::theme::scaled_ui(12.)))
            .text_color(rgb(colors.muted))
            .child(
                toggle(colors, "toggle-threads", IconName::PanelLeft, "Toggle threads · ⌘ B", self.threads_open)
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.threads_open = !s.threads_open;
                        cx.notify();
                    })),
            )
            .when_some(self.mission_button(cx), |s, button| s.child(button))
            .when_some(self.servers_button(cx), |s, button| s.child(button))
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
            .when_some(self.limit_item(cx), |s, item| s.child(item).child(divider(colors)))
            .when_some(self.current_thread(), |s, t| {
                s.child(match self.setting(Role::Model) {
                    Some(model) => format!("{} · {}", self.agent_name(t), model.current_name()),
                    None => self.agent_name(t).to_string(),
                })
            })
            .into_any_element()
    }

    /// The current agent's shortest usage limit window (Claude's five-hour one)
    /// in the status bar, or whichever has run out.
    fn limit_item(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::agent::LimitStatus;
        let colors = palette(cx);
        let thread = self.current_thread()?;
        let now = chrono::Utc::now().timestamp();
        // A window that has reset no longer says anything.
        let live = self.limits.get(&thread.agent)?.values().filter(|l| l.resets_at.is_none_or(|at| at > now));
        let limit = live
            .min_by_key(|l| (l.status != LimitStatus::Rejected, l.rank()))?
            .clone();
        let resets = limit.resets_at.and_then(|at| chrono::DateTime::from_timestamp(at, 0)).map(|at| {
            let at = at.with_timezone(&chrono::Local);
            if at.date_naive() == chrono::Local::now().date_naive() {
                at.format("%-I:%M %p").to_string()
            } else {
                at.format("%a %-I:%M %p").to_string()
            }
        });
        let used = limit.used.map(|u| format!("{}%", u.round() as u32));
        let (tint, mut text) = match limit.status {
            LimitStatus::Rejected => (colors.red, format!("{} limit reached", limit.short_name())),
            LimitStatus::Warning => (colors.amber, format!("{} {}", limit.short_name(), used.clone().unwrap_or_else(|| "near limit".into()))),
            LimitStatus::Allowed => (colors.muted, format!("{} {}", limit.short_name(), used.clone().unwrap_or_else(|| "OK".into()))),
        };
        if let Some(resets) = &resets {
            text.push_str(&format!(" · resets {resets}"));
        }
        let mut tip = limit.long_name();
        match (&used, limit.status) {
            (_, LimitStatus::Rejected) => tip.push_str(" reached"),
            (Some(used), _) => tip.push_str(&format!(" · {used} used")),
            (None, _) => {}
        }
        if let Some(resets) = &resets {
            tip.push_str(&format!(" · resets {resets}"));
        }
        let tip: SharedString = tip.into();
        let open = self.usage_open();
        Some(
            self.anchor(
                "usage",
                row()
                    .id("rate-limit")
                    .debug_selector(|| "rate-limit".into())
                    .gap(px(4.))
                    .px(px(6.))
                    .mx(px(-6.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_color(rgb(tint))
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .when(open, |s| s.bg(rgb(colors.hover)))
                    .when(limit.status != LimitStatus::Allowed, |s| s.child(icon(IconName::TriangleAlert).size(px(12.)).text_color(rgb(tint))))
                    .child(text)
                    .when(!open, |s| s.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx)))
                    .on_click(cx.listener(|s, _, _, cx| s.toggle_usage(cx))),
            )
            .into_any_element(),
        )
    }
}

/// The apps `path` can be opened in, then Reveal in Finder.
pub(super) fn open_with_entries(editors: &crate::editors::Editors, path: &Path, dir: bool) -> Vec<crate::menu::Entry> {
    let mut entries = editor_entries(editors, path, dir);
    entries.push(crate::menu::Entry::Separator);
    entries.push(reveal_item(editors, path));
    entries
}

/// The apps `path` can be opened in, each with its icon. A folder is offered
/// only to editors that open folders.
fn editor_entries(editors: &crate::editors::Editors, path: &Path, dir: bool) -> Vec<crate::menu::Entry> {
    use crate::menu::{Entry, Item};
    let mut entries: Vec<Entry> = editors
        .list
        .iter()
        .filter(|e| !dir || e.folders)
        .map(|editor| {
            let (app, target) = (editor.app.clone(), path.to_path_buf());
            let item = Item::new(editor.name.clone()).on_click(move |_, _| crate::editors::open(&app, &target));
            Entry::Item(match &editor.icon {
                Some(png) => item.image(png.clone()),
                None => item.icon(IconName::ExternalLink),
            })
        })
        .collect();
    if entries.is_empty() {
        entries.push(Entry::Item(Item::new("No editors found").disabled(true)));
    }
    entries
}

/// "Open with", its apps in a submenu.
pub(super) fn open_with_item(editors: &Rc<crate::editors::Editors>, path: &Path, dir: bool) -> crate::menu::Entry {
    let (editors, path) = (editors.clone(), path.to_path_buf());
    let sub: crate::menu::Builder = Rc::new(move |_: &App| editor_entries(&editors, &path, dir));
    crate::menu::Entry::Item(crate::menu::Item::new("Open with").icon(IconName::ExternalLink).submenu(sub))
}

/// "Copy link" (to the branch HEAD tracks) and "Copy permalink" (to HEAD's
/// commit), for a file in a Git repository; nothing otherwise.
fn link_entries(weak: &WeakEntity<Workspace>, path: &Path, lines: Option<(usize, usize)>, point: Point<Pixels>) -> Vec<crate::menu::Entry> {
    if !path.ancestors().skip(1).any(|dir| dir.join(".git").exists()) {
        return Vec::new();
    }
    let item = |label: &'static str, permalink: bool| {
        let (weak, path) = (weak.clone(), path.to_path_buf());
        let glyph = if permalink { crate::icons::Glyph::Commit } else { crate::icons::Glyph::Link };
        crate::menu::Entry::Item(crate::menu::Item::new(label).icon(glyph).on_click(move |_, cx| {
            let _ = weak.update(cx, |s, cx| s.copy_link(&path, lines, permalink, point, cx));
        }))
    };
    let (link, permalink) = match lines {
        Some((a, b)) if a == b => ("Copy link to line", "Copy permalink to line"),
        Some(_) => ("Copy link to lines", "Copy permalink to lines"),
        None => ("Copy link", "Copy permalink"),
    };
    vec![crate::menu::Entry::Separator, item(link, false), item(permalink, true)]
}

fn reveal_item(editors: &crate::editors::Editors, path: &Path) -> crate::menu::Entry {
    let target = path.to_path_buf();
    let item = crate::menu::Item::new("Reveal in Finder").on_click(move |_, cx| cx.reveal_path(&target));
    crate::menu::Entry::Item(match &editors.finder {
        Some(png) => item.image(png.clone()),
        None => item.icon(IconName::FolderOpen),
    })
}

#[cfg(test)]
mod thread_order_tests {
    use super::{Day, thread_order};
    use crate::model::{Block, Message, Thread};
    use crate::settings::ThreadSort;
    use chrono::TimeZone;

    fn thread(id: usize, sent: &[i64]) -> Thread {
        let mut t = Thread::new(id, 0);
        for at in sent {
            t.blocks.push(Block::User(Message { text: "hi".into(), at: Some(*at) }));
            t.mark_active(*at);
        }
        t
    }

    #[test]
    fn sorts_newest_first_and_sections_by_day() {
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 21, 15, 0, 0).unwrap();
        let at = |d: u32, h: u32| chrono::Local.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap().timestamp_millis();
        let threads = vec![
            thread(1, &[at(10, 9), at(21, 8)]), // began long ago, active this morning
            thread(2, &[at(20, 23)]),           // yesterday, late
            thread(3, &[at(21, 10)]),           // began this morning, after 1 was active
            Thread::new(4, 0),                  // empty: just made
            {
                // From before times were kept.
                let mut t = Thread::new(5, 0);
                t.blocks.push(Block::User(Message { text: "old".into(), at: None }));
                t
            },
        ];
        let ids = |sort| thread_order(&threads, sort, now).into_iter().map(|(d, i)| (d, threads[i].id)).collect::<Vec<_>>();
        assert_eq!(
            ids(ThreadSort::LastActive),
            [(Day::Today, 4), (Day::Today, 3), (Day::Today, 1), (Day::Yesterday, 2), (Day::Older, 5)]
        );
        assert_eq!(
            ids(ThreadSort::Created),
            [(Day::Today, 4), (Day::Today, 3), (Day::Yesterday, 2), (Day::Older, 1), (Day::Older, 5)]
        );
    }
}
