//! The pinned chat tab: the thread's transcript above the composer.
use super::ui::*;
use super::*;
use crate::{
    agent::{Answer, Kind, Status},
    icons::Glyph,
    menu::{Entry, Item},
    model::{Permission, ToolCall},
};
use gpui_component::{Icon, IconName, input::Input, tooltip::Tooltip};

/// The transcript and composer share one readable column.
const COLUMN: f32 = 760.;
/// How much of a tool's output an opened tool call shows: the end, where results are.
const TOOL_OUTPUT_LINES: usize = 200;

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

    fn composer_view(&self, colors: Palette, thread: &Thread, cx: &mut Context<Self>) -> Div {
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
        // While the agent works the button stops it; one turn runs at a time.
        let send = row()
            .id("send")
            .size(px(26.))
            .ml_1()
            .flex_shrink_0()
            .justify_center()
            .rounded_full()
            .map(|s| {
                if busy {
                    s.bg(rgb(colors.text)).text_color(rgb(colors.background)).cursor_pointer()
                } else if empty {
                    s.bg(rgb(colors.hover)).text_color(rgb(colors.quiet))
                } else {
                    s.bg(rgb(colors.blue)).text_color(rgb(colors.primary_text)).cursor_pointer()
                }
            })
            .child(if busy { icon(Glyph::Stop).size(px(12.)) } else { icon(IconName::ArrowUp) })
            .tooltip(move |w, cx| Tooltip::new(if busy { "Stop · esc" } else { "Send · ⌘ ↵" }).build(w, cx))
            .on_click(cx.listener(move |s, _, w, cx| if busy { s.stop(cx) } else { s.submit(&Submit, w, cx) }));
        col()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .child(div().px_1().pt_1().child(Input::new(&self.composer).appearance(false)))
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
                    .child(send),
            )
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
                Block::Thought(text) => div()
                    .pl_3()
                    .border_l_2()
                    .border_color(rgb(colors.border))
                    .text_size(px(12.5))
                    .text_color(rgb(colors.muted))
                    .line_height(relative(1.5))
                    .child(text.trim().to_string()),
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
                    div().w_full().max_w(px(COLUMN - 40.)).child(self.composer_view(colors, thread, cx)),
                ),
            )
            .into_any_element()
    }
}
