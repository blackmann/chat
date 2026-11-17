//! What opens beside the chat: a file, a diff, or a terminal.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::Icon;

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
                text: change.value().trim_end_matches(['\n', '\r']).replace('\t', "    "),
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
        .text_size(px(12.))
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

    pub(super) fn file_view(&self, path: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let relative = self.relative(&path);
        let file = match self.files.get(&path) {
            Some(Ok(file)) => file.clone(),
            Some(Err(why)) => return empty(colors, Glyph::FileText, relative, why.clone()).into_any_element(),
            None => return empty(colors, Glyph::FileText, relative, "This file is no longer open.").into_any_element(),
        };
        let edited = self.current_thread().is_some_and(|t| t.changes().iter().any(|c| c.path == path));
        let count = file.lines.len();
        // Only the lines on screen are built, so long files stay cheap.
        let list = uniform_list(SharedString::from(format!("file-{}", path.display())), count, move |range, _, _| {
            range
                .map(|i| {
                    let line = file.lines[i].clone();
                    let marks = file.highlights.as_ref().and_then(|h| h.line(colors.dark, i)).cloned().unwrap_or_default();
                    row()
                        .h(px(LINE))
                        .gap_4()
                        .pr_4()
                        .child(number(colors, Some(i + 1), 44.))
                        .child(div().flex_shrink_0().text_color(rgb(colors.code)).child(StyledText::new(line).with_highlights(marks)))
                })
                .collect()
        })
        .size_full()
        .py_2()
        .font_family(mono())
        .text_size(px(crate::theme::MONO_SIZE));
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
                    .child(label(colors, format!("{count} lines"))),
            ))
            .child(div().flex_1().min_h_0().child(list))
            .into_any_element()
    }

    pub(super) fn diff_view(&self, path: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let relative = self.relative(&path);
        let change = self.current_thread().and_then(|t| t.changes().into_iter().find(|c| c.path == path));
        let Some(change) = change else {
            return empty(colors, Glyph::FileDiff, relative, "This thread hasn't changed this file.").into_any_element();
        };
        // Each side is coloured as the whole file it is, then read off by line number.
        let name = path.to_string_lossy();
        let old = change.old.as_deref().unwrap_or("");
        let (before, after) = (syntax::cached(&name, old), syntax::cached(&name, &change.new));
        let mut lines = col().font_family(mono()).text_size(px(crate::theme::MONO_SIZE));
        for line in diff_lines(old, &change.new) {
            let (sign, fg, bg) = match line.kind {
                LineKind::Add => ("+", colors.green, Some(colors.green_tint)),
                LineKind::Del => ("−", colors.red, Some(colors.red_tint)),
                LineKind::Hunk => ("", colors.blue, Some(colors.blue_tint)),
                LineKind::Context => ("", colors.text, None),
            };
            let hunk = line.kind == LineKind::Hunk;
            lines = lines.child(
                row()
                    .h(px(if hunk { LINE + 4. } else { LINE }))
                    .pr_4()
                    .when_some(bg, |s, bg| s.bg(rgb(bg)))
                    .child(number(colors, line.old, 40.))
                    .child(number(colors, line.new, 40.))
                    .child(div().w(px(28.)).flex_shrink_0().text_center().text_color(rgb(fg)).child(sign))
                    .child(if hunk {
                        div().flex_shrink_0().text_color(rgb(colors.muted)).child(line.text)
                    } else {
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
                        div().flex_shrink_0().text_color(rgb(colors.code)).child(StyledText::new(line.text).with_highlights(marks))
                    }),
            );
        }
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
            .child(div().id(SharedString::from(format!("diff-{}", path.display()))).flex_1().min_h_0().overflow_scroll().child(lines))
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
    use super::{LineKind, diff_lines};

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
