//! Small building blocks shared by every view, in Gitflow's visual roles.
use super::*;
use crate::menu::Entry;
use gpui_component::{
    Icon, Sizable,
    button::{Button, ButtonVariants},
    spinner::Spinner,
};

/// Shared layout rhythm: tab strip and panel headers line up.
pub const BAR: f32 = 34.;
pub const RAIL: f32 = 52.;

pub fn row() -> Div {
    div().flex().items_center()
}
pub fn col() -> Div {
    div().flex().flex_col()
}
pub fn label(colors: Palette, text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(colors.muted))
        .text_size(px(12.))
        .child(text.into())
}
pub fn icon(name: impl Into<Icon>) -> Icon {
    name.into().size(px(14.))
}
pub fn icon_button(id: impl Into<ElementId>, name: impl Into<Icon>, tip: &'static str) -> Button {
    Button::new(id).icon(name).ghost().small().tooltip(tip)
}
/// An icon toggle that reads as "on" through color.
pub fn toggle(colors: Palette, id: &'static str, name: impl Into<Icon>, tip: &'static str, on: bool) -> Button {
    Button::new(id)
        .icon(icon(name).text_color(rgb(if on { colors.blue } else { colors.muted })))
        .ghost()
        .xsmall()
        .tooltip(tip)
}
pub fn divider(colors: Palette) -> Div {
    div()
        .w(px(1.))
        .h(px(16.))
        .mx_1()
        .flex_shrink_0()
        .bg(rgb(colors.border))
}
pub fn pill(text: impl Into<SharedString>, fg: u32, bg: u32) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(4.))
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}
pub fn count_badge(colors: Palette, n: usize) -> Div {
    div()
        .flex_shrink_0()
        .min_w(px(18.))
        .h(px(16.))
        .px(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(rgb(colors.hover))
        .text_size(px(11.))
        .text_color(rgb(colors.muted))
        .child(n.to_string())
}
/// A branch name behind its glyph, in mono on a quiet chip.
pub fn branch_chip(colors: Palette, text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .max_w(px(220.))
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(4.))
        .bg(rgb(colors.blue_tint))
        .text_color(rgb(colors.blue))
        .font_family(mono())
        .text_size(px(11.5))
        .child(icon(crate::icons::Glyph::Branch).size(px(11.)))
        .child(div().min_w_0().truncate().child(text.into()))
}
/// One line of text that ends in an ellipsis when it runs out of room. GPUI's
/// own truncation keeps whatever width the text was first measured at, which in
/// a flex row is rarely its final one, so this fits the text once bounds are known.
/// The element has no width of its own: give it one, or let a flex row stretch it.
pub fn one_line(text: impl Into<SharedString>, height: f32) -> Div {
    line_with_detail(text, None, height)
}

/// `one_line` with a trailing note in another colour, such as a file's folder.
/// Both share one line, so the note is what gives way first.
pub fn line_with_detail(text: impl Into<SharedString>, detail: Option<(SharedString, Hsla)>, height: f32) -> Div {
    let main: SharedString = text.into();
    div().h(px(height)).child(
        canvas(
            move |bounds, window, cx| {
                let style = window.text_style();
                let size = style.font_size.to_pixels(window.rem_size());
                let mut text = main.to_string();
                let mut runs = vec![style.to_run(main.len())];
                if let Some((detail, color)) = &detail {
                    let joined = format!("  {detail}");
                    runs.push(TextRun { color: *color, ..style.to_run(joined.len()) });
                    text.push_str(&joined);
                }
                let fitted = cx.text_system().line_wrapper(style.font(), size).truncate_line(text.into(), bounds.size.width, "…", &mut runs);
                window.text_system().shape_line(fitted, size, &runs, None)
            },
            move |bounds, line, window, cx| {
                let _ = line.paint(bounds.origin, bounds.size.height, window, cx);
            },
        )
        .size_full(),
    )
}

/// How wide `text` is in the interface font at `size`, for sizing to content.
pub fn text_width(window: &Window, text: &str, size: f32) -> Pixels {
    text_width_in(window, text, size, crate::theme::ui_font())
}
/// How wide `text` is in `family` at `size`.
pub fn text_width_in(window: &Window, text: &str, size: f32, family: SharedString) -> Pixels {
    let mut run = window.text_style().to_run(text.len());
    run.font.family = family;
    window.text_system().shape_line(text.to_string().into(), px(size), &[run], None).width
}

pub fn spinner(colors: Palette) -> Spinner {
    Spinner::new().xsmall().color(rgb(colors.amber).into())
}
/// "+12 −3" in the change colors; zero sides are left out.
pub fn stat(colors: Palette, adds: u32, dels: u32) -> Div {
    row()
        .flex_shrink_0()
        .gap(px(5.))
        .font_family(mono())
        .text_size(px(11.5))
        .when(adds > 0, |s| s.child(div().text_color(rgb(colors.green)).child(format!("+{adds}"))))
        .when(dels > 0, |s| s.child(div().text_color(rgb(colors.red)).child(format!("−{dels}"))))
}
/// A file's change as a letter and a color: added or modified.
pub fn status_mark(colors: Palette, added: bool) -> Div {
    let (letter, color) = if added { ("A", colors.green) } else { ("M", colors.amber) };
    div()
        .w(px(12.))
        .flex_shrink_0()
        .text_center()
        .font_family(mono())
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(color))
        .child(letter)
}

/// A centered placeholder for empty states.
pub fn empty(colors: Palette, glyph: impl Into<Icon>, title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Div {
    col()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .child(icon(glyph).size(px(22.)).text_color(rgb(colors.quiet)))
        .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
        .child(
            label(colors, detail)
                .max_w(px(420.))
                .text_center()
                .line_height(relative(1.45)),
        )
}

impl Workspace {
    /// Wrap a popover trigger so its painted bounds are known when it is clicked.
    pub(super) fn anchor(&self, id: &'static str, trigger: impl IntoElement) -> Div {
        let anchors = self.anchors.clone();
        div().relative().flex_shrink_0().child(trigger).child(
            canvas(
                move |bounds, _, _| {
                    anchors.borrow_mut().insert(id, bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }
    /// A trigger that opens a popover built from live workspace state.
    pub(super) fn popover_trigger(
        &self,
        id: &'static str,
        trigger: Stateful<Div>,
        corner: Corner,
        width: f32,
        search: Option<&'static str>,
        build: impl Fn(&Workspace, WeakEntity<Workspace>, &App) -> Vec<Entry> + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let weak = cx.weak_entity();
        let build: crate::menu::Builder = Rc::new(move |cx: &App| {
            weak.upgrade().map(|w| build(w.read(cx), weak.clone(), cx)).unwrap_or_default()
        });
        self.anchor(
            id,
            trigger.on_click(cx.listener(move |s, e: &ClickEvent, w, cx| {
                // Mouse-down outside already closed this trigger's menu: treat the click as a toggle.
                if s.dismissed.take().is_some_and(|(a, at)| a == id && at.elapsed() < Duration::from_millis(400)) {
                    return;
                }
                s.open_popover(id, e.position(), corner, width, search, build.clone(), w, cx)
            })),
        )
    }
}

/// A quiet heading inside a side panel, with a rule running to the edge.
pub fn section(colors: Palette, title: impl Into<SharedString>, count: Option<usize>) -> Div {
    row()
        .h(px(30.))
        .px_3()
        .gap_2()
        .text_size(px(12.))
        .text_color(rgb(colors.muted))
        .child(title.into())
        .when_some(count, |s, n| s.child(count_badge(colors, n)))
        .child(div().flex_1().h(px(1.)).bg(rgb(colors.row_border)))
}
pub fn card(colors: Palette) -> Div {
    col()
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(colors.border))
        .bg(rgb(colors.background))
        .overflow_hidden()
}

/// The file name and its directory, split for "name dir" rows.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

#[cfg(test)]
mod tests {
    // No glob import: `gpui::test` would shadow the built-in `#[test]`.
    use super::split_path;

    #[test]
    fn splits_paths() {
        assert_eq!(split_path("src/workspace/ui.rs"), ("ui.rs", "src/workspace"));
        assert_eq!(split_path("README.md"), ("README.md", ""));
    }
}
