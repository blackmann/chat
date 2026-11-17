//! Slash command suggestions for the chat composer.
use super::{ui::*, *};
use gpui_component::input::{Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp};

fn command_prefix(text: &str, cursor: usize) -> Option<&str> {
    let prefix = text.get(..cursor)?.strip_prefix('/')?;
    (!prefix.chars().any(char::is_whitespace)).then_some(prefix)
}

impl Workspace {
    pub(super) fn observe_command_draft(&mut self, cx: &mut Context<Self>) {
        cx.subscribe(&self.composer, |s, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change | InputEvent::Blur) {
                s.command_index = 0;
                s.command_scroll.set_offset(point(px(0.), px(0.)));
                s.commands_dismissed = false;
                cx.notify();
            }
        })
        .detach();
    }

    fn command_matches<'a>(&'a self, window: &Window, cx: &App) -> Vec<&'a (String, String)> {
        let input = self.composer.read(cx);
        if self.commands_dismissed
            || self.popup.is_some()
            || !input.focus_handle(cx).is_focused(window)
        {
            return vec![];
        }
        let text = input.value();
        let Some(prefix) = command_prefix(&text, input.cursor()) else {
            return vec![];
        };
        let prefix = prefix.to_lowercase();
        self.current_thread()
            .into_iter()
            .flat_map(|t| &t.commands)
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
            .collect()
    }

    fn fill_command(&self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |input, cx| {
            let draft = input.value();
            let end = draft.find(char::is_whitespace).unwrap_or(draft.len());
            // Include the separator in the edit so the cursor lands where arguments begin.
            let separator = draft[end..].chars().next();
            let (end, separator) = match separator {
                Some(c) => (end + c.len_utf8(), c),
                None => (end, ' '),
            };
            input.replace_text_in_range(
                Some(0..draft[..end].encode_utf16().count()),
                &format!("/{name}{separator}"),
                window,
                cx,
            );
            input.focus(window, cx);
        });
    }

    fn step_command(&mut self, delta: isize, window: &Window, cx: &mut Context<Self>) {
        let count = self.command_matches(window, cx).len();
        if count != 0 {
            self.command_index = (self.command_index.min(count - 1) as isize + delta)
                .rem_euclid(count as isize) as usize;
            self.command_scroll.scroll_to_item(self.command_index);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn accept_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.command_matches(window, cx);
        if let Some((name, _)) =
            matches.get(self.command_index.min(matches.len().saturating_sub(1)))
        {
            let name = name.clone();
            self.fill_command(&name, window, cx);
            self.commands_dismissed = true;
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn with_command_menu(
        &self,
        composer: Div,
        colors: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let matches = self.command_matches(window, cx);
        if matches.is_empty() {
            return composer;
        }

        let active = self.command_index.min(matches.len() - 1);
        let menu = col()
            .id("slash-commands")
            .occlude()
            .p_1()
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .text_color(rgb(colors.text));
        let mut rows = col()
            .id("slash-command-rows")
            .max_h(px(168.))
            .overflow_y_scroll()
            .track_scroll(&self.command_scroll);
        for (i, (name, description)) in matches.into_iter().enumerate() {
            let name = name.clone();
            rows = rows.child(
                row()
                    .id(("slash-command", i))
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_3()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(i == active, |row| row.bg(rgb(colors.selected)))
                    .hover(|row| row.bg(rgb(colors.popover_hover)))
                    .child(
                        div()
                            .font_family(mono())
                            .text_size(px(crate::theme::mono_size()))
                            .child(format!("/{name}")),
                    )
                    .child(
                        one_line(description.clone(), 18.)
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::theme::scaled_ui(12.)))
                            .text_color(rgb(colors.muted)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, _, window, cx| {
                            s.fill_command(&name, window, cx);
                            s.commands_dismissed = true;
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            );
        }
        composer
            .relative()
            .capture_action(cx.listener(|s, _: &MoveDown, w, cx| s.step_command(1, w, cx)))
            .capture_action(cx.listener(|s, _: &MoveUp, w, cx| s.step_command(-1, w, cx)))
            .capture_action(cx.listener(|s, action: &Enter, w, cx| {
                if !action.secondary {
                    s.accept_command(w, cx);
                }
            }))
            .capture_action(cx.listener(|s, _: &IndentInline, w, cx| s.accept_command(w, cx)))
            .capture_action(cx.listener(|s, _: &Escape, _, cx| {
                s.commands_dismissed = true;
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                deferred(
                    div()
                        .absolute()
                        .bottom_full()
                        .left_0()
                        .ml_2()
                        .mb_2()
                        .w_full()
                        .max_w(px(460.))
                        .child(menu.child(rows)),
                )
                .with_priority(1),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::command_prefix;

    #[test]
    fn command_prefix_checks_the_start_and_cursor() {
        assert_eq!(command_prefix("/", 1), Some(""));
        assert_eq!(command_prefix("/compact argument", 3), Some("co"));
        assert_eq!(command_prefix("hello /co", 9), None);
        assert_eq!(command_prefix("/compact argument", 17), None);
    }
}
