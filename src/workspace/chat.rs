//! The pinned chat tab: the thread's transcript above the composer.
use super::ui::*;
use super::*;
use crate::{
    agent::{Answer, Kind, Status},
    icons::Glyph,
    menu::{Entry, Item},
    model::{Permission, ToolCall},
};
use gpui_component::{Icon, IconName, Rope, scroll::ScrollableElement, input::{DocumentColorProvider, Input}, tooltip::Tooltip};
use lsp_types::{Color, ColorInformation, Position, Range};

/// The transcript and composer share one readable column.
const COLUMN: f32 = 760.;
/// How much of a tool's output an opened tool call shows: the end, where results are.
const TOOL_OUTPUT_LINES: usize = 200;
use super::mentions::MARKER as FILE_MARKER;

/// The file a link points at, and the 1-based lines it names, if any:
/// `file://` URLs and paths, absolute or relative to `root`, ending in
/// `:12`, `:12:3`, `:12-20`, `#L12` or `#L12-L20`.
fn link_path(url: &str, root: Option<&Path>) -> Option<(PathBuf, Option<(usize, usize)>)> {
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let (raw, anchor) = match raw.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (raw, None),
    };
    let raw = raw.split('?').next().unwrap_or(raw);
    let raw = percent_decode(raw);
    if raw.is_empty() || raw.contains("://") {
        return None;
    }
    let resolve = |p: &str| -> PathBuf {
        let p = Path::new(p);
        match root {
            Some(root) if p.is_relative() => root.join(p),
            _ => p.to_path_buf(),
        }
    };
    let mut lines = anchor.and_then(anchor_lines);
    // A trailing `:line`, `:line:col` or `:line-line`, unless it's the file's name.
    let mut candidate = raw.as_str();
    let mut suffix = Vec::new();
    for _ in 0..2 {
        if resolve(candidate).exists() {
            break;
        }
        match candidate.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-') => {
                suffix.insert(0, tail);
                candidate = head;
            }
            _ => break,
        }
    }
    if lines.is_none()
        && let Some(first) = suffix.first()
    {
        lines = line_range(first);
    }
    Some((resolve(candidate), lines))
}

/// `12` or `12-20` as a range of lines.
fn line_range(s: &str) -> Option<(usize, usize)> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let a: usize = a.parse().ok().filter(|&n| n > 0)?;
    let b = b.parse::<usize>().unwrap_or(a).max(a);
    Some((a, b))
}

/// GitHub's `L12` or `L12-L20` anchors.
fn anchor_lines(anchor: &str) -> Option<(usize, usize)> {
    let a = anchor.strip_prefix('L')?;
    line_range(&a.replace("-L", "-"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// When something happened, as briefly as it can be told from now: the time of
/// day today, then "Yesterday", then the date.
fn clock(at_ms: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, TimeZone};
    let Some(at) = chrono::Local.timestamp_millis_opt(at_ms).single() else { return String::new() };
    let time = at.format("%-I:%M %p");
    let (day, today) = (at.date_naive(), now.date_naive());
    if day == today {
        time.to_string()
    } else if today.pred_opt() == Some(day) {
        format!("Yesterday {time}")
    } else if day.year() == today.year() {
        format!("{} · {time}", at.format("%b %-d"))
    } else {
        format!("{} · {time}", at.format("%b %-d, %Y"))
    }
}

/// How long something took: `42s`, `3m 05s`, `1h 12m`.
fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// A quiet line of metadata under a message or turn.
fn meta(colors: Palette, text: String) -> Div {
    div().text_size(px(crate::theme::scaled_ui(11.))).text_color(rgb(colors.quiet)).child(text)
}

/// Agent thought updates often arrive as short lines separated by blank lines.
/// Empty lines are spacing, not another selectable update.
fn thought_lines(text: &str) -> Vec<SharedString> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| SharedString::from(line.to_string()))
        .collect::<Vec<_>>();
    if lines.is_empty() { vec![SharedString::default()] } else { lines }
}

/// Thinking and tool calls: what the activity setting folds or hides. Edits
/// that carry a diff are results, so they stay.
fn is_activity(block: &Block) -> bool {
    match block {
        Block::Thought(_) => true,
        Block::Tool(tool) => tool.diffs.is_empty(),
        _ => false,
    }
}

/// Each run of consecutive activity blocks, as `start..end`.
fn activity_runs(blocks: &[Block]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, block) in blocks.iter().enumerate() {
        match (is_activity(block), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push(s..blocks.len());
    }
    runs
}

/// One line for a run of activity, in the order things first happened:
/// "Ran 5 commands, read 4 files", "Read md.rs, ran 8 commands, 1 failed".
fn activity_summary(blocks: &[Block]) -> String {
    #[derive(PartialEq)]
    enum Part {
        Thought,
        Tool(Kind),
    }
    let mut parts: Vec<(Part, usize)> = Vec::new();
    let mut read: Option<String> = None;
    let mut failed = 0;
    for block in blocks {
        let part = match block {
            Block::Thought(_) => Part::Thought,
            Block::Tool(tool) => {
                if tool.status == Status::Failed {
                    failed += 1;
                }
                match tool.kind {
                    Kind::Think => Part::Thought,
                    Kind::Read => {
                        read = tool.locations.first().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                        Part::Tool(Kind::Read)
                    }
                    Kind::Search | Kind::Execute | Kind::Fetch => Part::Tool(tool.kind),
                    _ => Part::Tool(Kind::Other),
                }
            }
            _ => continue,
        };
        match parts.iter_mut().find(|(p, _)| *p == part) {
            Some((_, n)) => *n += 1,
            None => parts.push((part, 1)),
        }
    }
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut words: Vec<String> = parts
        .into_iter()
        .map(|(part, n)| match part {
            Part::Thought => "thought".to_string(),
            Part::Tool(Kind::Read) => match (n, &read) {
                (1, Some(name)) => format!("read {name}"),
                _ => format!("read {}", count(n, "file", "files")),
            },
            Part::Tool(Kind::Search) => format!("ran {}", count(n, "search", "searches")),
            Part::Tool(Kind::Execute) => format!("ran {}", count(n, "command", "commands")),
            Part::Tool(Kind::Fetch) => format!("fetched {}", count(n, "page", "pages")),
            Part::Tool(_) => format!("used {}", count(n, "tool", "tools")),
        })
        .collect();
    if failed > 0 {
        words.push(format!("{failed} failed"));
    }
    let mut line = words.join(", ");
    if let Some(first) = line.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    line
}

fn dropped_paths(paths: &ExternalPaths, root: Option<&Path>, mentions: &mut Vec<FileMention>, window: &Window) -> String {
    let added = paths
        .paths()
        .iter()
        .map(|path| super::mentions::fit(super::mentions::path_mention(path, root), window))
        .collect::<Vec<_>>();
    let mut text = added.iter().map(|mention| mention.token.as_str()).collect::<Vec<_>>().join(" ");
    mentions.extend(added);
    if !text.is_empty() {
        text.push(' ');
    }
    text
}

/// What a mention becomes when the composer's text leaves it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mentions {
    /// Its name, as in a thread's title.
    Names,
    /// Its path, so the agent knows where the file is.
    Paths,
    /// A chip link the transcript draws as it looked in the composer.
    Chips,
}

/// Backslash-escape what Markdown would otherwise read as syntax.
fn escape_markdown(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>') {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

pub(super) fn replace_file_mentions(text: &str, mentions: &[FileMention], mode: Mentions) -> String {
    let mut text = text.to_string();
    for mention in mentions {
        let with = match mode {
            Mentions::Names => mention.label.clone(),
            Mentions::Paths => mention.sent.clone(),
            Mentions::Chips => format!("[{}](<chip:{}|{}>)", escape_markdown(&mention.label), mention.icon, mention.reference.replace(['<', '>', '\n'], "")),
        };
        text = text.replacen(&mention.token, &with, 1);
    }
    // If a token was partially edited, do not leak its invisible delimiters.
    text.replace(FILE_MARKER, "")
}

pub(super) fn lsp_position(text: &str, offset: usize) -> Position {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let column = before.rsplit_once('\n').map_or(before, |(_, line)| line).encode_utf16().count() as u32;
    Position::new(line, column)
}

/// Uses the input's document-color hook to paint dropped basenames like inline
/// chips. The full path never enters the rendered text.
pub(super) struct FileMentionColors(Rc<RefCell<Vec<FileMention>>>);

impl FileMentionColors {
    pub(super) fn new(mentions: Rc<RefCell<Vec<FileMention>>>) -> Self {
        Self(mentions)
    }
}

impl DocumentColorProvider for FileMentionColors {
    fn document_colors(&self, text: &Rope, _: &mut Window, cx: &mut App) -> Task<anyhow::Result<Vec<ColorInformation>>> {
        let text = text.to_string();
        let color = palette(cx).hover;
        let color = Color {
            red: ((color >> 16) & 0xff) as f32 / 255.,
            green: ((color >> 8) & 0xff) as f32 / 255.,
            blue: (color & 0xff) as f32 / 255.,
            alpha: 1.,
        };
        let mut colors = Vec::new();
        let mut seen = HashSet::new();
        for mention in self.0.borrow().iter() {
            if !seen.insert(mention.token.clone()) {
                continue;
            }
            for (start, _) in text.match_indices(&mention.token) {
                let start = start + FILE_MARKER.len();
                let end = start + mention.token.len() - FILE_MARKER.len() * 2;
                colors.push(ColorInformation { range: Range::new(lsp_position(&text, start), lsp_position(&text, end)), color });
            }
        }
        Task::ready(Ok(colors))
    }
}

/// A quiet control in the composer's footer: glyph and current value.
fn option(colors: Palette, id: &'static str, glyph: impl Into<Icon>, text: impl Into<SharedString>, tip: &'static str) -> Stateful<Div> {
    row()
        .id(id)
        .h(px(24.))
        .max_w(px(200.))
        .px(px(7.))
        .gap(px(5.))
        .rounded(px(5.))
        .text_size(px(crate::theme::scaled_ui(12.)))
        .text_color(rgb(colors.muted))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
        .child(icon(glyph).size(px(13.)))
        .child(div().truncate().child(text.into()))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
}

/// A menu of everything a session setting can be, the current value checked.
/// Descriptions are left out where the names say enough, as for models.
pub(super) fn choices(setting: &Setting, weak: WeakEntity<Workspace>, describe: bool) -> Vec<Entry> {
    let mut entries = vec![Entry::Label(setting.name.clone().into())];
    for choice in &setting.choices {
        let (weak, id, value) = (weak.clone(), setting.id.clone(), choice.id.clone());
        let mut item = Item::new(choice.name.clone()).checked(choice.id == setting.current).on_click(move |_, cx| {
            let _ = weak.update(cx, |s, cx| s.set_setting(id.clone(), value.clone(), cx));
        });
        if describe && let Some(detail) = &choice.detail {
            item = item.description(detail.clone());
        }
        entries.push(Entry::Item(item));
    }
    entries
}

impl Workspace {
    fn insert_dropped_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.current_project().path.as_deref();
        let text = dropped_paths(paths, root, &mut self.file_mentions.borrow_mut(), window);
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |input, cx| {
            // No range means the active selection, or the caret when it is empty.
            input.replace_text_in_range(None, &text, window, cx);
            input.focus(window, cx);
        });
    }

    /// Put a snippet's chip at the composer's caret, on the chat tab.
    pub(super) fn insert_mention(&mut self, added: FileMention, window: &mut Window, cx: &mut Context<Self>) {
        let mut added = super::mentions::fit(added, window);
        {
            // Snippets with the same label would share a token and could swap places when sent.
            let mentions = self.file_mentions.borrow();
            let label = added.label.clone();
            let mut n = 2;
            while mentions.iter().any(|m| m.token == added.token && m.sent != added.sent) {
                added = super::mentions::fit(super::mentions::relabel(&added, &format!("{label} ({n})")), window);
                n += 1;
            }
        }
        let token = format!("{} ", added.token);
        self.file_mentions.borrow_mut().push(added);
        self.active = 0;
        self.composer.update(cx, |input, cx| {
            input.replace_text_in_range(None, &token, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// ⌘ L: add the selected lines of a file or diff, or else the transcript's
    /// selected text, to the message being written.
    pub(super) fn add_to_chat(&mut self, _: &AddToChat, window: &mut Window, cx: &mut Context<Self>) {
        let added = match self.line_selection.take() {
            Some(lines) => self.lines_mention(&lines),
            None => self.selection.borrow().text().filter(|t| !t.trim().is_empty()).map(|t| super::mentions::quote_mention(&t)),
        };
        self.selection.borrow_mut().clear();
        match added {
            Some(added) => self.insert_mention(added, window, cx),
            None => cx.notify(),
        }
    }

    /// The floating "Add to chat" button over a selection.
    pub(super) fn add_to_chat_button(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = palette(cx);
        row()
            .id(id)
            .occlude()
            .h(px(26.))
            .px_2()
            .gap_2()
            .rounded(px(6.))
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .shadow_md()
            .cursor_pointer()
            .font_family(crate::theme::ui_font())
            .text_size(px(crate::theme::scaled_ui(12.)))
            .hover(|s| s.bg(rgb(colors.hover)))
            .child(icon(IconName::Plus).size(px(12.)))
            .child("Add to chat")
            .child(div().text_color(rgb(colors.quiet)).child("⌘ L"))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|s, _, window, cx| s.add_to_chat(&AddToChat, window, cx)))
    }

    /// What you sent, rendered as Markdown with mentions as chips, as it looked when typed.
    fn user_block(&self, colors: Palette, thread: usize, block: usize, message: &crate::model::Message, now: chrono::DateTime<chrono::Local>, on_link: &md::OnLink) -> Div {
        let bubble = div()
            .px_3()
            .py(px(9.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .child(md::render(
                SharedString::from(format!("user-{thread}-{block}")),
                &message.text,
                md::Breaks::Hard,
                colors,
                mono(),
                px(crate::theme::mono_size()),
                Some((self.selection.clone(), block)),
                Some(on_link.clone()),
            ));
        col()
            .mt_2()
            .gap_1()
            .child(bubble)
            .when_some(message.at, |s, at| s.child(meta(colors, clock(at, now)).px_1()))
    }

    fn queued_block(&self, colors: Palette, n: usize, text: &SharedString) -> Div {
        col()
            .mt_2()
            .px_3()
            .py(px(9.))
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .child(div().text_size(px(crate::theme::scaled_ui(11.))).text_color(rgb(colors.quiet)).child("Queued"))
            .child(md::render(SharedString::from(format!("queued-{n}")), text, md::Breaks::Hard, colors, mono(), px(crate::theme::mono_size()), None, None))
    }

    /// Thought progress stays one row tall. Each short vertical marker stands
    /// for one update; hovering a marker makes its update the persistent one.
    fn thought_block(&self, colors: Palette, thread: usize, block: usize, text: &str, cx: &mut Context<Self>) -> Div {
        let lines = thought_lines(text);
        let key = (thread, block);
        let selected = self
            .thought_selection
            .get(&key)
            .copied()
            .unwrap_or(lines.len() - 1)
            .min(lines.len() - 1);
        let mut markers = row().h(px(16.)).gap(px(2.)).flex_shrink_0();
        for i in 0..lines.len() {
            markers = markers.child(
                row()
                    .id(SharedString::from(format!("thought-{thread}-{block}-{i}")))
                    .w(px(5.))
                    .h_full()
                    .justify_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(2.))
                            .h_full()
                            .rounded_full()
                            .bg(rgb(if i == selected { colors.muted } else { colors.border })),
                    )
                    .on_mouse_move(cx.listener(move |s, _, _, cx| {
                        if s.thought_selection.get(&key).copied() != Some(i) {
                            s.thought_selection.insert(key, i);
                            cx.notify();
                        }
                    })),
            );
        }
        row()
            .h(px(25.))
            .gap(px(8.))
            .items_center()
            .text_size(px(crate::theme::scaled_ui(12.5)))
            .text_color(rgb(colors.muted))
            .child(markers)
            .child(one_line(lines[selected].clone(), 19.).flex_1().min_w_0())
    }

    /// A file path as a mono name and a quiet folder, relative to the project:
    /// as wide as it needs, up to `max`, past which it ends in an ellipsis.
    fn path_label(&self, window: &Window, colors: Palette, path: &Path, max: Pixels) -> Div {
        let relative = self.current_project().relative(path).to_string_lossy().into_owned();
        let (name, dir) = split_path(&relative);
        let shown = if dir.is_empty() { name.to_string() } else { format!("{name}  {dir}") };
        let width = (text_width_in(window, &shown, crate::theme::mono_size(), mono()) + px(2.)).min(max);
        let dir = (!dir.is_empty()).then(|| (SharedString::from(dir.to_string()), Hsla::from(rgb(colors.quiet))));
        line_with_detail(name.to_string(), dir, 18.).w(width).flex_shrink_0().font_family(mono()).text_size(px(crate::theme::mono_size()))
    }

    /// One line per tool call: what ran, on what, and how it went. A call that
    /// edited files gets a line per file instead, each opening its diff.
    fn tool_block(&self, window: &Window, colors: Palette, thread: usize, index: usize, tool: &ToolCall, cx: &mut Context<Self>) -> Div {
        let (glyph, verb): (Icon, _) = match tool.kind {
            Kind::Think => (Glyph::Brain.into(), "Thought"),
            Kind::Read => (Glyph::FileText.into(), "Read"),
            Kind::Search => (IconName::Search.into(), "Searched"),
            Kind::Execute => (Glyph::Terminal.into(), "Ran"),
            Kind::Edit => (Glyph::Pencil.into(), "Edited"),
            Kind::Delete => (IconName::Delete.into(), "Deleted"),
            Kind::Move => (Glyph::FileText.into(), "Moved"),
            Kind::Fetch => (IconName::Globe.into(), "Fetched"),
            Kind::Other => (Glyph::Plug.into(), "Used"),
        };
        let mark = |glyph: Icon| match tool.status {
            Status::Running => spinner(colors).into_any_element(),
            Status::Failed => icon(IconName::CircleX).size(px(13.)).text_color(rgb(colors.red)).into_any_element(),
            Status::Pending => icon(glyph).size(px(13.)).text_color(rgb(colors.quiet)).into_any_element(),
            Status::Done => icon(glyph)
                .size(px(13.))
                .text_color(rgb(if tool.kind == Kind::Edit { colors.amber } else { colors.quiet }))
                .into_any_element(),
        };
        if !tool.diffs.is_empty() {
            let mut lines = col();
            for (i, diff) in tool.diffs.iter().enumerate() {
                let (adds, dels) = crate::model::count(diff.old.as_deref().unwrap_or(""), &diff.new);
                let path = diff.path.clone();
                lines = lines.child(
                    row()
                        .id(("edit", index * 1000 + i))
                        .h(px(28.))
                        .mx(px(-8.))
                        .px_2()
                        .gap(px(7.))
                        .rounded(px(5.))
                        .text_size(px(crate::theme::scaled_ui(12.5)))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(colors.hover)))
                        .child(mark(Glyph::Pencil.into()))
                        .child(div().text_color(rgb(colors.muted)).child(match (tool.status, diff.old.is_some()) {
                            (Status::Pending, _) => "Wants to edit",
                            (Status::Failed, _) => "Didn't edit",
                            (_, false) => "Created",
                            _ => "Edited",
                        }))
                        .child(self.path_label(window, colors, &diff.path, px(COLUMN - 240.)))
                        .child(stat(colors, adds as u32, dels as u32))
                        .on_mouse_down(MouseButton::Right, {
                            let path = path.clone();
                            cx.listener(move |s, e: &MouseDownEvent, window, cx| s.diff_menu(path.clone(), e.position, window, cx))
                        })
                        .on_click(cx.listener(move |s, _, _, cx| s.open(TabKind::Diff(path.clone()), cx))),
                );
            }
            return lines;
        }
        // Command output and the like: its last line says how the call went.
        let output = tool.output.trim_end();
        let detail = output.lines().last().unwrap_or_default().trim().to_string();
        let key = (thread, index);
        let open = self.open_tools.contains(&key);
        // Thoughts are prose; everything else names a command, file or query.
        let prose = tool.kind == Kind::Think;
        let group = SharedString::from(format!("tool-{thread}-{index}"));
        let line = row()
            .id(("tool", index))
            .group(group.clone())
            .h(px(26.))
            .mx(px(-8.))
            .px_2()
            .gap(px(7.))
            .rounded(px(5.))
            .text_size(px(crate::theme::scaled_ui(12.5)))
            .text_color(rgb(colors.muted))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(colors.hover)))
            .when(open, |s| s.bg(rgb(colors.hover)))
            .child(mark(glyph))
            .child(div().flex_shrink_0().child(verb))
            .child(
                line_with_detail(
                    tool.title.clone(),
                    (!detail.is_empty()).then(|| (SharedString::from(detail), Hsla::from(rgb(colors.quiet)))),
                    18.,
                )
                .flex_1()
                .min_w_0()
                .text_color(rgb(colors.text))
                .when(!prose, |s| s.font_family(mono()).text_size(px(crate::theme::mono_size()))),
            )
            // The chevron shows on hover, and stays while the call is open.
            .child(
                div()
                    .flex_shrink_0()
                    .when(!open, |s| s.opacity(0.).group_hover(group, |s| s.opacity(1.)))
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(12.)).text_color(rgb(colors.quiet))),
            )
            .on_click(cx.listener(move |s, _, _, cx| {
                if !s.open_tools.remove(&key) {
                    s.open_tools.insert(key);
                }
                cx.notify();
            }));
        if !open {
            return div().child(line);
        }
        // Opened: the whole command, wrapped, and what it printed.
        let mut lines = output.lines().map(super::syntax::expand_tabs).collect::<Vec<_>>();
        let hidden = lines.len().saturating_sub(TOOL_OUTPUT_LINES);
        lines.drain(..hidden);
        col().child(line).child(
            col()
                .mt_1()
                .mb_2()
                .ml(px(20.))
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(colors.row_border))
                .bg(rgb(colors.panel))
                .overflow_hidden()
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_size(px(crate::theme::scaled_ui(12.)))
                        .line_height(relative(1.5))
                        .when(!prose, |s| s.font_family(mono()).text_size(px(crate::theme::mono_size())))
                        .child(tool.title.clone()),
                )
                .when(!lines.is_empty(), |s| {
                    s.child(
                        div()
                            .id(("tool-output", index))
                            .max_h(px(260.))
                            .overflow_scroll()
                            .px_3()
                            .py_2()
                            .border_t_1()
                            .border_color(rgb(colors.row_border))
                            .font_family(mono())
                            .text_size(px(crate::theme::mono_size()))
                            .line_height(relative(1.5))
                            .text_color(rgb(colors.muted))
                            .when(hidden > 0, |s| {
                                s.child(div().text_color(rgb(colors.quiet)).child(format!("… {hidden} earlier lines")))
                            })
                            .children(lines.into_iter().map(|l| div().whitespace_nowrap().min_h(px(4.)).child(l))),
                    )
                }),
        )
    }

    /// A run of thinking and tool calls folded into one line; clicking it
    /// shows them in place.
    fn summary_line(&self, colors: Palette, thread: usize, start: usize, line: String, cx: &mut Context<Self>) -> Div {
        let key = (thread, start);
        let open = self.open_summaries.contains(&key);
        div().child(
            row()
                .id(SharedString::from(format!("summary-{thread}-{start}")))
                .h(px(26.))
                .gap(px(4.))
                .text_size(px(crate::theme::scaled_ui(13.)))
                .text_color(rgb(colors.muted))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(colors.text)))
                .child(div().whitespace_nowrap().child(line))
                .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(12.)).flex_shrink_0())
                .on_click(cx.listener(move |s, _, _, cx| {
                    if !s.open_summaries.remove(&key) {
                        s.open_summaries.insert(key);
                    }
                    cx.notify();
                })),
        )
    }

    /// The agent asking before it acts: the question and one button per answer.
    fn permission_block(&self, colors: Palette, permission: &Permission, cx: &mut Context<Self>) -> Div {
        let id = permission.id;
        let mut actions = row().gap_2();
        for (i, Answer { id: answer, name, allow }) in permission.answers.iter().enumerate() {
            let (answer, name, allow) = (answer.clone(), name.clone(), *allow);
            actions = actions.child(
                row()
                    .id(SharedString::from(format!("answer-{id}-{i}")))
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(5.))
                    .text_size(px(crate::theme::scaled_ui(12.5)))
                    .cursor_pointer()
                    .map(|s| {
                        // The first way to say yes is the one to reach for.
                        if allow && i == 0 {
                            s.bg(rgb(colors.blue))
                                .text_color(rgb(colors.primary_text))
                                .font_weight(FontWeight::MEDIUM)
                                .hover(|s| s.opacity(0.9))
                        } else {
                            s.border_1().border_color(rgb(colors.border)).hover(|s| s.bg(rgb(colors.hover)))
                        }
                    })
                    .child(name.clone())
                    .on_click(cx.listener(move |s, _, _, cx| s.answer(id, Some((answer.clone(), name.clone())), cx))),
            );
        }
        let waiting = permission.chosen.is_none();
        card(colors)
            .my_1()
            .when(waiting, |s| s.border_color(rgb(colors.blue)))
            .child(
                row()
                    .min_h(px(40.))
                    .pl_3()
                    .pr_2()
                    .py(px(6.))
                    .gap_2()
                    .child(icon(Glyph::ShieldCheck).text_color(rgb(if waiting { colors.blue } else { colors.quiet })))
                    .child(div().flex_1().min_w_0().truncate().when(!waiting, |s| s.text_color(rgb(colors.muted))).child(permission.title.clone()))
                    .map(|s| match &permission.chosen {
                        None => s.child(actions),
                        Some(chosen) => s.child(label(colors, chosen.clone()).pr_1()),
                    }),
            )
    }

    /// What a finished turn left behind, with the way into reviewing it.
    fn changes_card(&self, colors: Palette, files: usize, adds: usize, dels: usize, cx: &mut Context<Self>) -> Div {
        card(colors).mt_2().child(
            row()
                .h(px(40.))
                .pl_3()
                .pr_2()
                .gap_2()
                .child(icon(Glyph::FileDiff).text_color(rgb(colors.amber)))
                .child(div().font_weight(FontWeight::MEDIUM).child(format!("{files} file{} changed", if files == 1 { "" } else { "s" })))
                .child(stat(colors, adds as u32, dels as u32))
                .child(div().flex_1())
                .child(
                    row()
                        .id("review-changes")
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .text_size(px(crate::theme::scaled_ui(12.5)))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(colors.hover)))
                        .child("Review changes")
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.side = Side::Changes;
                            s.side_open = true;
                            cx.notify();
                        })),
                ),
        )
    }

    fn composer_view(&self, colors: Palette, thread: &Thread, window: &Window, cx: &mut Context<Self>) -> Div {
        let empty = self.composer.read(cx).value().trim().is_empty();
        let busy = thread.state.busy();
        // The composer sits low in the window, so its menus open upward.
        let agent = self.popover_trigger(
            "agent",
            option(colors, "agent-button", self.agent_glyph(thread.agent), self.agent_name(thread).to_string(), "Agent"),
            Corner::BottomRight,
            260.,
            None,
            |s, weak, _| {
                // An agent's session holds the conversation, so a thread stays with the one it started on.
                let started = s.current_thread().is_some_and(|t| !t.blocks.is_empty());
                let current = s.current_thread().map(|t| t.agent);
                let mut entries = vec![Entry::Label(if started { "Agent · fixed once a thread starts" } else { "Agent" }.into())];
                for (i, spec) in s.settings.agents.iter().enumerate() {
                    let weak = weak.clone();
                    entries.push(Entry::Item(
                        Item::new(spec.name.clone())
                            .icon(super::chrome::agent_glyph(Some(spec)))
                            .checked(current == Some(i))
                            .disabled(started && current != Some(i))
                            .on_click(move |_, cx| {
                                let _ = weak.update(cx, |s, cx| s.set_agent(i, cx));
                            }),
                    ));
                }
                entries
            },
            cx,
        );
        // Whatever the agent lets a session change shows up here, and applies mid-thread.
        let setting = |role: Role, anchor: &'static str, button: &'static str, glyph: Icon, corner: Corner, tip: &'static str, describe: bool, cx: &mut Context<Self>| {
            let setting = thread.settings.iter().find(|s| s.role == role)?;
            Some(self.popover_trigger(
                anchor,
                option(colors, button, glyph, setting.current_name().to_string(), tip),
                corner,
                300.,
                None,
                move |s, weak, _| s.setting(role).map(|setting| choices(setting, weak, describe)).unwrap_or_default(),
                cx,
            ))
        };
        let mode = setting(Role::Mode, "mode", "mode-button", Glyph::ShieldCheck.into(), Corner::BottomLeft, "How much the agent may do before asking", true, cx);
        let model = setting(Role::Model, "model", "model-button", Glyph::Sparkle.into(), Corner::BottomRight, "Model", false, cx);
        // Effort is a scale, so it gets a slider rather than a list.
        let effort = thread.settings.iter().find(|s| s.role == Role::Effort).map(|setting| {
            self.anchor(
                "effort",
                option(colors, "effort-button", Glyph::Gauge, setting.current_name().to_string(), "Reasoning effort")
                    .when(self.effort_open, |s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
                    .on_click(cx.listener(|s, _, _, cx| {
                        if s.dismissed.take().is_some_and(|(a, at)| a == "effort" && at.elapsed() < Duration::from_millis(400)) {
                            return;
                        }
                        s.effort_open = !s.effort_open;
                        cx.notify();
                    })),
            )
        });
        let follow_up = self.settings.follow_up_mode;
        let supports_steering = thread.supports_steering;
        let stop = busy.then(|| {
            row()
                .id("stop")
                .size(px(26.))
                .ml_1()
                .flex_shrink_0()
                .justify_center()
                .rounded_full()
                .bg(rgb(colors.text))
                .text_color(rgb(colors.background))
                .cursor_pointer()
                .child(icon(Glyph::Stop).size(px(12.)))
                .tooltip(|w, cx| Tooltip::new("Stop · esc").build(w, cx))
                .on_click(cx.listener(|s, _, _, cx| s.stop(cx)))
        });
        let send = row()
            .id("send")
            .size(px(26.))
            .ml_1()
            .flex_shrink_0()
            .justify_center()
            .rounded_full()
            .map(|s| {
                if empty {
                    s.bg(rgb(colors.hover)).text_color(rgb(colors.quiet))
                } else {
                    s.bg(rgb(colors.blue)).text_color(rgb(colors.primary_text)).cursor_pointer()
                }
            })
            .child(icon(IconName::ArrowUp))
            .tooltip(move |w, cx| {
                let tip = if !busy {
                    "Send · ↵"
                } else if follow_up == FollowUpMode::Queue || !supports_steering {
                    "Queue message · ↵"
                } else {
                    "Steer agent · ↵"
                };
                Tooltip::new(tip).build(w, cx)
            })
            .on_click(cx.listener(|s, _, w, cx| s.submit(&Submit, w, cx)));
        // Docked across the bottom of the chat, edge to edge, under a single rule.
        let composer = col()
            .w_full()
            .min_h(px(112.))
            .border_t_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(rgb(colors.blue_tint)).border_color(rgb(colors.blue)))
            .on_drop(cx.listener(|s, paths: &ExternalPaths, window, cx| {
                s.insert_dropped_paths(paths, window, cx);
            }))
            .child(
                div().key_context("Composer").flex_1().px(px(14.)).pt(px(12.)).pb(px(6.)).child(
                    // No padding of its own, so its text starts at its top-left and chips land true.
                    div().relative().child(Input::new(&self.composer).appearance(false).p_0()).child(self.chip_overlay(colors)),
                ),
            )
            .child(
                row()
                    .h(px(36.))
                    .px(px(8.))
                    .gap(px(2.))
                    .children(mode)
                    .when(thread.session.is_some() && !thread.ready, |s| {
                        s.child(row().px(px(7.)).gap(px(6.)).text_size(px(crate::theme::scaled_ui(12.))).text_color(rgb(colors.quiet)).child(spinner(colors)).child("Connecting…"))
                    })
                    .child(div().flex_1())
                    .child(agent)
                    .children(model)
                    .children(effort)
                    .children(stop)
                    .child(send),
            );
        let composer = self.with_command_menu(composer, colors, window, cx);
        self.with_mention_menu(composer, colors, window, cx)
    }

    /// Clicking a link in the transcript: web links go to the browser, paths to
    /// the project's files (agents link them as `src/x.rs`, `src/x.rs:12` or
    /// `file:///…`), opened in a tab beside the chat.
    fn link_opener(&self, cx: &mut Context<Self>) -> md::OnLink {
        let root = self.current_project().path.clone();
        let this = cx.entity().downgrade();
        std::rc::Rc::new(move |url: &str, _window: &mut Window, cx: &mut App| {
            if md::is_web(url) {
                return cx.open_url(url);
            }
            match link_path(url, root.as_deref()) {
                Some((path, lines)) if path.is_file() => {
                    this.update(cx, |s, cx| s.open_at(path, lines, cx)).ok();
                }
                Some((path, _)) if path.exists() => cx.reveal_path(&path),
                // Some other app's scheme, such as `vscode://` or `zed://`.
                _ if url.contains("://") => cx.open_url(url),
                _ => {}
            }
        })
    }

    pub(super) fn chat_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let Some(thread) = self.current_thread() else {
            return empty(colors, Glyph::Message, "No thread selected", "Pick a thread on the left, or start a new one with ⌘ N.").into_any_element();
        };
        // The column gets its width in pixels, from the last frame's bounds. With a
        // percentage width GPUI sized the column before its text had wrapped, so the
        // scroll range stopped short of the last lines.
        let available = self.transcript.bounds().size.width;
        // Where the selection was last painted, for its "Add to chat" button,
        // before text records where it lands this time.
        let selection_ends = {
            let selection = self.selection.borrow();
            (!selection.is_empty() && !selection.dragging).then(|| selection.ends())
        };
        self.selection.borrow_mut().forget_layout();
        let mut list = col().px_5().pt_3().pb_5().gap(px(6.)).map(|s| {
            if available > px(0.) { s.w(available.min(px(COLUMN))) } else { s.w_full().max_w(px(COLUMN)) }
        });
        let on_link = self.link_opener(cx);
        let now = chrono::Local::now();
        let mode = self.settings.activity;
        // Runs of thinking and tool calls fold away once something follows
        // them; the run the agent is still in stays live.
        let mut folds: HashMap<usize, usize> = HashMap::new();
        if mode != ActivityMode::Visible {
            for run in activity_runs(&thread.blocks) {
                if run.end < thread.blocks.len() || !thread.is_active() || mode == ActivityMode::Hidden {
                    folds.insert(run.start, run.end);
                }
            }
        }
        let mut skip_until = 0;
        for (i, block) in thread.blocks.iter().enumerate() {
            if i < skip_until {
                continue;
            }
            if let Some(&end) = folds.get(&i) {
                if mode == ActivityMode::Hidden {
                    skip_until = end;
                    continue;
                }
                let line = activity_summary(&thread.blocks[i..end]);
                list = list.child(self.summary_line(colors, thread.id, i, line, cx));
                if !self.open_summaries.contains(&(thread.id, i)) {
                    skip_until = end;
                    continue;
                }
            }
            list = list.child(match block {
                Block::User(message) => self.user_block(colors, thread.id, i, message, now, &on_link),
                Block::TurnEnded { at, worked_ms } => {
                    row().h(px(22.)).child(meta(colors, format!("Worked for {} · {}", duration(*worked_ms), clock(*at, now))))
                }
                Block::Text(text) => div().py(px(5.)).child(md::render(
                    SharedString::from(format!("md-{}-{i}", thread.id)),
                    text.trim(),
                    md::Breaks::Hard,
                    colors,
                    mono(),
                    px(crate::theme::mono_size()),
                    Some((self.selection.clone(), i)),
                    Some(on_link.clone()),
                )),
                Block::Thought(text) => self.thought_block(colors, thread.id, i, text, cx),
                Block::Tool(tool) => self.tool_block(window, colors, thread.id, i, tool, cx),
                Block::Permission(permission) => self.permission_block(colors, permission, cx),
                Block::Notice(text) => row()
                    .items_start()
                    .py(px(4.))
                    .gap_2()
                    .text_size(px(crate::theme::scaled_ui(12.5)))
                    .text_color(rgb(colors.muted))
                    .child(row().h(px(19.)).child(icon(IconName::Info).size(px(13.)).text_color(rgb(colors.quiet))))
                    .child(div().flex_1().min_w_0().line_height(relative(1.5)).child(text.clone())),
            });
        }
        for (n, queued) in thread.queued.iter().enumerate() {
            list = list.child(self.queued_block(colors, n, &queued.shown));
        }
        if thread.state == ThreadState::Running {
            list = list.child(
                row()
                    .h(px(28.))
                    .gap_2()
                    .text_size(px(crate::theme::scaled_ui(12.5)))
                    .text_color(rgb(colors.muted))
                    .child(spinner(colors))
                    .child(if thread.ready { "Working…" } else { "Starting the agent…" })
                    .child(div().text_color(rgb(colors.quiet)).child("esc to interrupt")),
            );
        }
        if let Some(problem) = &thread.problem {
            list = list.child(
                row()
                    .items_start()
                    .py(px(4.))
                    .gap_2()
                    .text_size(px(crate::theme::scaled_ui(12.5)))
                    .child(row().h(px(19.)).child(icon(IconName::TriangleAlert).size(px(13.)).text_color(rgb(colors.red))))
                    .child(div().flex_1().min_w_0().line_height(relative(1.5)).child(problem.clone())),
            );
        }
        if thread.state == ThreadState::Done {
            let changes = thread.changes();
            if !changes.is_empty() {
                let (adds, dels) = changes.iter().fold((0, 0), |(a, d), c| (a + c.adds, d + c.dels));
                list = list.child(self.changes_card(colors, changes.len(), adds, dels, cx));
            }
        }
        let transcript = if thread.blocks.is_empty() && thread.problem.is_none() {
            let project = self.current_project();
            empty(
                colors,
                Glyph::Sparkle,
                match project.path {
                    Some(_) => format!("What should we build in {}?", project.name),
                    None => "What's on your mind?".to_string(),
                },
                "Describe a change, paste an error, or ask how something works.",
            )
            .into_any_element()
        } else {
            let transcript = div()
                .id("transcript")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.transcript)
                // A sideways swipe belongs to the code block under it. Unrestricted,
                // gpui turns a purely sideways delta into an up-and-down one here,
                // and the transcript jumps away from the block being scrolled.
                .map(|mut d| {
                    d.style().restrict_scroll_to_axis = Some(true);
                    d
                })
                // Any scroll up lets go of the end; reaching it again takes hold.
                .on_scroll_wheel(cx.listener(|s, e: &ScrollWheelEvent, _, _| {
                    let dy = e.delta.pixel_delta(px(16.)).y;
                    if dy > px(0.) {
                        s.follow = false;
                    } else if dy < px(0.) {
                        let end = -s.transcript.max_offset().height;
                        s.follow = s.transcript.offset().y + dy <= end + px(2.);
                    }
                }))
                // Dragging over text selects it; ⌘ C copies it.
                .on_mouse_down(MouseButton::Left, cx.listener(|s, e: &MouseDownEvent, window, cx| {
                    s.line_selection = None;
                    if s.selection.borrow_mut().press(e.position, e.click_count, e.modifiers.shift) {
                        // Away from the composer, so ⌘ C reaches the transcript.
                        s.focus.focus(window);
                    }
                    cx.notify();
                }))
                .on_mouse_move(cx.listener(|s, e: &MouseMoveEvent, _, cx| {
                    if e.pressed_button == Some(MouseButton::Left) && s.selection.borrow_mut().drag(e.position) {
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| {
                    s.selection.borrow_mut().dragging = false;
                    cx.notify();
                }))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, cx| {
                    s.selection.borrow_mut().dragging = false;
                    cx.notify();
                }))
                .child(list.mx_auto());
            // The scrollbar moves the view without wheel events: a drag that ends
            // off the end lets go of it, and one that ends at the end takes hold.
            // A click that doesn't move the view leaves `follow` as it was.
            let release = |s: &mut Self, cx: &mut Context<Self>| {
                let Some(pressed) = s.holding_transcript.take() else { return };
                let now = s.transcript.offset().y;
                if now != pressed {
                    s.follow = now <= -s.transcript.max_offset().height + px(2.);
                }
                if s.follow {
                    s.transcript.scroll_to_bottom();
                }
                cx.notify();
            };
            div()
                .relative()
                .size_full()
                .capture_any_mouse_down(cx.listener(|s, _, _, _| s.holding_transcript = Some(s.transcript.offset().y)))
                .capture_any_mouse_up(cx.listener(move |s, _, _, cx| release(s, cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(move |s, _, _, cx| release(s, cx)))
                .child(transcript)
                .vertical_scrollbar(&self.transcript)
                .into_any_element()
        };
        col()
            .size_full()
            .child(div().flex_1().min_h_0().child(transcript).when_some(selection_ends, |s, (start, end)| {
                // Below the selection's last line; above its first when the end is
                // scrolled away; in the transcript's corner when both are.
                let view = self.transcript.bounds();
                let visible = |p: &Point<Pixels>| p.y >= view.top() && p.y <= view.bottom();
                let (corner, at) = match (end.filter(visible), start.filter(visible)) {
                    (Some(end), _) => (Corner::TopLeft, point(end.x, end.y + px(6.))),
                    (None, Some(start)) => (Corner::BottomLeft, point(start.x, start.y - px(6.))),
                    (None, None) => (Corner::BottomRight, view.bottom_right() - point(px(16.), px(12.))),
                };
                s.child(
                    deferred(
                        anchored()
                            .anchor(corner)
                            .position(at)
                            .snap_to_window_with_margin(px(8.))
                            .child(self.add_to_chat_button("add-selection", cx)),
                    )
                    .with_priority(1),
                )
            }))
            .child(div().flex_shrink_0().w_full().child(self.composer_view(colors, thread, window, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Mentions, activity_runs, activity_summary, replace_file_mentions, thought_lines};
    use crate::{agent::{Kind, Status}, model::{Block, ToolCall}};

    fn tool(kind: Kind, status: Status, file: Option<&str>) -> Block {
        Block::Tool(ToolCall {
            id: String::new(),
            title: "t".into(),
            kind,
            status,
            diffs: vec![],
            output: String::new(),
            locations: file.map(|f| vec![f.into()]).unwrap_or_default(),
        })
    }

    #[test]
    fn activity_runs_split_at_prose() {
        let blocks = vec![
            Block::Thought("Planning".into()),
            tool(Kind::Read, Status::Done, None),
            Block::Text("Found it".into()),
            tool(Kind::Execute, Status::Done, None),
        ];
        assert_eq!(activity_runs(&blocks), [0..2, 3..4]);
    }

    #[test]
    fn summaries_read_like_a_sentence() {
        let run = vec![
            tool(Kind::Execute, Status::Done, None),
            tool(Kind::Read, Status::Done, Some("/a/b.rs")),
            tool(Kind::Read, Status::Done, Some("/a/c.rs")),
            tool(Kind::Execute, Status::Failed, None),
        ];
        assert_eq!(activity_summary(&run), "Ran 2 commands, read 2 files, 1 failed");
        assert_eq!(activity_summary(&run[1..2]), "Read b.rs");
        assert_eq!(activity_summary(&[Block::Thought("x".into())]), "Thought");
    }
    use super::super::mentions::path_mention as file_mention;
    use std::path::Path;

    #[test]
    fn file_links_resolve_against_the_project() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let md = root.join("src/workspace/md.rs");
        for (url, lines) in [
            ("src/workspace/md.rs", None),
            ("src/workspace/md.rs:12", Some((12, 12))),
            ("src/workspace/md.rs:12:4", Some((12, 12))),
            ("src/workspace/md.rs:12-20", Some((12, 20))),
            ("src/workspace/md.rs#L12", Some((12, 12))),
            ("src/workspace/md.rs#L12-L20", Some((12, 20))),
        ] {
            assert_eq!(super::link_path(url, Some(root)), Some((md.clone(), lines)), "{url}");
        }
        let file_url = format!("file://{}#L3", md.display());
        assert_eq!(super::link_path(&file_url, None), Some((md.clone(), Some((3, 3)))));
        assert_eq!(super::link_path("/tmp/a%20b.txt", None), Some((std::path::PathBuf::from("/tmp/a b.txt"), None)));
        assert_eq!(super::link_path("zed://file", Some(root)), None);
    }
    #[test]
    fn dropped_project_file_shows_only_its_name_and_sends_its_path() {
        let mention = file_mention(Path::new("/project/src/main.rs"), Some(Path::new("/project")));
        assert!(!mention.token.contains("src/"));
        assert!(mention.token.contains("main.rs"));
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], Mentions::Names), "main.rs");
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], Mentions::Paths), "@src/main.rs");
        let chip = replace_file_mentions(&mention.token, &[mention.clone()], Mentions::Chips);
        assert!(chip.starts_with("[main.rs](<chip:") && chip.ends_with("|@src/main.rs>)"));
    }

    #[test]
    fn dropped_external_file_sends_its_absolute_path() {
        let mention = file_mention(Path::new("/tmp/example.txt"), Some(Path::new("/project")));
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], Mentions::Paths), "@/tmp/example.txt");
    }

    #[test]
    fn durations_read_briefly() {
        assert_eq!(super::duration(42_900), "42s");
        assert_eq!(super::duration(185_000), "3m 05s");
        assert_eq!(super::duration(4_320_000), "1h 12m");
    }

    #[test]
    fn thought_updates_ignore_spacing_lines() {
        let lines = thought_lines("Planning\n\n  Designing  \n\r\nRefining\n");
        assert_eq!(lines, ["Planning", "Designing", "Refining"]);
    }
}
