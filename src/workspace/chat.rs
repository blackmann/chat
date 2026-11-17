//! The pinned chat tab: the thread's transcript above the composer.
use super::ui::*;
use super::*;
use crate::{
    agent::{Answer, Kind, Status},
    icons::Glyph,
    menu::{Entry, Item},
    model::{Permission, ToolCall},
};
use gpui_component::{Icon, IconName, Rope, input::{DocumentColorProvider, Input}, tooltip::Tooltip};
use lsp_types::{Color, ColorInformation, Position, Range};

/// The transcript and composer share one readable column.
const COLUMN: f32 = 760.;
/// How much of a tool's output an opened tool call shows: the end, where results are.
const TOOL_OUTPUT_LINES: usize = 200;
/// Word joiner is zero-width but survives ordinary editing and copying, so it
/// can bind a visible basename to its full path without showing that path.
const FILE_MARKER: &str = "\u{2060}";

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

fn dropped_path(path: &Path, root: Option<&Path>) -> String {
    let shown = root
        .and_then(|root| path.strip_prefix(root).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(path);
    format!("@{}", shown.to_string_lossy())
}

fn file_mention(path: &Path, root: Option<&Path>) -> FileMention {
    let label = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    // Non-breaking spaces supply the horizontal padding inside the highlight.
    let token = format!("{FILE_MARKER}\u{a0}{label}\u{a0}{FILE_MARKER}");
    FileMention { token, label, path: dropped_path(path, root) }
}

fn dropped_paths(paths: &ExternalPaths, root: Option<&Path>, mentions: &mut Vec<FileMention>) -> String {
    let added = paths.paths().iter().map(|path| file_mention(path, root)).collect::<Vec<_>>();
    let mut text = added.iter().map(|mention| mention.token.as_str()).collect::<Vec<_>>().join(" ");
    mentions.extend(added);
    if !text.is_empty() {
        text.push(' ');
    }
    text
}

pub(super) fn replace_file_mentions(text: &str, mentions: &[FileMention], paths: bool) -> String {
    let mut text = text.to_string();
    for mention in mentions {
        text = text.replacen(&mention.token, if paths { &mention.path } else { &mention.label }, 1);
    }
    // If a token was partially edited, do not leak its invisible delimiters.
    text.replace(FILE_MARKER, "")
}

fn position(text: &str, offset: usize) -> Position {
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
                colors.push(ColorInformation { range: Range::new(position(&text, start), position(&text, end)), color });
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
        .text_size(px(12.))
        .text_color(rgb(colors.muted))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
        .child(icon(glyph).size(px(13.)))
        .child(div().truncate().child(text.into()))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
}

/// A menu of everything a session setting can be, the current value checked.
fn choices(setting: &Setting, weak: WeakEntity<Workspace>) -> Vec<Entry> {
    let mut entries = vec![Entry::Label(setting.name.clone().into())];
    for choice in &setting.choices {
        let (weak, id, value) = (weak.clone(), setting.id.clone(), choice.id.clone());
        let mut item = Item::new(choice.name.clone()).checked(choice.id == setting.current).on_click(move |_, cx| {
            let _ = weak.update(cx, |s, cx| s.set_setting(id.clone(), value.clone(), cx));
        });
        if let Some(detail) = &choice.detail {
            item = item.description(detail.clone());
        }
        entries.push(Entry::Item(item));
    }
    entries
}

impl Workspace {
    fn insert_dropped_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.current_project().path.as_deref();
        let text = dropped_paths(paths, root, &mut self.file_mentions.borrow_mut());
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |input, cx| {
            // No range means the active selection, or the caret when it is empty.
            input.replace_text_in_range(None, &text, window, cx);
            input.focus(window, cx);
        });
    }

    fn user_block(&self, colors: Palette, text: &SharedString) -> Div {
        div()
            .mt_2()
            .px_3()
            .py(px(9.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .line_height(relative(1.5))
            .child(text.clone())
    }

    fn queued_block(&self, colors: Palette, text: &SharedString) -> Div {
        col()
            .mt_2()
            .px_3()
            .py(px(9.))
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .child(div().text_size(px(11.)).text_color(rgb(colors.quiet)).child("Queued"))
            .child(div().line_height(relative(1.5)).child(text.clone()))
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
            .text_size(px(12.5))
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
        let width = (text_width_in(window, &shown, 12., mono()) + px(2.)).min(max);
        let dir = (!dir.is_empty()).then(|| (SharedString::from(dir.to_string()), Hsla::from(rgb(colors.quiet))));
        line_with_detail(name.to_string(), dir, 18.).w(width).flex_shrink_0().font_family(mono()).text_size(px(12.))
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
                        .text_size(px(12.5))
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
            .text_size(px(12.5))
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
                .when(!prose, |s| s.font_family(mono()).text_size(px(12.))),
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
        let mut lines = output.lines().map(|l| l.replace('\t', "    ")).collect::<Vec<_>>();
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
                        .text_size(px(12.))
                        .line_height(relative(1.5))
                        .when(!prose, |s| s.font_family(mono()))
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
                            .text_size(px(12.))
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
                    .text_size(px(12.5))
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
                        .text_size(px(12.5))
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
            option(colors, "agent-button", IconName::Bot, self.agent_name(thread).to_string(), "Agent"),
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
        let setting = |role: Role, anchor: &'static str, button: &'static str, glyph: Icon, corner: Corner, tip: &'static str, cx: &mut Context<Self>| {
            let setting = thread.settings.iter().find(|s| s.role == role)?;
            Some(self.popover_trigger(
                anchor,
                option(colors, button, glyph, setting.current_name().to_string(), tip),
                corner,
                300.,
                None,
                move |s, weak, _| s.setting(role).map(|setting| choices(setting, weak)).unwrap_or_default(),
                cx,
            ))
        };
        let mode = setting(Role::Mode, "mode", "mode-button", Glyph::ShieldCheck.into(), Corner::BottomLeft, "How much the agent may do before asking", cx);
        let model = setting(Role::Model, "model", "model-button", Glyph::Sparkle.into(), Corner::BottomRight, "Model", cx);
        let effort = setting(Role::Effort, "effort", "effort-button", Glyph::Gauge.into(), Corner::BottomRight, "Reasoning effort", cx);
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
        let composer = col()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(rgb(colors.blue)))
            .on_drop(cx.listener(|s, paths: &ExternalPaths, window, cx| {
                s.insert_dropped_paths(paths, window, cx);
            }))
            .child(div().key_context("Composer").px_1().pt_1().child(Input::new(&self.composer).appearance(false)))
            .child(
                row()
                    .h(px(36.))
                    .px(px(6.))
                    .gap(px(2.))
                    .children(mode)
                    .when(thread.session.is_some() && !thread.ready, |s| {
                        s.child(row().px(px(7.)).gap(px(6.)).text_size(px(12.)).text_color(rgb(colors.quiet)).child(spinner(colors)).child("Connecting…"))
                    })
                    .child(div().flex_1())
                    .child(agent)
                    .children(model)
                    .children(effort)
                    .children(stop)
                    .child(send),
            );
        self.with_command_menu(composer, colors, window, cx)
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
        let mut list = col().px_5().pt_3().pb_5().gap(px(6.)).map(|s| {
            if available > px(0.) { s.w(available.min(px(COLUMN))) } else { s.w_full().max_w(px(COLUMN)) }
        });
        for (i, block) in thread.blocks.iter().enumerate() {
            list = list.child(match block {
                Block::User(text) => self.user_block(colors, text),
                Block::Text(text) => div().py(px(5.)).child(md::render(
                    SharedString::from(format!("md-{}-{i}", thread.id)),
                    text.trim(),
                    md::Breaks::Hard,
                    colors,
                    mono(),
                    px(crate::theme::MONO_SIZE),
                )),
                Block::Thought(text) => self.thought_block(colors, thread.id, i, text, cx),
                Block::Tool(tool) => self.tool_block(window, colors, thread.id, i, tool, cx),
                Block::Permission(permission) => self.permission_block(colors, permission, cx),
                Block::Notice(text) => row()
                    .items_start()
                    .py(px(4.))
                    .gap_2()
                    .text_size(px(12.5))
                    .text_color(rgb(colors.muted))
                    .child(row().h(px(19.)).child(icon(IconName::Info).size(px(13.)).text_color(rgb(colors.quiet))))
                    .child(div().flex_1().min_w_0().line_height(relative(1.5)).child(text.clone())),
            });
        }
        for queued in &thread.queued {
            list = list.child(self.queued_block(colors, &queued.shown));
        }
        if thread.state == ThreadState::Running {
            list = list.child(
                row()
                    .h(px(28.))
                    .gap_2()
                    .text_size(px(12.5))
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
                    .text_size(px(12.5))
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
            div()
                .id("transcript")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.transcript)
                .child(list.mx_auto())
                .into_any_element()
        };
        col()
            .size_full()
            .child(div().flex_1().min_h_0().child(transcript))
            .child(
                row().flex_shrink_0().justify_center().px_5().pb_4().child(
                    div().w_full().max_w(px(COLUMN - 40.)).child(self.composer_view(colors, thread, window, cx)),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{file_mention, replace_file_mentions, thought_lines};
    use std::path::Path;

    #[test]
    fn dropped_project_file_shows_only_its_name_and_sends_its_path() {
        let mention = file_mention(Path::new("/project/src/main.rs"), Some(Path::new("/project")));
        assert!(!mention.token.contains("src/"));
        assert!(mention.token.contains("main.rs"));
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], false), "main.rs");
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], true), "@src/main.rs");
    }

    #[test]
    fn dropped_external_file_sends_its_absolute_path() {
        let mention = file_mention(Path::new("/tmp/example.txt"), Some(Path::new("/project")));
        assert_eq!(replace_file_mentions(&mention.token, &[mention.clone()], true), "@/tmp/example.txt");
    }

    #[test]
    fn thought_updates_ignore_spacing_lines() {
        let lines = thought_lines("Planning\n\n  Designing  \n\r\nRefining\n");
        assert_eq!(lines, ["Planning", "Designing", "Refining"]);
    }
}
