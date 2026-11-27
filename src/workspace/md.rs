//! A small GitHub-flavoured Markdown renderer for what agents write, carried over
//! from Gitflow without its image loading. It draws inline code in the monospace
//! font (GPUI highlights can't switch fonts, so text is built from runs) and
//! spaces blocks itself.
use super::ui::{col, row};
use crate::theme::Palette;
use gpui::prelude::FluentBuilder;
use gpui::*;
use markdown::mdast::{AlignKind, Node};
use std::{cell::Cell, ops::Range};

/// How source newlines inside a paragraph read: Markdown files treat them as
/// spaces; chat messages treat them as line breaks.
#[derive(Clone, Copy, PartialEq)]
pub enum Breaks {
    #[allow(dead_code)]
    Soft,
    Hard,
}

struct Ctx {
    id: SharedString,
    next: Cell<usize>,
    colors: Palette,
    mono: SharedString,
    mono_size: Pixels,
    breaks: Breaks,
    /// Where selections are tracked, and the block this text is.
    select: Option<(super::select::Shared, usize)>,
    segment: Cell<usize>,
    on_link: Option<OnLink>,
}

/// What clicking a link does; without one, only web and mail links open.
pub type OnLink = std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Whether `url` belongs in the browser (or mail client) rather than the app.
pub fn is_web(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

impl Ctx {
    /// Draw `styled` as `child`, selectable when the text is in the transcript.
    fn selectable(&self, layout: TextLayout, text: SharedString, child: AnyElement) -> AnyElement {
        let Some((state, block)) = &self.select else {
            return child;
        };
        let n = self.segment.get();
        self.segment.set(n + 1);
        let color = Hsla::from(rgb(self.colors.blue)).opacity(0.28);
        super::select::selectable(state, (*block, n), layout, text, child, color).into_any_element()
    }
    fn id(&self, what: &str) -> ElementId {
        let n = self.next.get();
        self.next.set(n + 1);
        ElementId::Name(format!("{}/{what}{n}", self.id).into())
    }
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
    /// Part of a mention chip, which draws its own background and border.
    chip: bool,
}

/// One paragraph's worth of styled text, with link ranges for click handling.
#[derive(Default)]
struct Inline {
    text: String,
    runs: Vec<(usize, Style)>,
    links: Vec<(Range<usize>, String)>,
    /// Mentions drawn as chips: their range and icon.
    chips: Vec<(Range<usize>, SharedString)>,
    /// Local images, drawn below the text: as written, and where they are.
    images: Vec<(String, std::path::PathBuf)>,
    /// Whether file paths written as plain text become links.
    autolink: bool,
}

impl Inline {
    fn push(&mut self, s: &str, style: &Style) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        self.runs.push((s.len(), style.clone()));
        if let Some(url) = &style.link {
            match self.links.last_mut() {
                Some((r, u)) if u == url && r.end == start => r.end = self.text.len(),
                _ => self.links.push((start..self.text.len(), url.clone())),
            }
        }
    }
    /// `s`, with any file paths in it linked to themselves.
    fn push_linking_paths(&mut self, s: &str, style: &Style) {
        if !self.autolink || style.link.is_some() || style.chip {
            return self.push(s, style);
        }
        let mut at = 0;
        for span in path_spans(s) {
            self.push(&s[at..span.start], style);
            self.push(&s[span.clone()], &Style { link: Some(s[span.clone()].to_string()), ..style.clone() });
            at = span.end;
        }
        self.push(&s[at..], style);
    }
    fn walk(&mut self, nodes: &[Node], style: &Style, breaks: Breaks) {
        for n in nodes {
            match n {
                Node::Text(t) => match breaks {
                    Breaks::Soft => self.push_linking_paths(&t.value.replace('\n', " "), style),
                    Breaks::Hard => self.push_linking_paths(&t.value, style),
                },
                Node::Break(_) => self.push("\n", style),
                Node::InlineCode(c) => self.push_linking_paths(
                    &c.value,
                    &Style {
                        code: true,
                        ..style.clone()
                    },
                ),
                Node::Strong(s) => self.walk(
                    &s.children,
                    &Style {
                        bold: true,
                        ..style.clone()
                    },
                    breaks,
                ),
                Node::Emphasis(e) => self.walk(
                    &e.children,
                    &Style {
                        italic: true,
                        ..style.clone()
                    },
                    breaks,
                ),
                Node::Delete(d) => self.walk(
                    &d.children,
                    &Style {
                        strike: true,
                        ..style.clone()
                    },
                    breaks,
                ),
                // A mention from the composer: `[name](<chip:icon|sent>)`.
                Node::Link(l) if l.url.starts_with("chip:") => {
                    let icon = l.url["chip:".len()..].split('|').next().unwrap_or_default().to_string();
                    let mut name = Inline::default();
                    name.walk(&l.children, &Style::default(), Breaks::Soft);
                    let start = self.text.len();
                    let chip = Style { chip: true, ..Style::default() };
                    let shown = name.text.replace(' ', "\u{a0}");
                    self.push(&format!("{}{shown}{}", super::mentions::LEAD, super::mentions::TRAIL), &chip);
                    self.chips.push((start..self.text.len(), icon.into()));
                }
                Node::Link(l) => self.walk(
                    &l.children,
                    &Style {
                        link: Some(l.url.clone()),
                        ..style.clone()
                    },
                    breaks,
                ),
                Node::LinkReference(l) => self.walk(&l.children, style, breaks),
                Node::Image(i) if let Some(path) = crate::agent::local_image(&i.url) => self.images.push((i.url.clone(), path)),
                Node::Image(i) => self.push(
                    &format!("[{}]", if i.alt.is_empty() { "image" } else { &i.alt }),
                    &Style {
                        link: Some(i.url.clone()),
                        ..style.clone()
                    },
                ),
                Node::Html(h) => {
                    let lower = h.value.to_ascii_lowercase();
                    if lower.starts_with("<br") {
                        self.push("\n", style);
                    }
                }
                other => {
                    if let Some(children) = other.children() {
                        self.walk(children, style, breaks);
                    }
                }
            }
        }
    }
    fn element(mut self, cx: &Ctx) -> AnyElement {
        let images = std::mem::take(&mut self.images);
        if images.is_empty() {
            return self.text_element(cx);
        }
        let text = (!self.text.trim().is_empty()).then(|| self.text_element(cx));
        col().w_full().gap_2().children(text).children(images.into_iter().map(|(url, path)| image(url, path, cx))).into_any_element()
    }
    fn text_element(mut self, cx: &Ctx) -> AnyElement {
        let colors = cx.colors;
        let runs = self
            .runs
            .iter()
            .map(|(len, s)| TextRun {
                len: *len,
                font: Font {
                    family: if s.code || s.chip { cx.mono.clone() } else { crate::theme::ui_font() },
                    features: FontFeatures::default(),
                    fallbacks: None,
                    weight: if s.bold { FontWeight::SEMIBOLD } else { FontWeight::NORMAL },
                    style: if s.italic { FontStyle::Italic } else { FontStyle::Normal },
                },
                color: rgb(if s.link.is_some() {
                    colors.blue
                } else {
                    colors.text
                })
                .into(),
                background_color: (s.code || s.chip).then(|| rgb(colors.hover).into()),
                underline: s.link.as_ref().map(|_| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(Hsla::from(rgb(colors.blue)).opacity(0.4)),
                    wavy: false,
                }),
                strikethrough: s.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
            })
            .collect::<Vec<_>>();
        let text = SharedString::from(self.text);
        let styled = StyledText::new(text.clone()).with_runs(runs);
        let layout = styled.layout().clone();
        let chips = std::mem::take(&mut self.chips);
        let wrap = |child: AnyElement| -> AnyElement {
            if chips.is_empty() {
                return child;
            }
            Chips { child, layout: layout.clone(), chips: chips.clone(), border: rgb(colors.border).into(), ink: rgb(colors.muted).into() }
                .into_any_element()
        };
        if self.links.is_empty() {
            return cx.selectable(layout.clone(), text, wrap(styled.into_any_element()));
        }
        let (ranges, urls): (Vec<_>, Vec<_>) = self.links.into_iter().unzip();
        let on_link = cx.on_link.clone();
        let linked = InteractiveText::new(cx.id("text"), styled)
            .on_click(ranges, move |ix, window, cx| {
                let Some(url) = urls.get(ix) else { return };
                match &on_link {
                    Some(f) => f(url, window, cx),
                    None if is_web(url) => cx.open_url(url),
                    None => {}
                }
            })
            .into_any_element();
        cx.selectable(layout.clone(), text, wrap(linked))
    }
}

/// Text with mention chips drawn over it: a border around each mention and its
/// file's icon in the space left before the name, as in the composer.
struct Chips {
    child: AnyElement,
    layout: TextLayout,
    chips: Vec<(Range<usize>, SharedString)>,
    border: Hsla,
    ink: Hsla,
}

impl IntoElement for Chips {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Chips {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.prepaint(window, cx);
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
        let line = self.layout.line_height();
        for (range, icon) in &self.chips {
            let lead = range.start + super::mentions::LEAD.len();
            let (Some(start), Some(end), Some(name)) = (
                self.layout.position_for_index(range.start),
                self.layout.position_for_index(range.end),
                self.layout.position_for_index(lead),
            ) else {
                continue;
            };
            // A chip split across lines is left as highlighted text.
            if start.y != end.y {
                continue;
            }
            let chip = Bounds::from_corners(point(start.x, start.y + px(1.)), point(end.x, start.y + line - px(1.)));
            window.paint_quad(quad(chip, px(4.), transparent_black(), px(1.), self.border, BorderStyle::Solid));
            let room = name.x - start.x;
            let size = px(12.).min(room - px(4.)).max(px(8.));
            let at = point(start.x + (room - size) / 2., chip.center().y - size / 2.);
            let _ = window.paint_svg(Bounds { origin: at, size: Size { width: size, height: size } }, icon.clone(), TransformationMatrix::unit(), self.ink, cx);
        }
    }
}

/// An image in the transcript, at most a comfortable size; clicking it opens
/// it in a tab, as its link would.
fn image(url: String, path: std::path::PathBuf, cx: &Ctx) -> AnyElement {
    let on_link = cx.on_link.clone();
    div()
        .id(cx.id("image"))
        .flex_shrink_0()
        .max_w_full()
        .rounded(px(6.))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(cx.colors.border))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            if let Some(f) = &on_link {
                f(&url, window, cx);
            }
        })
        .child(img(path).max_w(px(560.)).max_h(px(420.)).object_fit(ObjectFit::Contain))
        .into_any_element()
}

fn inline(nodes: &[Node], cx: &Ctx) -> AnyElement {
    inline_styled(nodes, &Style::default(), cx)
}

fn inline_styled(nodes: &[Node], style: &Style, cx: &Ctx) -> AnyElement {
    let mut i = Inline { autolink: cx.on_link.is_some(), ..Inline::default() };
    i.walk(nodes, style, cx.breaks);
    i.element(cx)
}

/// How wide each column of a table should be, as fractions of the whole: by
/// its longest cell, within limits so short columns stay readable and long
/// ones wrap instead of crowding the rest out.
fn column_widths(t: &markdown::mdast::Table) -> Vec<f32> {
    let mut longest: Vec<usize> = Vec::new();
    for tr in &t.children {
        let Node::TableRow(tr) = tr else { continue };
        for (c, cell) in tr.children.iter().enumerate() {
            let mut i = Inline::default();
            i.walk(cell.children().map(Vec::as_slice).unwrap_or_default(), &Style::default(), Breaks::Soft);
            let len = i.text.chars().count();
            if c >= longest.len() {
                longest.resize(c + 1, 0);
            }
            longest[c] = longest[c].max(len);
        }
    }
    let weights: Vec<f32> = longest.iter().map(|&n| n.clamp(6, 40) as f32).collect();
    let total: f32 = weights.iter().sum();
    weights.iter().map(|w| w / total.max(1.)).collect()
}

/// Text inside HTML blocks (e.g. `<details>`), without the tags.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long a code block's copy button says "Copied" after a click.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

thread_local! {
    /// The copy button clicked last and when. Rendering here is stateless, so the
    /// button finds its feedback by its element id, which is stable across frames.
    static COPIED: std::cell::RefCell<Option<(ElementId, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

fn recently_copied(id: &ElementId) -> bool {
    COPIED.with_borrow(|c| matches!(c, Some((at, when)) if at == id && when.elapsed() < COPIED_FOR))
}

/// Show the check on `id`'s button, then redraw once it should go back to "Copy".
fn mark_copied(id: ElementId, cx: &mut App) {
    COPIED.set(Some((id, std::time::Instant::now())));
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(COPIED_FOR).await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

fn blocks(nodes: &[Node], cx: &Ctx) -> Vec<AnyElement> {
    nodes.iter().filter_map(|n| block(n, cx)).collect()
}

fn block(node: &Node, cx: &Ctx) -> Option<AnyElement> {
    let colors = cx.colors;
    Some(match node {
        Node::Paragraph(p) => div().w_full().line_height(relative(1.55)).child(inline(&p.children, cx)).into_any_element(),
        Node::Heading(h) => {
            let size = match h.depth {
                1 => 20.,
                2 => 17.,
                3 => 15.,
                _ => 14.,
            };
            div()
                .w_full()
                .pt(px(if h.depth <= 2 { 6. } else { 2. }))
                .text_size(px(size))
                .font_weight(FontWeight::SEMIBOLD)
                .when(h.depth <= 2, |d| d.pb(px(6.)).border_b_1().border_color(rgb(colors.row_border)))
                .child(inline(&h.children, cx))
                .into_any_element()
        }
        Node::List(l) => {
            let start = l.start.unwrap_or(1);
            let mut list = col().w_full().gap(px(if l.spread { 8. } else { 3. }));
            for (i, item) in l.children.iter().enumerate() {
                let Node::ListItem(item) = item else { continue };
                let marker = match item.checked {
                    Some(done) => div()
                        .mt(px(3.))
                        .size(px(13.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(rgb(if done { colors.green } else { colors.border }))
                        .when(done, |d| d.bg(rgb(colors.green)))
                        .into_any_element(),
                    None if l.ordered => div()
                        .text_color(rgb(colors.muted))
                        .child(format!("{}.", start as usize + i))
                        .into_any_element(),
                    None => div().text_color(rgb(colors.muted)).child("•").into_any_element(),
                };
                list = list.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(div().w(px(18.)).flex_shrink_0().flex().justify_end().line_height(relative(1.55)).child(marker))
                        .child(col().flex_1().min_w_0().gap(px(6.)).children(blocks(&item.children, cx))),
                );
            }
            list.into_any_element()
        }
        Node::Code(c) => {
            let code = c.value.clone();
            let copy_id = cx.id("copy");
            let copied = recently_copied(&copy_id);
            // Fenced blocks that name a language get its colours.
            let styles = c.lang.as_deref().and_then(|lang| super::syntax::cached(lang, &c.value));
            let lines = super::syntax::lines(&c.value);
            div()
                .relative()
                .w_full()
                .group("code")
                .child(
                    div()
                        .id(cx.id("code"))
                        .w_full()
                        .overflow_x_scroll()
                        // A sideways swipe scrolls the code; the page keeps vertical ones.
                        .map(|mut d| {
                            d.style().restrict_scroll_to_axis = Some(true);
                            d
                        })
                        .flex()
                        .rounded(px(6.))
                        .bg(rgb(colors.panel))
                        .border_1()
                        .border_color(rgb(colors.row_border))
                        .font_family(cx.mono.clone())
                        .text_size(cx.mono_size)
                        .line_height(relative(1.5))
                        .text_color(rgb(colors.code))
                        // Lines keep their full width, so long ones scroll rather than clip.
                        .child(col().flex_shrink_0().p(px(10.)).children(lines.into_iter().enumerate().map(|(n, line)| {
                            let marks = styles.as_ref().and_then(|s| s.line(colors.dark, n)).cloned().unwrap_or_default();
                            let styled = StyledText::new(line.clone()).with_highlights(marks);
                            let layout = styled.layout().clone();
                            div().whitespace_nowrap().min_h(px(4.)).child(cx.selectable(layout, line, styled.into_any_element()))
                        }))),
                )
                .child(
                    div()
                        .id(copy_id.clone())
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .text_size(px(crate::theme::scaled_ui(11.)))
                        .text_color(rgb(if copied { colors.green } else { colors.muted }))
                        .bg(rgb(colors.background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .cursor_pointer()
                        // Stays visible while it says the copy worked, hovered or not.
                        .when(!copied, |d| d.opacity(0.).group_hover("code", |d| d.opacity(1.)))
                        .when(copied, |d| {
                            d.child(super::ui::icon(gpui_component::IconName::Check).size(px(11.)).text_color(rgb(colors.green)))
                        })
                        .child(if copied { "Copied" } else { "Copy" })
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            mark_copied(copy_id.clone(), cx);
                        }),
                )
                .into_any_element()
        }
        Node::Blockquote(b) => col()
            .w_full()
            .gap(px(8.))
            .pl_3()
            .border_l_2()
            .border_color(rgb(colors.border))
            .text_color(rgb(colors.muted))
            .children(blocks(&b.children, cx))
            .into_any_element(),
        Node::ThematicBreak(_) => div().w_full().h(px(1.)).bg(rgb(colors.border)).into_any_element(),
        Node::Table(t) => {
            let widths = column_widths(t);
            let mut table = col()
                .w_full()
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(colors.border))
                .overflow_hidden();
            for (r, tr) in t.children.iter().enumerate() {
                let Node::TableRow(tr) = tr else { continue };
                let header = r == 0;
                let style = Style { bold: header, ..Style::default() };
                let mut line = row()
                    .w_full()
                    .map(|mut d| {
                        // Every cell as tall as the row, so the column rules run its full height.
                        d.style().align_items = Some(AlignItems::Stretch);
                        d
                    })
                    .when(!header, |d| d.border_t_1().border_color(rgb(colors.row_border)))
                    .when(header, |d| d.bg(rgb(colors.chrome)));
                for (c, cell) in tr.children.iter().enumerate() {
                    let Node::TableCell(cell) = cell else { continue };
                    let align = t.align.get(c).copied().unwrap_or(AlignKind::None);
                    line = line.child(
                        // A block, not a flex row, so the text wraps to the cell's width.
                        div()
                            .w(relative(widths.get(c).copied().unwrap_or(0.)))
                            .flex_shrink_0()
                            .min_w_0()
                            .px_2()
                            .py(px(5.))
                            .when(c > 0, |d| d.border_l_1().border_color(rgb(colors.row_border)))
                            .map(|d| match align {
                                AlignKind::Center => d.text_center(),
                                AlignKind::Right => d.text_right(),
                                _ => d,
                            })
                            .child(inline_styled(&cell.children, &style, cx)),
                    );
                }
                table = table.child(line);
            }
            table.into_any_element()
        }
        Node::Html(h) => {
            let text = strip_tags(&h.value);
            if text.is_empty() {
                return None;
            }
            div().w_full().text_color(rgb(colors.muted)).child(text).into_any_element()
        }
        Node::Root(r) => col().w_full().gap(px(10.)).children(blocks(&r.children, cx)).into_any_element(),
        Node::Definition(_) | Node::FootnoteDefinition(_) | Node::Yaml(_) | Node::Toml(_) => return None,
        other => {
            let children = other.children()?;
            div().w_full().child(inline(children, cx)).into_any_element()
        }
    })
}

/// Render Markdown text. Unparsable input falls back to plain text.
#[allow(clippy::too_many_arguments)]
pub fn render(
    id: SharedString,
    text: &str,
    breaks: Breaks,
    colors: Palette,
    mono: SharedString,
    mono_size: Pixels,
    select: Option<(super::select::Shared, usize)>,
    on_link: Option<OnLink>,
) -> AnyElement {
    let cx = Ctx { id, next: Cell::new(0), colors, mono, mono_size, breaks, select, segment: Cell::new(0), on_link };
    match markdown::to_mdast(text, &markdown::ParseOptions::gfm()) {
        Ok(Node::Root(root)) => col()
            .w_full()
            .gap(px(10.))
            .text_size(px(crate::theme::ui_size()))
            .children(blocks(&root.children, &cx))
            .into_any_element(),
        _ => div().text_size(px(crate::theme::ui_size())).child(text.to_string()).into_any_element(),
    }
}

/// Every file a message points at, as written: image and link targets, the
/// paths of mentions, and paths named in its prose or code spans. Not yet
/// resolved, so relative ones are still relative.
pub(super) fn mentioned_paths(text: &str) -> Vec<String> {
    fn walk(nodes: &[Node], out: &mut Vec<String>) {
        for n in nodes {
            match n {
                Node::Text(t) => out.extend(path_spans(&t.value).into_iter().map(|r| t.value[r].to_string())),
                Node::InlineCode(c) => out.extend(path_spans(&c.value).into_iter().map(|r| c.value[r].to_string())),
                Node::Image(i) => out.push(i.url.clone()),
                Node::Link(l) if l.url.starts_with("chip:") => {
                    if let Some(sent) = l.url.split_once('|').map(|(_, s)| s) {
                        out.push(sent.trim_start_matches('@').to_string());
                    }
                }
                Node::Link(l) => {
                    out.push(l.url.clone());
                    walk(&l.children, out);
                }
                other => {
                    if let Some(children) = other.children() {
                        walk(children, out);
                    }
                }
            }
        }
    }
    let Ok(root) = markdown::to_mdast(text, &markdown::ParseOptions::gfm()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(root.children().map(Vec::as_slice).unwrap_or_default(), &mut out);
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Where `s` names files or folders in plain text: `src/x.rs`, `src/x.rs:12`,
/// `./out/a.webm`, `/Users/me/notes`, `~/Downloads/a.png`. Relative paths need
/// a folder and an extension, so prose like "and/or" stays text.
fn path_spans(s: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, ch) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        match (ch.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(from)) => {
                start = None;
                let word = &s[from..i];
                let lead = word.len() - word.trim_start_matches(['(', '[', '<', '"', '\'', '`']).len();
                let word = word[lead..].trim_end_matches([')', ']', '>', '"', '\'', '`', '.', ',', ';', ':', '!', '?']);
                if is_path(word) {
                    spans.push(from + lead..from + lead + word.len());
                }
            }
            _ => {}
        }
    }
    spans
}

fn is_path(word: &str) -> bool {
    if word.contains("://") || !word.contains('/') {
        return false;
    }
    // A trailing `:12`, `:12:3` or `:12-20`.
    let mut path = word;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-') => path = head,
            _ => break,
        }
    }
    if !path.chars().all(|c| c.is_alphanumeric() || "/._-~@+".contains(c)) || path.contains("//") {
        return false;
    }
    let parts = path.split('/').filter(|p| !p.is_empty()).count();
    if path.starts_with('/') || path.starts_with("~/") {
        return parts >= 2;
    }
    let first = path.chars().next().unwrap_or(' ');
    let named = path.rsplit('/').next().and_then(|name| name.rsplit_once('.')).is_some_and(|(stem, ext)| {
        !stem.is_empty() && (1..=10).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    parts >= 2 && (first.is_alphanumeric() || first == '.' || first == '_') && (named || path.starts_with("./") || path.starts_with("../"))
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
    use super::{Breaks, Inline, Node, Style, mentioned_paths, path_spans, strip_tags};
    #[test]
    fn finds_every_mentioned_path() {
        let text = "Recorded `out/demo.mp4` and ![shot](/tmp/a.png).\n\nSee [the clip](./clips/b.webm), and [x](<chip:file|@shots/c.png>). Twice: out/demo.mp4";
        assert_eq!(mentioned_paths(text), ["out/demo.mp4", "/tmp/a.png", "./clips/b.webm", "shots/c.png"]);
    }
    fn flatten(src: &str, breaks: Breaks) -> Inline {
        let Ok(Node::Root(root)) = markdown::to_mdast(src, &markdown::ParseOptions::gfm()) else {
            panic!()
        };
        let Node::Paragraph(p) = &root.children[0] else { panic!() };
        let mut i = Inline::default();
        i.walk(&p.children, &Style::default(), breaks);
        i
    }
    #[test]
    fn local_images_leave_the_text() {
        let i = flatten("See ![chart](/tmp/chart.png) and ![remote](https://x.dev/a.png) and ![v](/tmp/a.mp4)", Breaks::Soft);
        assert_eq!(i.images, [("/tmp/chart.png".to_string(), std::path::PathBuf::from("/tmp/chart.png"))]);
        assert_eq!(i.text, "See  and [remote] and [v]");
    }

    #[test]
    fn styles_inline_code_and_links() {
        let i = flatten("Run `cargo test` or see [the docs](https://x.dev) now", Breaks::Soft);
        assert_eq!(i.text, "Run cargo test or see the docs now");
        assert!(i.runs.iter().any(|(len, s)| s.code && *len == "cargo test".len()));
        assert_eq!(i.links, vec![(22..30, "https://x.dev".to_string())]);
    }
    #[test]
    fn chip_links_become_chips() {
        let i = flatten("see [my file.rs](<chip:app-icons/file-code.svg|@src/my file.rs>) now", Breaks::Hard);
        assert_eq!(i.chips.len(), 1);
        assert_eq!(i.chips[0].1.as_ref(), "app-icons/file-code.svg");
        assert!(i.text[i.chips[0].0.clone()].contains("my\u{a0}file.rs"));
        assert!(i.links.is_empty() && i.text.starts_with("see ") && i.text.ends_with(" now"));
    }
    #[test]
    fn finds_paths_in_text() {
        let found = |s: &str| path_spans(s).into_iter().map(|r| s[r].to_string()).collect::<Vec<_>>();
        assert_eq!(found("Video: output/review-recordings/eng-1165/scan-to-admit.webm (about 35s)."), ["output/review-recordings/eng-1165/scan-to-admit.webm"]);
        assert_eq!(found("see (src/x.rs:12), ~/Downloads/a.png and /Users/me/notes."), ["src/x.rs:12", "~/Downloads/a.png", "/Users/me/notes"]);
        assert_eq!(found("and/or, km/h, 1.2/3.4, https://x.dev/a.rs, Cargo.toml, /"), Vec::<String>::new());
    }
    #[test]
    fn links_paths_only_when_asked() {
        let Ok(Node::Root(root)) = markdown::to_mdast("open `src/a.rs` or src/b.rs now", &markdown::ParseOptions::gfm()) else { panic!() };
        let Node::Paragraph(p) = &root.children[0] else { panic!() };
        let mut i = Inline { autolink: true, ..Inline::default() };
        i.walk(&p.children, &Style::default(), Breaks::Soft);
        assert_eq!(i.links.iter().map(|(_, u)| u.as_str()).collect::<Vec<_>>(), ["src/a.rs", "src/b.rs"]);
        assert!(flatten("open src/b.rs now", Breaks::Soft).links.is_empty());
    }
    #[test]
    fn soft_and_hard_breaks() {
        assert_eq!(flatten("one\ntwo", Breaks::Soft).text, "one two");
        assert_eq!(flatten("one\ntwo", Breaks::Hard).text, "one\ntwo");
    }
    #[test]
    fn strips_html_tags() {
        assert_eq!(strip_tags("<details><summary>More</summary> info</details>"), "More info");
    }
}
