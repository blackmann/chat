//! Selecting text in the transcript. Every piece of text the transcript draws
//! is a segment, keyed by where it sits (block, then order within the block),
//! so a selection can run across paragraphs, code lines and messages. Segments
//! record their layout as they paint; the mouse handlers on the transcript
//! turn pointer positions into (segment, byte offset) ends.
use gpui::*;
use std::{cell::RefCell, collections::BTreeMap, ops::Range, rc::Rc};

/// A segment's place in the transcript: its block, then its order inside it.
pub type Key = (usize, usize);
/// One end of a selection: a segment and a byte offset into its text.
pub type Point2 = (Key, usize);

struct Segment {
    layout: TextLayout,
    bounds: Bounds<Pixels>,
    text: SharedString,
}

#[derive(Default)]
pub struct Selection {
    segments: BTreeMap<Key, Segment>,
    anchor: Option<Point2>,
    head: Option<Point2>,
    pub dragging: bool,
}

pub type Shared = Rc<RefCell<Selection>>;

impl Selection {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
    }
    /// Forget where segments were drawn; each paints itself back in.
    pub fn forget_layout(&mut self) {
        self.segments.clear();
    }
    fn ordered(&self) -> Option<(Point2, Point2)> {
        let (a, b) = (self.anchor?, self.head?);
        Some(if a <= b { (a, b) } else { (b, a) })
    }
    /// The selection's two ends in order, when it covers anything.
    pub fn span(&self) -> Option<(Point2, Point2)> {
        self.ordered().filter(|(a, b)| a != b)
    }
    pub fn is_empty(&self) -> bool {
        self.ordered().is_none_or(|(a, b)| a == b)
    }
    /// The part of a segment's text that is selected.
    fn range(&self, key: Key, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered()?;
        if key < start.0 || key > end.0 {
            return None;
        }
        let from = if key == start.0 { start.1 } else { 0 };
        let to = if key == end.0 { end.1 } else { len };
        (from < to).then_some(from..to.min(len))
    }
    /// The selected text, a line per segment.
    pub fn text(&self) -> Option<String> {
        let (start, end) = self.ordered().filter(|(a, b)| a != b)?;
        let mut parts = Vec::new();
        for (key, segment) in self.segments.range(start.0..=end.0) {
            if let Some(range) = self.range(*key, segment.text.len()) {
                parts.push(segment.text.get(range).unwrap_or_default().to_string());
            } else if start.0 < *key && *key < end.0 {
                parts.push(String::new());
            }
        }
        Some(parts.join("\n"))
    }

    /// Where the selection starts and ends on screen, as of the last paint: the
    /// top of the first character and the top of the line after the last, each
    /// only when its segment was drawn.
    pub fn ends(&self) -> (Option<Point<Pixels>>, Option<Point<Pixels>>) {
        let Some((start, end)) = self.ordered().filter(|(a, b)| a != b) else {
            return (None, None);
        };
        let at = |(key, offset): Point2| {
            let segment = self.segments.get(&key)?;
            Some((segment.layout.position_for_index(offset.min(segment.text.len()))?, segment.layout.line_height()))
        };
        (at(start).map(|(p, _)| p), at(end).map(|(p, line)| point(p.x, p.y + line)))
    }

    /// The segment and offset under `position`. Outside every segment, `strict`
    /// finds nothing; otherwise the nearest text before the pointer is used, so
    /// a drag past the edge of a paragraph still selects to its end.
    fn hit(&self, position: Point<Pixels>, strict: bool) -> Option<Point2> {
        if let Some((key, segment)) = self.segments.iter().find(|(_, s)| s.bounds.contains(&position)) {
            return Some((*key, offset(segment, position)));
        }
        if strict {
            return None;
        }
        let before = self.segments.iter().rfind(|(_, s)| s.bounds.top() <= position.y);
        match before {
            Some((key, segment)) if position.y > segment.bounds.bottom() => Some((*key, segment.text.len())),
            Some((key, segment)) => Some((*key, offset(segment, position))),
            None => self.segments.iter().next().map(|(key, _)| (*key, 0)),
        }
    }
    /// Start a selection where the mouse went down: a click places it, a double
    /// click takes the word, a triple click the whole segment. Returns whether
    /// the press landed on text.
    pub fn press(&mut self, position: Point<Pixels>, clicks: usize, extend: bool) -> bool {
        self.press_at(position, clicks, extend, true)
    }
    /// `press`, where `strict` false lets a press beside the text (past a line's
    /// end, say) land on the nearest text, as code views want.
    pub fn press_at(&mut self, position: Point<Pixels>, clicks: usize, extend: bool, strict: bool) -> bool {
        let Some((key, at)) = self.hit(position, strict) else {
            self.clear();
            return false;
        };
        let text = self.segments[&key].text.clone();
        let (from, to) = match clicks {
            2 => word(&text, at),
            n if n >= 3 => (0, text.len()),
            _ => (at, at),
        };
        if extend && self.anchor.is_some() {
            self.head = Some((key, at));
        } else {
            self.anchor = Some((key, from));
            self.head = Some((key, to));
        }
        self.dragging = true;
        true
    }
    pub fn drag(&mut self, position: Point<Pixels>) -> bool {
        if !self.dragging {
            return false;
        }
        let head = self.hit(position, false);
        if head.is_some() && head != self.head {
            self.head = head;
            return true;
        }
        false
    }
}

fn offset(segment: &Segment, position: Point<Pixels>) -> usize {
    match segment.layout.index_for_position(position) {
        Ok(i) | Err(i) => i.min(segment.text.len()),
    }
}

/// The word around byte `at`: letters, digits and underscores, or just the one character.
fn word(text: &str, at: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(text.len());
    let start = text[..at].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(at, |(i, _)| i);
    let end = text[at..].char_indices().find(|(_, c)| !is_word(*c)).map_or(text.len(), |(i, _)| at + i);
    if start == end {
        let next = text[at..].chars().next().map_or(0, char::len_utf8);
        return (at, at + next);
    }
    (start, end)
}

/// Text that can be selected: draws `child` (a `StyledText`, or something built
/// around one, like `InteractiveText`) with its selected part highlighted.
pub struct Selectable {
    key: Key,
    child: AnyElement,
    layout: TextLayout,
    text: SharedString,
    state: Shared,
    color: Hsla,
    cursor: bool,
}

impl Selectable {
    /// Whether the text shows the I-beam; off where something else sets the cursor.
    pub fn cursor(mut self, on: bool) -> Self {
        self.cursor = on;
        self
    }
}

/// Wrap `child`, which draws a `StyledText` whose layout is `layout`.
pub fn selectable(state: &Shared, key: Key, layout: TextLayout, text: impl Into<SharedString>, child: AnyElement, color: Hsla) -> Selectable {
    Selectable { key, child, layout, text: text.into(), state: state.clone(), color, cursor: true }
}

impl IntoElement for Selectable {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Hitbox {
        self.child.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), hitbox: &mut Hitbox, window: &mut Window, cx: &mut App) {
        let range = {
            let mut state = self.state.borrow_mut();
            state.segments.insert(self.key, Segment { layout: self.layout.clone(), bounds, text: self.text.clone() });
            state.range(self.key, self.text.len())
        };
        if self.cursor {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }
        if let Some(range) = range {
            for rect in highlight(&self.layout, bounds, range) {
                window.paint_quad(fill(rect, self.color));
            }
        }
        self.child.paint(window, cx);
    }
}

/// Rectangles covering `range` of the laid-out text, a line at a time.
fn highlight(layout: &TextLayout, bounds: Bounds<Pixels>, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let line = layout.line_height();
    let (Some(start), Some(end)) = (layout.position_for_index(range.start), layout.position_for_index(range.end)) else {
        return vec![bounds];
    };
    if start.y == end.y {
        return vec![Bounds::from_corners(start, point(end.x, end.y + line))];
    }
    let mut rects = vec![Bounds::from_corners(start, point(bounds.right(), start.y + line))];
    if end.y > start.y + line {
        rects.push(Bounds::from_corners(point(bounds.left(), start.y + line), point(bounds.right(), end.y)));
    }
    rects.push(Bounds::from_corners(point(bounds.left(), end.y), point(end.x.max(bounds.left() + px(4.)), end.y + line)));
    rects
}

#[cfg(test)]
mod tests {
    use super::word;

    #[test]
    fn words_stop_at_punctuation() {
        assert_eq!(word("let foo_bar = 1;", 6), (4, 11));
        assert_eq!(word("a = b", 2), (2, 3));
        assert_eq!(word("héllo wörld", 8), (7, 13));
    }
}
