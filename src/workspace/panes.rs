//! What opens beside the chat: a file, a diff, or a terminal.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::{ActiveTheme, Icon, scroll::ScrollableElement};

const LINE: f32 = 20.;
/// Unchanged lines kept around each change in a diff.
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Hunk,
    Context,
    Add,
    Del,
}

struct DiffLine {
    kind: LineKind,
    old: Option<usize>,
    new: Option<usize>,
    text: String,
}

/// The pane whose lines are selected.
#[derive(Clone, PartialEq)]
pub(super) enum LineSource {
    File(PathBuf),
    /// A file's change in the current thread.
    Diff(PathBuf),
    /// A file's change from the Git panel, staged or not.
    GitDiff(PathBuf, bool),
}

/// Whole lines selected in a file or diff pane, by row: a file's line index, or
/// a diff's row in `diff_lines`.
pub(super) struct LineSelection {
    pub source: LineSource,
    anchor: usize,
    head: usize,
    dragging: bool,
}
impl LineSelection {
    fn rows(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.min(self.head)..=self.anchor.max(self.head)
    }
}

/// Width of the column the line-selection controls sit in.
const GUTTER: f32 = 22.;

/// How a code view lets lines be picked: who to tell, which pane this is, and
/// the rows already picked.
type Select<'a> = (&'a WeakEntity<Workspace>, &'a LineSource, Option<std::ops::RangeInclusive<usize>>);

/// A code row, led by a gutter for picking lines: a `+` shows on hover; a click
/// picks the line, shift-click extends, and a drag from the gutter extends over
/// the rows it passes. Picked lines draw as one pill down the gutter with the
/// `+` at its top, which adds them to the chat.
fn code_row(colors: Palette, select: Option<&Select>, n: usize) -> Div {
    // Full width, so a flashed or selected line is tinted to the pane's edge.
    let line = row().w_full().h(px(LINE)).group("code-line");
    let Some((this, source, picked)) = select else {
        return line;
    };
    let span = picked.as_ref().filter(|rows| rows.contains(&n)).map(|rows| (*rows.start(), *rows.end()));
    let plus = || icon(gpui_component::IconName::Plus).size(px(11.)).text_color(rgb(colors.primary_text));
    let mark = match span {
        Some((a, b)) if a != b => {
            let (first, last) = (n == a, n == b);
            div().w(px(16.)).h_full().flex().flex_col().child(
                div()
                    .flex_1()
                    .w_full()
                    .when(first, |s| s.mt(px((LINE - 16.) / 2.)).rounded_t(px(8.)))
                    .when(last, |s| s.mb(px((LINE - 16.) / 2.)).rounded_b(px(8.)))
                    .bg(rgb(colors.blue))
                    .when(first, |s| s.child(div().h(px(16.)).flex().items_center().justify_center().child(plus()))),
            )
        }
        _ => div()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(px(4.))
            .bg(rgb(colors.blue))
            .when(span.is_none(), |s| s.opacity(0.).group_hover("code-line", |s| s.opacity(1.)))
            .child(plus()),
    };
    let adds = span.is_some_and(|(a, _)| a == n);
    let (down, moved, up) = ((*this).clone(), (*this).clone(), (*this).clone());
    let (press_source, drag_source) = ((*source).clone(), (*source).clone());
    let gutter = div()
        .id(("code-gutter", n))
        .w(px(GUTTER))
        .h_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .child(mark)
        .when(adds, |s| s.tooltip(|w, cx| gpui_component::tooltip::Tooltip::new("Add to chat · ⌘ L").build(w, cx)))
        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
            down.update(cx, |s, cx| s.press_gutter(press_source.clone(), n, e.modifiers.shift, window, cx)).ok();
        });
    line.child(gutter)
        .on_mouse_move(move |e: &MouseMoveEvent, _, cx| {
            if e.pressed_button == Some(MouseButton::Left) {
                moved.update(cx, |s, cx| s.drag_line(&drag_source, n, cx)).ok();
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            up.update(cx, |s, _| {
                if let Some(lines) = &mut s.line_selection {
                    lines.dragging = false;
                }
            })
            .ok();
        })
}

/// A diff's rows as a unified diff: hunk headers and `+`/`-`/` ` lines.
fn diff_text(lines: &[DiffLine]) -> String {
    lines
        .iter()
        .map(|l| match l.kind {
            LineKind::Hunk => l.text.clone(),
            LineKind::Add => format!("+{}", l.text),
            LineKind::Del => format!("-{}", l.text),
            LineKind::Context => format!(" {}", l.text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line numbers `lines` cover: new-side ones, or old-side when only removals.
fn diff_span(lines: &[DiffLine]) -> Option<(usize, usize)> {
    let span = |nums: Vec<usize>| Some((*nums.iter().min()?, *nums.iter().max()?));
    span(lines.iter().filter_map(|l| l.new).collect()).or_else(|| span(lines.iter().filter_map(|l| l.old).collect()))
}

/// A unified diff of two texts, hunk by hunk.
fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    use similar::ChangeTag;
    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for group in diff.grouped_ops(CONTEXT) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let (old_range, new_range) = (first.old_range().start..last.old_range().end, first.new_range().start..last.new_range().end);
        lines.push(DiffLine {
            kind: LineKind::Hunk,
            old: None,
            new: None,
            text: format!("@@ -{},{} +{},{} @@", old_range.start + 1, old_range.len(), new_range.start + 1, new_range.len()),
        });
        for change in group.iter().flat_map(|op| diff.iter_changes(op)) {
            lines.push(DiffLine {
                kind: match change.tag() {
                    ChangeTag::Insert => LineKind::Add,
                    ChangeTag::Delete => LineKind::Del,
                    ChangeTag::Equal => LineKind::Context,
                },
                old: change.old_index().map(|i| i + 1),
                new: change.new_index().map(|i| i + 1),
                text: syntax::expand_tabs(change.value().trim_end_matches(['\n', '\r'])),
            });
        }
    }
    lines
}

/// Where a pane's content came from: a path, and whatever belongs at the far end.
fn pane_header(colors: Palette, glyph: impl Into<Icon>, path: &str, trailing: impl IntoElement) -> Div {
    let (name, dir) = split_path(path);
    row()
        .h(px(30.))
        .flex_shrink_0()
        .px_3()
        .gap(px(6.))
        .border_b_1()
        .border_color(rgb(colors.row_border))
        .font_family(mono())
        .text_size(px(crate::theme::mono_size()))
        .child(icon(glyph).size(px(13.)).text_color(rgb(colors.muted)))
        .child(
            row()
                .min_w_0()
                .when(!dir.is_empty(), |s| s.child(div().text_color(rgb(colors.muted)).child(format!("{dir}/"))))
                .child(name.to_string()),
        )
        .child(div().flex_1())
        .child(trailing)
}

/// A file's change drawn as Zed draws one: hunks of the new file with the removed
/// lines among them, a coloured bar down the gutter beside each change, line
/// numbers, syntax colours, and a quiet gap between hunks instead of `@@` lines.
/// Returns the rows, the longest line in characters, and the gutter's width.
fn diff_body(
    colors: Palette,
    name: &str,
    old: &str,
    new: &str,
    offset: Pixels,
    select: Option<Select>,
    window: &Window,
) -> Option<(Div, usize, f32)> {
    let (before, after) = (syntax::cached(name, old), syntax::cached(name, new));
    let lines = diff_lines(old, new);
    if lines.is_empty() {
        return None;
    }
    let longest = lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
    // Each run of changed lines gets one bar colour: green for only added,
    // red for only removed, amber when lines were replaced.
    let mut bars = vec![None; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if matches!(lines[i].kind, LineKind::Add | LineKind::Del) {
            let start = i;
            while i < lines.len() && matches!(lines[i].kind, LineKind::Add | LineKind::Del) {
                i += 1;
            }
            let run = &lines[start..i];
            let adds = run.iter().any(|l| l.kind == LineKind::Add);
            let dels = run.iter().any(|l| l.kind == LineKind::Del);
            let hue = match (adds, dels) {
                (true, true) => colors.amber,
                (true, false) => colors.green,
                _ => colors.red,
            };
            bars[start..i].fill(Some(hue));
        } else {
            i += 1;
        }
    }
    let (unit, levels) = syntax::indent_guides(&lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>());
    let guides = Guides::new(colors, window, unit);
    let width = lines.iter().filter_map(|l| l.new.or(l.old)).max().unwrap_or(1).to_string().len() as f32 * 8. + 20.;
    let mut rows = col().w_full().pb_4().font_family(mono()).text_size(px(crate::theme::mono_size()));
    for (n, line) in lines.into_iter().enumerate() {
        if line.kind == LineKind::Hunk {
            if n > 0 {
                rows = rows.child(div().h(px(14.)).bg(rgb(colors.chrome)).border_y_1().border_color(rgb(colors.row_border)));
            }
            continue;
        }
        let side = match line.kind {
            LineKind::Del => before.as_ref().zip(line.old),
            _ => after.as_ref().zip(line.new),
        };
        let marks = side
            .and_then(|(styles, n)| styles.line(colors.dark, n - 1))
            // A line that doesn't match its source can't take that source's ranges.
            .filter(|marks| marks.iter().all(|(r, _)| r.end <= line.text.len() && line.text.is_char_boundary(r.end)))
            .cloned()
            .unwrap_or_default();
        let selected = select.as_ref().and_then(|(_, _, rows)| rows.as_ref()).is_some_and(|rows| rows.contains(&n));
        let bg = match line.kind {
            _ if selected => Some(colors.selected),
            LineKind::Add => Some(colors.green_tint),
            LineKind::Del => Some(colors.red_tint),
            _ => None,
        };
        let cells = code_row(colors, select.as_ref(), n)
            .when_some(bg, |s, bg| s.bg(rgb(bg)))
            .child(div().w(px(3.)).h_full().flex_shrink_0().when_some(bars[n], |s, hue| s.bg(rgb(hue))))
            // Removed lines have no place in the new file, so no number.
            .child(number(colors, if line.kind == LineKind::Del { None } else { line.new }, width).pr_3())
            .child(code_cell(offset, StyledText::new(line.text).with_highlights(marks), guides.at(levels[n])));
        rows = rows.child(cells);
    }
    let gutter = if select.is_some() { GUTTER } else { 0. };
    Some((rows, longest, gutter + 3. + width + 12.))
}

/// Vertical indent guides for one code row: how many, the gap between them
/// (an indent step in pixels), and their colour.
#[derive(Clone, Copy)]
struct Guides {
    count: u16,
    step: Pixels,
    color: Hsla,
}

impl Guides {
    /// A code view's guide spacing and colour; each row then sets its own count.
    fn new(colors: Palette, window: &Window, unit: usize) -> Self {
        Self { count: 0, step: glyph_width(window) * unit as f32, color: rgb(colors.row_border).into() }
    }

    fn at(self, count: u16) -> Self {
        // With guides turned off, every row has none.
        Self { count: if syntax::show_guides() { count } else { 0 }, ..self }
    }
}

/// The width of one character in the code font.
fn glyph_width(window: &Window) -> Pixels {
    text_width_in(window, "0000000000", crate::theme::mono_size(), mono()) / 10.
}

/// A line's code, shifted left by how far the view has scrolled sideways and
/// clipped at the gutter, which stays put. Indent guides scroll with the code.
fn code_cell(offset: Pixels, text: impl IntoElement, guides: Guides) -> Div {
    let lines = (0..guides.count).map(|k| {
        div().absolute().top_0().bottom_0().left(guides.step * k as f32).w(px(1.)).bg(guides.color)
    });
    div().flex().flex_1().min_w_0().h_full().items_center().overflow_hidden().child(
        div()
            .relative()
            .h_full()
            .flex()
            .items_center()
            .flex_shrink_0()
            .whitespace_nowrap()
            .pr_4()
            .ml(-offset)
            .children(lines)
            .child(text),
    )
}

/// How far a code view has scrolled sideways, how wide its longest line is, and
/// how much room its code has: enough to keep the scroll in bounds.
#[derive(Clone, Default)]
pub(super) struct Sideways {
    offset: Pixels,
    content: Pixels,
    viewport: Pixels,
    /// Up-and-down scroll of a view that has no handle of its own, for its scrollbar.
    scroll: ScrollHandle,
    /// The axis the current trackpad swipe is held to.
    axis: Option<Axis>,
    /// Where a drag on the sideways scrollbar began, and the offset then.
    drag: Option<(Pixels, Pixels)>,
    hover: bool,
}

impl Sideways {
    fn max(&self) -> Pixels {
        (self.content - self.viewport).max(px(0.))
    }

    /// The sideways scrollbar's thumb within `track`, when there is anywhere to scroll.
    fn thumb(&self, track: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let max = self.max();
        if max <= px(0.) || self.content <= px(0.) {
            return None;
        }
        let width = (track.size.width * (self.viewport / self.content)).max(px(24.)).min(track.size.width);
        let left = track.left() + (track.size.width - width) * (self.offset / max);
        Some(Bounds::new(point(left, track.top() + px(2.)), size(width, track.size.height - px(4.))))
    }
}

/// Where a code view's sideways scrollbar runs: along the bottom, right of the
/// line numbers and clear of the up-and-down scrollbar.
fn sideways_track(bounds: Bounds<Pixels>, gutter: f32) -> Bounds<Pixels> {
    let left = bounds.left() + px(gutter);
    let right = bounds.right() - px(12.);
    Bounds::new(point(left, bounds.bottom() - px(10.)), size((right - left).max(px(0.)), px(10.)))
}

/// Width of a file view's blame column.
const BLAME: f32 = 190.;

/// Who last changed a file's line and when, written only where a run of lines
/// from one commit begins; hovering shows the commit.
fn blame_cell(colors: Palette, git: Option<&crate::git::FileInfo>, i: usize, now: i64) -> AnyElement {
    let cell = div().w(px(BLAME)).h_full().flex_shrink_0().pl_1().pr_3().flex().items_center().overflow_hidden();
    let Some((git, at)) = git.and_then(|g| Some((g, (*g.blame.get(i)?)?))) else {
        return cell.into_any_element();
    };
    let commit = &git.commits[at];
    let first = i == 0 || git.blame.get(i - 1).copied().flatten() != Some(at);
    let (who, tip) = if commit.committed() {
        let date = chrono::DateTime::from_timestamp(commit.time, 0).map(|d| d.format("%b %-d, %Y").to_string()).unwrap_or_default();
        (
            format!("{} · {}", commit.author, ago(now - commit.time)),
            format!("{} · {} · {}\n{}", &commit.sha[..7.min(commit.sha.len())], commit.author, date, commit.summary),
        )
    } else {
        ("Not committed yet".to_owned(), "Not committed yet".to_owned())
    };
    cell.id(("blame", i))
        .font_family(crate::theme::ui_font())
        .text_size(px(crate::theme::scaled_ui(11.)))
        .text_color(rgb(colors.quiet))
        .when(i > 0 && first, |s| s.border_t_1().border_color(rgb(colors.row_border)))
        .hover(|s| s.text_color(rgb(colors.muted)))
        .when(first, |s| s.child(div().truncate().child(who)))
        .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        .into_any_element()
}

/// A span of seconds as a short "how long ago".
fn ago(seconds: i64) -> String {
    let (minute, hour, day) = (60, 3600, 86400);
    match seconds.max(0) {
        s if s < hour => format!("{} min ago", (s / minute).max(1)),
        s if s < day => format!("{} hr ago", s / hour),
        s if s < 2 * day => "yesterday".to_owned(),
        s if s < 30 * day => format!("{} days ago", s / day),
        s if s < 365 * day => format!("{} mo ago", s / (30 * day)),
        s => format!("{} yr ago", s / (365 * day)),
    }
}

/// The bar beside a file's line numbers marking its change since HEAD: green
/// for added, amber for changed, and a red notch where lines were removed.
fn change_bar(colors: Palette, mark: Option<crate::git::Mark>) -> Div {
    use crate::git::Mark;
    let notch = || div().absolute().left(px(-2.)).w(px(7.)).h(px(2.)).bg(rgb(colors.red));
    div()
        .relative()
        .w(px(3.))
        .h_full()
        .mr(px(10.))
        .flex_shrink_0()
        .when_some(mark, |s, mark| match mark {
            Mark::Added => s.bg(rgb(colors.green)),
            Mark::Modified => s.bg(rgb(colors.amber)),
            Mark::RemovedAbove => s.child(notch().top_0()),
            Mark::RemovedBelow => s.child(notch().bottom_0()),
        })
}

fn number(colors: Palette, n: Option<usize>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .text_right()
        .text_color(rgb(colors.quiet))
        .child(n.map(|n| n.to_string()).unwrap_or_default())
}

impl Workspace {
    fn relative(&self, path: &Path) -> String {
        self.current_project().relative(path).to_string_lossy().into_owned()
    }

    pub(super) fn file_view(&self, path: PathBuf, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let relative = self.relative(&path);
        let file = match self.files.get(&path) {
            Some(Ok(file)) => file.clone(),
            Some(Err(why)) => return empty(colors, Glyph::FileText, relative, why.clone()).into_any_element(),
            None => return empty(colors, Glyph::FileText, relative, "This file is no longer open.").into_any_element(),
        };
        let edited = self.current_thread().is_some_and(|t| t.changes().iter().any(|c| c.path == path));
        let count = file.lines.len();
        let key = SharedString::from(format!("file-{}", path.display()));
        let offset = self.sideways_offset(&key);
        let longest = file.lines.get(file.longest).map_or(0, |l| l.chars().count());
        let source = LineSource::File(path.clone());
        let selected = self.selected_rows(&source);
        let this = cx.weak_entity();
        let flash = self.flash.as_ref().filter(|f| f.path == path).map(|f| (f.rows.clone(), f.strength())).filter(|(_, k)| *k > 0.);
        if flash.is_some() {
            window.request_animation_frame();
        }
        let scroll = self.file_scroll.get(&path).cloned().unwrap_or_default();
        let guides = Guides::new(colors, window, file.guides.0);
        let git = self.file_git.get(&path).cloned();
        let has_blame = git.as_ref().is_some_and(|g| !g.blame.is_empty());
        let blame = has_blame && self.show_blame;
        let now = chrono::Utc::now().timestamp();
        // Only the lines on screen are built, so long files stay cheap.
        let list = uniform_list(key.clone(), count, move |range, _, _| {
            range
                .map(|i| {
                    let line = file.lines[i].clone();
                    let marks = file.highlights.as_ref().and_then(|h| h.line(colors.dark, i)).cloned().unwrap_or_default();
                    code_row(colors, Some(&(&this, &source, selected.clone())), i)
                        .when_some(flash.as_ref().filter(|(rows, _)| rows.contains(&i)), |s, (_, k)| {
                            s.bg(Hsla::from(rgb(colors.amber)).opacity(0.22 * k))
                        })
                        .when(selected.as_ref().is_some_and(|rows| rows.contains(&i)), |s| s.bg(rgb(colors.selected)))
                        .when(blame, |s| s.child(blame_cell(colors, git.as_deref(), i, now)))
                        .child(number(colors, Some(i + 1), 44.).mr_2())
                        .child(change_bar(colors, git.as_ref().and_then(|g| g.marks.get(i).copied().flatten())))
                        .child(
                            code_cell(offset, StyledText::new(line).with_highlights(marks), guides.at(file.guides.1[i]))
                                .text_color(rgb(colors.code)),
                        )
                })
                .collect()
        })
        .track_scroll(scroll.clone())
        .size_full()
        .py_2()
        .font_family(mono())
        .text_size(px(crate::theme::mono_size()));
        col()
            .size_full()
            .child(pane_header(
                colors,
                Glyph::FileCode,
                &relative,
                row()
                    .gap_2()
                    .font_family(crate::theme::ui_font())
                    .when(edited, |s| s.child(pill("Edited in this thread", colors.amber, colors.amber_tint)))
                    .child(label(colors, format!("{count} lines")))
                    .when(has_blame, |s| {
                        s.child(
                            toggle(colors, "file-blame", Glyph::Branch, "Blame", self.show_blame)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.show_blame = !s.show_blame;
                                    cx.notify();
                                })),
                        )
                    }),
            ))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.scroll_sideways(key, longest, GUTTER + if blame { BLAME } else { 0. } + 65., window, list, cx))
                    .vertical_scrollbar(&scroll),
            )
            .into_any_element()
    }

    pub(super) fn sideways_offset(&self, key: &SharedString) -> Pixels {
        self.sideways.borrow().get(key).map_or(px(0.), |s| s.offset)
    }

    /// Wrap a code view whose lines are drawn with `code_cell`: sideways scrolling
    /// over it shifts the code and leaves the line numbers where they are.
    fn scroll_sideways(&self, key: SharedString, longest: usize, gutter: f32, window: &Window, view: impl IntoElement, cx: &mut Context<Self>) -> Div {
        let glyph = glyph_width(window);
        let content = glyph * longest as f32 + px(16.);
        self.sideways.borrow_mut().entry(key.clone()).or_default().content = content;
        let store = self.sideways.clone();
        let view_id = cx.entity_id();
        let theme = cx.theme();
        let (thumb, thumb_hover) = (theme.scrollbar_thumb, theme.scrollbar_thumb_hover);
        div().relative().size_full().child(view).child(
            canvas(
                {
                    let store = store.clone();
                    let key = key.clone();
                    move |bounds, window, _| {
                        let mut all = store.borrow_mut();
                        let s = all.entry(key).or_default();
                        s.viewport = bounds.size.width - px(gutter);
                        s.offset = s.offset.min(s.max());
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    }
                },
                move |bounds, hitbox, window, _| {
                    let track = sideways_track(bounds, gutter);
                    if let Some(bar) = store.borrow().get(&key).and_then(|s| s.thumb(track)) {
                        let lit = store.borrow().get(&key).is_some_and(|s| s.drag.is_some()) || track.contains(&window.mouse_position());
                        window.paint_quad(fill(bar, if lit { thumb_hover } else { thumb }).corner_radii(px(3.)));
                    }

                    // Trackpad swipes are taken whole: the axis the gesture starts
                    // along is the only one it moves, so code doesn't drift
                    // diagonally. Sideways ones are kept from the list beneath.
                    let (s1, k1, h1) = (store.clone(), key.clone(), hitbox.clone());
                    window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture || !h1.should_handle_scroll(window) {
                            return;
                        }
                        let mut all = s1.borrow_mut();
                        let side = all.entry(k1.clone()).or_default();
                        let d = e.delta.pixel_delta(px(LINE));
                        let sideways = match e.delta {
                            // A wheel turns sideways with shift.
                            ScrollDelta::Lines(_) => {
                                side.axis = None;
                                e.modifiers.shift || d.x.abs() > d.y.abs()
                            }
                            ScrollDelta::Pixels(_) => {
                                if matches!(e.touch_phase, TouchPhase::Started) {
                                    side.axis = None;
                                }
                                if side.axis.is_none() && (d.x != px(0.) || d.y != px(0.)) {
                                    side.axis = Some(if d.x.abs() > d.y.abs() { Axis::Horizontal } else { Axis::Vertical });
                                }
                                side.axis == Some(Axis::Horizontal)
                            }
                        };
                        if !sideways {
                            return;
                        }
                        cx.stop_propagation();
                        let dx = if d.x == px(0.) { d.y } else { d.x };
                        let next = (side.offset - dx).clamp(px(0.), side.max());
                        if next != side.offset {
                            side.offset = next;
                            cx.notify(view_id);
                        }
                    });

                    // The scrollbar: drag its thumb, or press the track to jump there.
                    let (s2, k2, h2) = (store.clone(), key.clone(), hitbox.clone());
                    window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble || e.button != MouseButton::Left || !h2.is_hovered(window) || !track.contains(&e.position) {
                            return;
                        }
                        let mut all = s2.borrow_mut();
                        let Some(side) = all.get_mut(&k2) else { return };
                        let Some(bar) = side.thumb(track) else { return };
                        if !bar.contains(&e.position) {
                            // Center the thumb under the press.
                            let room = track.size.width - bar.size.width;
                            let at = (e.position.x - track.left() - bar.size.width / 2.).clamp(px(0.), room);
                            side.offset = if room > px(0.) { side.max() * (at / room) } else { px(0.) };
                        }
                        side.drag = Some((e.position.x, side.offset));
                        cx.stop_propagation();
                        cx.notify(view_id);
                    });
                    let s3 = store.clone();
                    let k3 = key.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, _, _, cx| {
                        let mut all = s3.borrow_mut();
                        let Some(side) = all.get_mut(&k3) else { return };
                        let hovering = track.contains(&e.position);
                        if side.drag.is_none() {
                            if hovering != side.hover {
                                side.hover = hovering;
                                cx.notify(view_id);
                            }
                            return;
                        }
                        let (Some((x, from)), Some(bar)) = (side.drag, side.thumb(track)) else { return };
                        let room = track.size.width - bar.size.width;
                        if room > px(0.) {
                            side.offset = (from + (e.position.x - x) * (side.max() / room)).clamp(px(0.), side.max());
                            cx.stop_propagation();
                            cx.notify(view_id);
                        }
                    });
                    let s4 = store.clone();
                    window.on_mouse_event(move |e: &MouseUpEvent, _, _, cx| {
                        if e.button == MouseButton::Left
                            && let Some(side) = s4.borrow_mut().get_mut(&key)
                            && side.drag.take().is_some()
                        {
                            cx.notify(view_id);
                        }
                    });
                },
            )
            .absolute()
            .inset_0(),
        )
    }

    /// A change between two texts, as `diff_body` draws it, scrolling up and down
    /// as a whole and sideways under a fixed gutter.
    pub(super) fn diff_pane(&self, key: SharedString, source: LineSource, old: &str, new: &str, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let name = match &source {
            LineSource::File(path) | LineSource::Diff(path) | LineSource::GitDiff(path, _) => path.to_string_lossy().into_owned(),
        };
        let this = cx.weak_entity();
        let select = Some((&this, &source, self.selected_rows(&source)));
        let Some((rows, longest, gutter)) = diff_body(colors, &name, old, new, self.sideways_offset(&key), select, window) else {
            return empty(colors, Glyph::FileDiff, "No changes", "The two sides are the same.").into_any_element();
        };
        let scroll = self.sideways.borrow_mut().entry(key.clone()).or_default().scroll.clone();
        let list = div().id(key.clone()).size_full().overflow_y_scroll().track_scroll(&scroll).child(rows);
        self.scroll_sideways(key, longest, gutter, window, list, cx).vertical_scrollbar(&scroll).into_any_element()
    }

    /// The selected rows, when they are in `source`.
    fn selected_rows(&self, source: &LineSource) -> Option<std::ops::RangeInclusive<usize>> {
        self.line_selection.as_ref().filter(|l| &l.source == source).map(LineSelection::rows)
    }

    /// A press in a code row's gutter. The `+` atop picked lines adds them to
    /// the chat; anywhere else picks lines.
    fn press_gutter(&mut self, source: LineSource, n: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !extend && self.selected_rows(&source).is_some_and(|rows| *rows.start() == n) {
            self.add_to_chat(&AddToChat, window, cx);
            return;
        }
        self.press_line(source, n, extend, window, cx);
    }

    fn press_line(&mut self, source: LineSource, n: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.borrow_mut().clear();
        match &mut self.line_selection {
            Some(lines) if extend && lines.source == source => {
                lines.head = n;
                lines.dragging = true;
            }
            _ => self.line_selection = Some(LineSelection { source, anchor: n, head: n, dragging: true }),
        }
        // Away from the composer, so Escape reaches the workspace.
        self.focus.focus(window);
        cx.notify();
    }

    fn drag_line(&mut self, source: &LineSource, n: usize, cx: &mut Context<Self>) {
        if let Some(lines) = &mut self.line_selection
            && lines.dragging
            && &lines.source == source
            && lines.head != n
        {
            lines.head = n;
            cx.notify();
        }
    }

    /// The texts a diff source compares, as its pane shows them.
    fn diff_sides(&self, source: &LineSource) -> Option<(String, String)> {
        match source {
            LineSource::File(_) => None,
            LineSource::Diff(path) => {
                let change = self.current_thread()?.changes().into_iter().find(|c| &c.path == path)?;
                Some((change.old.unwrap_or_default(), change.new))
            }
            LineSource::GitDiff(path, staged) => match self.git_diffs.get(&(path.clone(), *staged)) {
                Some(Ok(crate::git::Diff::Text { old, new })) => Some((old.clone(), new.clone())),
                _ => None,
            },
        }
    }

    /// The selected lines as a mention to add to the composer.
    pub(super) fn lines_mention(&self, lines: &LineSelection) -> Option<FileMention> {
        let root = self.current_project().path.as_deref();
        let rows = lines.rows();
        match &lines.source {
            LineSource::File(path) => {
                let Some(Ok(file)) = self.files.get(path) else { return None };
                let picked = file.lines.get(*rows.start()..=(*rows.end()).min(file.lines.len().checked_sub(1)?))?;
                let text = picked.iter().map(|l| l.as_ref()).collect::<Vec<&str>>().join("\n");
                Some(super::mentions::file_lines_mention(path, root, rows.start() + 1, rows.start() + picked.len(), &text))
            }
            LineSource::Diff(path) | LineSource::GitDiff(path, _) => {
                let (old, new) = self.diff_sides(&lines.source)?;
                let all = diff_lines(&old, &new);
                let picked = all.get(*rows.start()..=(*rows.end()).min(all.len().checked_sub(1)?))?;
                // A leading hunk header says nothing a line range doesn't.
                let picked = if picked.first().is_some_and(|l| l.kind == LineKind::Hunk) { &picked[1..] } else { picked };
                if picked.is_empty() {
                    return None;
                }
                Some(super::mentions::diff_lines_mention(path, root, diff_span(picked), &diff_text(picked)))
            }
        }
    }

    pub(super) fn diff_view(&self, path: PathBuf, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let relative = self.relative(&path);
        let change = self.current_thread().and_then(|t| t.changes().into_iter().find(|c| c.path == path));
        let Some(change) = change else {
            return empty(colors, Glyph::FileDiff, relative, "This thread hasn't changed this file.").into_any_element();
        };
        col()
            .size_full()
            .child(pane_header(
                colors,
                Glyph::FileDiff,
                &relative,
                row()
                    .gap_3()
                    .when(change.old.is_none(), |s| s.child(pill("New file", colors.green, colors.green_tint).font_family(crate::theme::ui_font())))
                    .child(stat(colors, change.adds as u32, change.dels as u32)),
            ))
            .child(div().flex_1().min_h_0().child(self.diff_pane(
                SharedString::from(format!("diff-{}", path.display())),
                LineSource::Diff(path.clone()),
                change.old.as_deref().unwrap_or(""),
                &change.new,
                window,
                cx,
            )))
            .into_any_element()
    }

    pub(super) fn terminal_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        match &self.terminal {
            Some(Ok(terminal)) => terminal.clone().into_any_element(),
            Some(Err(error)) => empty(colors, Glyph::Terminal, "Could not start terminal", error.clone()).into_any_element(),
            None => empty(colors, Glyph::Terminal, "Terminal", "Open a terminal to start a shell.").into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LineKind, diff_lines, diff_span, diff_text};

    #[test]
    fn diff_snippets_mark_lines_and_span_the_new_side() {
        let lines = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(diff_text(&lines[1..]), " a\n-b\n+B\n c");
        assert_eq!(diff_span(&lines[2..4]), Some((2, 2)));
        assert_eq!(diff_span(&lines[2..3]), Some((2, 2)));
    }

    #[test]
    fn numbers_both_sides_of_a_diff() {
        let lines = diff_lines("a\nb\nc\n", "a\nB\nc\nd\n");
        let shape: Vec<_> = lines.iter().map(|l| (l.kind == LineKind::Hunk, l.old, l.new, l.text.as_str())).collect();
        assert_eq!(
            shape,
            [
                (true, None, None, "@@ -1,3 +1,4 @@"),
                (false, Some(1), Some(1), "a"),
                (false, Some(2), None, "b"),
                (false, None, Some(2), "B"),
                (false, Some(3), Some(3), "c"),
                (false, None, Some(4), "d"),
            ]
        );
    }
}
