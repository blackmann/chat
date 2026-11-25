//! What opens beside the chat: a file, a diff, or a terminal.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::{ActiveTheme, Icon, IconName, scroll::ScrollableElement};

const LINE: f32 = 20.;
/// The file header's Open with button, where its list opens.
pub(super) const OPEN_WITH: &str = "open-with";
/// Unchanged lines kept around each change in a diff.
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LineKind {
    Hunk,
    Context,
    Add,
    Del,
}

pub(super) struct DiffLine {
    pub kind: LineKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

/// Blame for both sides of a diff pane, and which texts it was read for.
#[derive(Clone)]
pub(super) struct DiffBlame {
    sides: (u64, u64),
    old: Option<Rc<crate::git::FileInfo>>,
    new: Option<Rc<crate::git::FileInfo>>,
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Where a diff pane's cursor is, and what to say about who wrote that line.
pub(super) struct Cursor {
    row: usize,
    blame: Option<DiffBlame>,
    now: i64,
}

/// The pane whose lines are selected.
#[derive(Clone, PartialEq, Eq, Hash)]
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

/// The tint over selected code, as the transcript draws its selection.
fn text_tint(colors: Palette) -> Hsla {
    Hsla::from(rgb(colors.blue)).opacity(0.28)
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
pub(super) fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
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
pub(super) fn pane_header(colors: Palette, glyph: impl Into<Icon>, path: &str, trailing: impl IntoElement) -> Div {
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
        .child(icon(glyph).size(px(13.)).flex_shrink_0().text_color(rgb(colors.muted)))
        // A long path gives way: its folders shrink to an ellipsis, while the
        // file's name and what's on the right stay whole.
        .child(
            row()
                .debug_selector(|| "pane-path".into())
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .when(!dir.is_empty(), |s| {
                    s.child(div().min_w_0().flex_shrink().overflow_hidden().text_ellipsis().text_color(rgb(colors.muted)).child(format!("{dir}/")))
                })
                .child(div().flex_shrink_0().child(name.to_string())),
        )
        .child(div().debug_selector(|| "pane-trailing".into()).flex_shrink_0().child(trailing))
}

/// A file's change drawn as Zed draws one: hunks of the new file with the removed
/// lines among them, a coloured bar down the gutter beside each change, line
/// numbers, syntax colours, and a quiet gap between hunks instead of `@@` lines.
/// Returns the rows, the longest line in characters, and the gutter's width.
#[allow(clippy::too_many_arguments)]
fn diff_body(
    colors: Palette,
    name: &str,
    old: &str,
    new: &str,
    offset: Pixels,
    select: Option<Select>,
    decor: Decor,
    code_text: &select::Shared,
    cursor: Option<Cursor>,
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
        let row_marks = select.as_ref().map(|(_, source, _)| decor.row(source, n));
        let linked = row_marks.as_ref().is_some_and(|m| m.link.is_some());
        let marks = match row_marks {
            Some(row_marks) => symbols::overlay(marks, &line.text, row_marks, decor.tints),
            None => marks,
        };
        let selected = select.as_ref().and_then(|(_, _, rows)| rows.as_ref()).is_some_and(|rows| rows.contains(&n));
        let here = cursor.as_ref().filter(|c| c.row == n);
        let bg = match line.kind {
            _ if selected => Some(colors.selected),
            LineKind::Add => Some(colors.green_tint),
            LineKind::Del => Some(colors.red_tint),
            _ if here.is_some() => Some(colors.hover),
            _ => None,
        };
        // A removed line was last written in the old side; the rest, in the new.
        let note = here.and_then(|c| {
            let blame = c.blame.as_ref()?;
            let (side, at) = match line.kind {
                LineKind::Del => (blame.old.as_deref()?, line.old?),
                _ => (blame.new.as_deref()?, line.new?),
            };
            blame_note(colors, side, at - 1, c.now)
        });
        let text = SharedString::from(line.text);
        let pick = select.as_ref().map(|(this, source, _)| Pick { this: (*this).clone(), source: (*source).clone(), line: text.clone(), row: n, linked, text: code_text.clone(), tint: text_tint(colors) });
        let cells = code_row(colors, select.as_ref(), n)
            .when_some(bg, |s, bg| s.bg(rgb(bg)))
            .child(div().w(px(3.)).h_full().flex_shrink_0().when_some(bars[n], |s, hue| s.bg(rgb(hue))))
            // Removed lines have no place in the new file, so no number.
            .child(number(colors, if line.kind == LineKind::Del { None } else { line.new }, width).pr_3())
            .child(code_cell(offset, StyledText::new(text.clone()).with_highlights(marks), guides.at(levels[n]), pick, note));
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
pub(super) fn glyph_width(window: &Window) -> Pixels {
    text_width_in(window, "0000000000", crate::theme::mono_size(), mono()) / 10.
}

/// Who to tell when a line's code is clicked or hovered, which pane and row
/// it is, and the line as shown.
struct Pick {
    this: WeakEntity<Workspace>,
    source: LineSource,
    line: SharedString,
    row: usize,
    /// The word under the pointer here is a link (⌘ held).
    linked: bool,
    /// Text selected in the pane.
    text: select::Shared,
    tint: Hsla,
}

/// The marks code views draw over their lines: the picked symbol and the link
/// under the pointer while ⌘ is held.
pub(super) struct Decor<'a> {
    pub symbol: Option<&'a symbols::SymbolMark>,
    pub link: Option<&'a super::lsp::Link>,
    pub tints: symbols::Tints,
}
impl Decor<'_> {
    fn row(&self, source: &LineSource, row: usize) -> symbols::RowMarks<'_> {
        let mut marks = self.symbol.map(|s| s.row(source, row)).unwrap_or_default();
        marks.link = self.link.filter(|l| &l.source == source && l.row == row).map(|l| l.range.clone());
        marks
    }
}

pub(super) fn tints(colors: Palette) -> symbols::Tints {
    symbols::Tints {
        read: Hsla::from(rgb(colors.blue)).opacity(if colors.dark { 0.24 } else { 0.18 }),
        write: Hsla::from(rgb(colors.amber)).opacity(if colors.dark { 0.28 } else { 0.24 }),
        link: rgb(colors.blue).into(),
    }
}

/// A line's code, shifted left by how far the view has scrolled sideways and
/// clipped at the gutter, which stays put. Indent guides scroll with the code.
/// A plain click on a word picks it, anywhere else lets it go; ⌘-click goes to
/// its definition and ⌘⇧-click lists its references. Resting on a word shows
/// what the server knows about it.
fn code_cell(offset: Pixels, text: StyledText, guides: Guides, pick: Option<Pick>, after: Option<AnyElement>) -> Stateful<Div> {
    let lines = (0..guides.count).map(|k| {
        div().absolute().top_0().bottom_0().left(guides.step * k as f32).w(px(1.)).bg(guides.color)
    });
    let layout = text.layout().clone();
    let id = pick.as_ref().map_or(0, |p| p.row);
    let text = match &pick {
        Some(p) => select::selectable(&p.text, (p.row, 0), layout.clone(), p.line.clone(), text.into_any_element(), p.tint)
            .cursor(!p.linked)
            .into_any_element(),
        None => text.into_any_element(),
    };
    div()
        .id(("code-cell", id))
        .flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .items_center()
        .overflow_hidden()
        .when_some(pick, |s, pick| {
            let s = s.when(pick.linked, |s| s.cursor_pointer());
            let pick = Rc::new(pick);
            let (down, moved, left) = (pick.clone(), pick.clone(), pick);
            let (l1, l2) = (layout.clone(), layout);
            s.on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
                let at = l1.index_for_position(e.position).ok();
                let Pick { this, source, line, row, .. } = &*down;
                let m = e.modifiers;
                let (source, row) = (source.clone(), *row);
                this.update(cx, |s, cx| {
                    s.place_cursor(&source, row, cx);
                    if !m.platform {
                        s.press_text(&source, e.position, e.click_count, m.shift, window, cx);
                    }
                    match (m.platform, m.shift, m.alt || m.control) {
                    (true, true, false) => s.find_references_at(source, row, line, at, e.position, window, cx),
                    (true, false, false) => s.go_to_definition_at(source, row, line, at, e.position, window, cx),
                    (false, false, false) => s.pick_symbol(source, row, line, at, e.position, window, cx),
                    _ => {}
                    }
                })
                .ok();
            })
            .on_mouse_move(move |e: &MouseMoveEvent, _, cx| {
                // The text hasn't been laid out yet on the frame it first shows.
                let at = l2.index_for_position(e.position).ok();
                let Pick { this, source, line, row, text, .. } = &*moved;
                if text.borrow().dragging {
                    return;
                }
                this.update(cx, |s, cx| s.hover_code(source, *row, line, at, e.modifiers.platform, e.position, cx)).ok();
            })
            .on_hover(move |hovered: &bool, _, cx| {
                if !*hovered {
                    let Pick { this, source, row, .. } = &*left;
                    this.update(cx, |s, cx| s.leave_code(source, *row, cx)).ok();
                }
            })
        })
        .child(
        div()
            .relative()
            .h_full()
            .flex()
            .items_center()
            .flex_shrink_0()
            .whitespace_nowrap()
            .pr_4()
            .ml(-offset)
            // Where a row's code starts, for tests that point at words.
            .debug_selector(move || format!("code:{id}"))
            .children(lines)
            .child(text)
            .children(after),
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

/// Who last changed a file's line and when, written after the code of the
/// line the cursor is on; hovering shows the commit.
fn blame_note(colors: Palette, git: &crate::git::FileInfo, i: usize, now: i64) -> Option<AnyElement> {
    let commit = &git.commits[(*git.blame.get(i)?)?];
    let (who, tip) = if commit.committed() {
        let date = chrono::DateTime::from_timestamp(commit.time, 0).map(|d| d.format("%b %-d, %Y").to_string()).unwrap_or_default();
        (
            format!("{}, {}", commit.author, ago(now - commit.time)),
            format!("{} · {} · {}\n{}", &commit.sha[..7.min(commit.sha.len())], commit.author, date, commit.summary),
        )
    } else {
        ("You, not committed yet".to_owned(), "Not committed yet".to_owned())
    };
    Some(
        row()
            .id(("blame", i))
            .ml(px(48.))
            .gap_1()
            .flex_shrink_0()
            .text_color(rgb(colors.quiet))
            .hover(|s| s.text_color(rgb(colors.muted)))
            .child(icon(Glyph::Branch).size(px(12.)))
            .child(who)
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .into_any_element(),
    )
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

/// A row of a file view: one of the file's lines, or one of HEAD's lines a
/// change replaced, shown above it while that change is open.
#[derive(Clone)]
pub(super) enum FileRow {
    Line(usize),
    Old { text: SharedString, at: usize, hunk: usize },
}

/// What a click on a change's bar does: open or close the change starting at
/// `hunk`, and whether it's open now.
struct BarToggle {
    this: WeakEntity<Workspace>,
    path: PathBuf,
    hunk: usize,
    open: bool,
}

/// The bar beside a file's line numbers marking its change since HEAD: green
/// for added, amber for changed, and a red notch where lines were removed. A
/// change that replaced or removed lines opens them above it on a click.
fn change_bar(colors: Palette, id: usize, mark: Option<crate::git::Mark>, toggle: Option<BarToggle>) -> Stateful<Div> {
    use crate::git::Mark;
    let notch = || div().absolute().left(px(-2.)).w(px(7.)).h(px(2.)).bg(rgb(colors.red));
    let bar = div()
        .relative()
        .w(px(3.))
        .h_full()
        .when_some(mark, |s, mark| match mark {
            Mark::Added => s.bg(rgb(colors.green)),
            Mark::Modified => s.bg(rgb(colors.amber)),
            Mark::RemovedAbove => s.child(notch().top_0()),
            Mark::RemovedBelow => s.child(notch().bottom_0()),
        });
    bar_cell(id, bar, toggle)
}

/// A change bar's room, wider than the bar so it's easy to hit.
fn bar_cell(id: usize, bar: Div, toggle: Option<BarToggle>) -> Stateful<Div> {
    div().id(("change-bar", id)).h_full().w(px(13.)).flex_shrink_0().child(bar).when_some(toggle, |s, t| {
        let tip = if t.open { "Hide the previous version" } else { "Show the previous version" };
        s.cursor_pointer()
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                t.this.update(cx, |s, cx| s.toggle_hunk(&t.path, t.hunk, cx)).ok();
            })
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
    /// A path as headers show it: from its project root, or from home (`~/…`)
    /// when it's outside the project, such as a library's source.
    pub(super) fn relative(&self, path: &Path) -> String {
        let relative = self.current_project().relative(path);
        if relative.is_absolute() { crate::model::tilde(&relative) } else { relative.to_string_lossy().into_owned() }
    }

    pub(super) fn file_view(&self, path: PathBuf, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        if let Some(media) = self.media.get(&path) {
            return self.media_view(&path, media, cx);
        }
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
        let symbol = self.symbol.clone();
        let link = self.servers.link.clone();
        let tints = tints(colors);
        let git = self.file_git.get(&path).cloned();
        let rows = self.file_rows(&path).map(Rc::new);
        let list_count = rows.as_ref().map_or(count, |r| r.len());
        let old_styles = rows.as_ref().and(git.as_ref()).and_then(|g| syntax::cached(&path.to_string_lossy(), &g.head));
        let hunks_open = self.hunks_open.get(&path).cloned().unwrap_or_default();
        let has_blame = git.as_ref().is_some_and(|g| !g.blame.is_empty());
        let cursor = self.cursor_line.get(&source).copied();
        let blame = git.clone().filter(|_| has_blame && self.show_blame).zip(cursor);
        let now = chrono::Utc::now().timestamp();
        let code_text = self.code_text_for(&source);
        let file_path = path.clone();
        // Only the lines on screen are built, so long files stay cheap.
        let list = uniform_list(key.clone(), list_count, move |range, _, _| {
            // Rows paint themselves back in; ones scrolled away must not answer the pointer.
            code_text.borrow_mut().forget_layout();
            range
                .map(|k| {
                    let toggle = |hunk: usize| BarToggle { this: this.clone(), path: file_path.clone(), hunk, open: hunks_open.contains(&hunk) };
                    let i = match rows.as_ref().map(|r| &r[k]) {
                        Some(FileRow::Old { text, at, hunk }) => {
                            let marks = old_styles
                                .as_ref()
                                .and_then(|h| h.line(colors.dark, *at))
                                .filter(|marks| marks.iter().all(|(r, _)| r.end <= text.len() && text.is_char_boundary(r.end)))
                                .cloned()
                                .unwrap_or_default();
                            let bar = div().w(px(3.)).h_full().bg(rgb(colors.red));
                            return row()
                                .w_full()
                                .h(px(LINE))
                                .bg(rgb(colors.red_tint))
                                .child(div().w(px(GUTTER)).flex_shrink_0())
                                .child(number(colors, None, 44.).mr_2())
                                .child(bar_cell(count + k, bar, Some(toggle(*hunk))))
                                .child(code_cell(offset, StyledText::new(text.clone()).with_highlights(marks), guides.at(0), None, None).text_color(rgb(colors.code)))
                                .into_any_element();
                        }
                        Some(FileRow::Line(i)) => *i,
                        None => k,
                    };
                    let mark = git.as_ref().and_then(|g| g.marks.get(i).copied().flatten());
                    let hunk = mark
                        .and(git.as_ref())
                        .and_then(|g| g.hunks.iter().find(|h| !h.old.is_empty() && h.marks(i, count)))
                        .map(|h| toggle(h.new.start));
                    let line = file.lines[i].clone();
                    let marks = file.highlights.as_ref().and_then(|h| h.line(colors.dark, i)).cloned().unwrap_or_default();
                    let decor = Decor { symbol: symbol.as_ref(), link: link.as_ref(), tints };
                    let row_marks = decor.row(&source, i);
                    let linked = row_marks.link.is_some();
                    let marks = symbols::overlay(marks, &line, row_marks, tints);
                    let pick = Pick { this: this.clone(), source: source.clone(), line: line.clone(), row: i, linked, text: code_text.clone(), tint: text_tint(colors) };
                    code_row(colors, Some(&(&this, &source, selected.clone())), i)
                        .when_some(flash.as_ref().filter(|(rows, _)| rows.contains(&i)), |s, (_, k)| {
                            s.bg(Hsla::from(rgb(colors.amber)).opacity(0.22 * k))
                        })
                        .when(cursor == Some(i), |s| s.bg(rgb(colors.hover)))
                        .on_mouse_down(MouseButton::Right, {
                            let (this, path, selected) = (this.clone(), file_path.clone(), selected.clone());
                            move |e: &MouseDownEvent, window, cx| {
                                // The lines picked, when the press is on them; else this one.
                                let rows = selected.clone().filter(|rows| rows.contains(&i)).unwrap_or(i..=i);
                                let lines = (rows.start() + 1, rows.end() + 1);
                                this.update(cx, |s, cx| s.code_menu(path.clone(), lines, e.position, window, cx)).ok();
                            }
                        })
                        .when(selected.as_ref().is_some_and(|rows| rows.contains(&i)), |s| s.bg(rgb(colors.selected)))
                        .child(number(colors, Some(i + 1), 44.).mr_2())
                        .child(change_bar(colors, i, mark, hunk))
                        .child(
                            code_cell(
                                offset,
                                StyledText::new(line).with_highlights(marks),
                                guides.at(file.guides.1[i]),
                                Some(pick),
                                blame.as_ref().filter(|(_, at)| *at == i).and_then(|(g, _)| blame_note(colors, g, i, now)),
                            )
                                .text_color(rgb(colors.code)),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>()
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
                    .when_some(self.lsp_badge(&path), |s, badge| {
                        let text = if badge.warn {
                            pill(badge.text, colors.amber, colors.amber_tint)
                        } else {
                            label(colors, badge.text)
                        };
                        let tip = badge.tooltip;
                        s.child(
                            div()
                                .id("lsp-badge")
                                .child(text)
                                .when_some(tip, |s, tip| s.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))),
                        )
                    })
                    .child(label(colors, format!("{count} lines")))
                    .child(self.open_with_button(&path, cx))
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
                    .child(self.scroll_sideways(key, longest, GUTTER + 65., window, list, cx))
                    .vertical_scrollbar(&scroll)
                    .on_mouse_move(cx.listener(Self::drag_text))
                    .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| s.release_text(cx)))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, cx| s.release_text(cx))),
            )
            .into_any_element()
    }

    /// A file view's rows while any of its changes are open, HEAD's lines for
    /// each placed above the lines that replaced them; `None` when none are.
    pub(super) fn file_rows(&self, path: &Path) -> Option<Vec<FileRow>> {
        let open = self.hunks_open.get(path).filter(|o| !o.is_empty())?;
        let git = self.file_git.get(path)?;
        let Some(Ok(file)) = self.files.get(path) else { return None };
        let head: Vec<&str> = git.head.lines().collect();
        let mut hunks = git.hunks.iter().filter(|h| open.contains(&h.new.start)).peekable();
        let mut rows = Vec::with_capacity(file.lines.len());
        for i in 0..=file.lines.len() {
            while let Some(h) = hunks.next_if(|h| h.new.start <= i) {
                rows.extend(h.old.clone().filter_map(|at| {
                    Some(FileRow::Old { text: syntax::expand_tabs(head.get(at)?).into(), at, hunk: h.new.start })
                }));
            }
            if i < file.lines.len() {
                rows.push(FileRow::Line(i));
            }
        }
        Some(rows)
    }

    /// Where file line `line` sits in its view's list, past any open changes above it.
    pub(super) fn file_list_index(&self, path: &Path, line: usize) -> usize {
        let (Some(open), Some(git)) = (self.hunks_open.get(path), self.file_git.get(path)) else { return line };
        line + git.hunks.iter().filter(|h| open.contains(&h.new.start) && h.new.start <= line).map(|h| h.old.len()).sum::<usize>()
    }

    /// The file line at row `index` of its view's list; an open change's old
    /// lines count as the line below them.
    pub(super) fn file_line_at(&self, path: &Path, index: usize) -> usize {
        let (Some(open), Some(git)) = (self.hunks_open.get(path), self.file_git.get(path)) else { return index };
        let mut shift = 0;
        for h in git.hunks.iter().filter(|h| open.contains(&h.new.start)) {
            let first = h.new.start + shift;
            if index < first {
                break;
            }
            if index < first + h.old.len() {
                return h.new.start;
            }
            shift += h.old.len();
        }
        index - shift
    }

    /// Open or close the change starting at line `hunk`: HEAD's lines it
    /// replaced show above it while open.
    fn toggle_hunk(&mut self, path: &Path, hunk: usize, cx: &mut Context<Self>) {
        let open = self.hunks_open.entry(path.to_path_buf()).or_default();
        if !open.remove(&hunk) {
            open.insert(hunk);
        }
        cx.notify();
    }

    /// The header's way out to another editor: a list of the installed ones.
    fn open_with_button(&self, path: &Path, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let trigger = row()
            .id(OPEN_WITH)
            .debug_selector(|| OPEN_WITH.into())
            .h(px(22.))
            .px(px(6.))
            .gap(px(4.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_size(px(crate::theme::scaled_ui(12.)))
            .text_color(rgb(colors.muted))
            .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
            .child(icon(IconName::ExternalLink).size(px(13.)))
            .child("Open with")
            .child(icon(IconName::ChevronDown).size(px(12.)));
        let path = path.to_path_buf();
        self.popover_trigger(OPEN_WITH, trigger, Corner::TopRight, 220., None, move |s, _, _| {
            super::chrome::open_with_entries(&s.editors, &path, false)
        }, cx)
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
        let decor = Decor { symbol: self.symbol.as_ref(), link: self.servers.link.as_ref(), tints: tints(colors) };
        let cursor = self.cursor_line.get(&source).map(|&row| Cursor {
            row,
            blame: self.show_blame.then(|| self.diff_blame_for(&source, old, new, cx)).flatten(),
            now: chrono::Utc::now().timestamp(),
        });
        let Some((rows, longest, gutter)) =
            diff_body(colors, &name, old, new, self.sideways_offset(&key), select, decor, &self.code_text_for(&source), cursor, window)
        else {
            return empty(colors, Glyph::FileDiff, "No changes", "The two sides are the same.").into_any_element();
        };
        let scroll = self.sideways.borrow_mut().entry(key.clone()).or_default().scroll.clone();
        let list = div().id(key.clone()).size_full().overflow_y_scroll().track_scroll(&scroll).child(rows);
        self.code_text.borrow_mut().forget_layout();
        self.scroll_sideways(key, longest, gutter, window, list, cx)
            .vertical_scrollbar(&scroll)
            .on_mouse_move(cx.listener(Self::drag_text))
            .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| s.release_text(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, cx| s.release_text(cx)))
            .into_any_element()
    }

    /// Put a pane's cursor on row `row`, where its blame shows.
    fn place_cursor(&mut self, source: &LineSource, row: usize, cx: &mut Context<Self>) {
        if self.cursor_line.insert(source.clone(), row) != Some(row) {
            cx.notify();
        }
    }

    /// Blame for a diff pane's sides, as last read; reads it again in the
    /// background when the sides have changed since.
    fn diff_blame_for(&self, source: &LineSource, old: &str, new: &str, cx: &mut Context<Self>) -> Option<DiffBlame> {
        let (LineSource::Diff(path) | LineSource::GitDiff(path, _)) = source else { return None };
        let sides = (hash_text(old), hash_text(new));
        let mut all = self.diff_blame.borrow_mut();
        match all.get(source) {
            Some(known) if known.sides == sides => return Some(known.clone()),
            _ => {}
        }
        // Marked as read, so the frames until it lands don't read it again.
        all.insert(source.clone(), DiffBlame { sides, old: None, new: None });
        let (source, path, old, new) = (source.clone(), path.clone(), old.to_owned(), new.to_owned());
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move {
                // A new file has no old side worth blaming.
                let old = (!old.is_empty()).then(|| crate::git::blame_text(&path, &old)).flatten();
                (old, crate::git::blame_text(&path, &new))
            });
            let (old, new) = read.await;
            let (old, new) = (old.map(Rc::new), new.map(Rc::new));
            let _ = this.update(cx, |s, cx| {
                let mut all = s.diff_blame.borrow_mut();
                // Unless the sides moved on meanwhile.
                if let Some(known) = all.get_mut(&source).filter(|k| k.sides == sides) {
                    (known.old, known.new) = (old, new);
                    cx.notify();
                }
            });
        })
        .detach();
        None
    }

    /// The text selection for a pane showing `source`: a selection made in
    /// another pane no longer shows.
    fn code_text_for(&self, source: &LineSource) -> select::Shared {
        if self.code_text_source.as_ref() != Some(source) {
            self.code_text.borrow_mut().clear();
        }
        self.code_text.clone()
    }

    /// A press on a line's code: places the text cursor, takes a word on a
    /// double click and the line on a triple, and extends with shift.
    fn press_text(&mut self, source: &LineSource, at: Point<Pixels>, clicks: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.code_text_source.as_ref() == Some(source);
        if !same {
            self.code_text.borrow_mut().clear();
            self.code_text_source = Some(source.clone());
        }
        self.line_selection = None;
        self.selection.borrow_mut().clear();
        self.code_text.borrow_mut().press_at(at, clicks, extend && same, false);
        // Away from the composer, so ⌘ C reaches the workspace.
        self.focus.focus(window);
        cx.notify();
    }

    fn drag_text(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if e.pressed_button == Some(MouseButton::Left) && self.code_text.borrow_mut().drag(e.position) {
            cx.notify();
        }
    }

    fn release_text(&mut self, cx: &mut Context<Self>) {
        let mut text = self.code_text.borrow_mut();
        if text.dragging {
            text.dragging = false;
            cx.notify();
        }
    }

    /// The code selected in a file or diff pane, as the lines show it.
    pub(super) fn code_text_selected(&self) -> Option<String> {
        let (((first, _), from), ((last, _), to)) = self.code_text.borrow().span()?;
        let source = self.code_text_source.as_ref()?;
        // Each row as drawn and as written, which differ where tabs were expanded.
        let lines: Vec<Option<(String, String)>> = match source {
            LineSource::File(path) => {
                let Some(Ok(file)) = self.files.get(path) else { return None };
                let shown = file.lines.get(first..=last.min(file.lines.len().checked_sub(1)?))?;
                shown.iter().zip(file.text.lines().skip(first)).map(|(s, raw)| Some((s.to_string(), raw.to_string()))).collect()
            }
            _ => {
                let (old, new) = self.diff_sides(source)?;
                let all = diff_lines(&old, &new);
                let raw = |text: &str, n: Option<usize>| n.and_then(|n| text.lines().nth(n - 1)).map(str::to_string);
                // Hunk headers aren't drawn, so they aren't copied.
                all.get(first..=last.min(all.len().checked_sub(1)?))?
                    .iter()
                    .map(|l| match l.kind {
                        LineKind::Hunk => None,
                        LineKind::Del => Some((l.text.clone(), raw(&old, l.old).unwrap_or_else(|| l.text.clone()))),
                        _ => Some((l.text.clone(), raw(&new, l.new).unwrap_or_else(|| l.text.clone()))),
                    })
                    .collect()
            }
        };
        let picked: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter_map(|(k, line)| {
                let (shown, raw) = line.as_ref()?;
                let i = first + k;
                let a = if i == first { from.min(shown.len()) } else { 0 };
                let b = if i == last { to.min(shown.len()) } else { shown.len() };
                Some(syntax::source_span(raw, a, b.max(a)))
            })
            .collect();
        Some(picked.join("\n"))
    }

    /// The rows the code selection runs over, as whole lines to add to the chat.
    pub(super) fn code_text_lines(&self) -> Option<LineSelection> {
        let (((first, _), _), ((last, _), _)) = self.code_text.borrow().span()?;
        Some(LineSelection { source: self.code_text_source.clone()?, anchor: first, head: last, dragging: false })
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
        self.place_cursor(&source, n, cx);
        self.selection.borrow_mut().clear();
        self.code_text.borrow_mut().clear();
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
    pub(super) fn diff_sides(&self, source: &LineSource) -> Option<(String, String)> {
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

    pub(super) fn terminal_view(&self, cwd: &Path, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        match self.terminals.get(cwd) {
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
