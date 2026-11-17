//! Reasoning effort as a slider: the agent's levels are stops along a track,
//! from least to most. The popover opens above the composer's effort button.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::{IconName, tooltip::Tooltip};

/// The thumb's diameter; the track is a little taller so the thumb sits inside it.
const THUMB: f32 = 16.;
const TRACK: f32 = 20.;
/// Where the first and last stops sit, in from the track's ends.
const INSET: f32 = TRACK / 2.;

impl Workspace {
    /// The stop under the pointer, from where the track was last painted.
    fn effort_step(&self, x: Pixels, steps: usize) -> usize {
        let track = self.effort_track.get();
        let span = (track.size.width - px(INSET * 2.)).max(px(1.));
        let share = ((x - track.left() - px(INSET)) / span).clamp(0., 1.);
        (share * (steps - 1) as f32).round() as usize
    }
    /// Move to the stop under `x`, if it isn't already there.
    fn slide_effort(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(setting) = self.setting(Role::Effort).cloned() else {
            return;
        };
        if setting.choices.len() < 2 {
            return;
        }
        let step = self.effort_step(x, setting.choices.len());
        let choice = &setting.choices[step];
        if choice.id != setting.current {
            self.set_setting(setting.id.clone(), choice.id.clone(), cx);
        }
    }
    pub(super) fn close_effort(&mut self, cx: &mut Context<Self>) {
        if self.effort_open {
            self.effort_open = false;
            self.effort_dragging = false;
            cx.notify();
        }
    }

    pub(super) fn effort_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = palette(cx);
        let thread = self.current_thread()?;
        let setting = self.setting(Role::Effort)?;
        let steps = setting.choices.len().max(1);
        let at = setting.choices.iter().position(|c| c.id == setting.current).unwrap_or(0);
        let model = self.setting(Role::Model).map(|m| m.current_name().to_string());
        // What the agent started the session on, to go back to.
        let default = thread.defaults.iter().find(|(id, _)| *id == setting.id).map(|(_, v)| v.clone());
        let can_reset = default.as_ref().is_some_and(|d| *d != setting.current);

        let track = self.effort_track.clone();
        let width = track.get().size.width;
        let share = if steps > 1 { at as f32 / (steps - 1) as f32 } else { 0. };
        // In pixels once the track has been measured; until then the thumb waits at the start.
        let center = px(INSET) + (width - px(INSET * 2.)).max(px(0.)) * share;
        let mut rail = div()
            .id("effort-track")
            .relative()
            .w_full()
            .h(px(TRACK))
            .rounded_full()
            .bg(rgb(colors.hover))
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, window, _| {
                        // Stops are placed in pixels: draw again once the track's width is known.
                        if track.get() != bounds {
                            track.set(bounds);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                    .absolute()
                    .inset_0(),
            )
            // The filled part runs from the start to the thumb.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h_full()
                    .w(center + px(TRACK / 2.))
                    .rounded_full()
                    .bg(rgb(colors.blue)),
            );
        for i in 0..steps {
            if i == at {
                continue;
            }
            let x = px(INSET) + (width - px(INSET * 2.)).max(px(0.)) * if steps > 1 { i as f32 / (steps - 1) as f32 } else { 0. };
            rail = rail.child(
                div()
                    .absolute()
                    .top(px(TRACK / 2. - 1.5))
                    .left(x - px(1.5))
                    .size(px(3.))
                    .rounded_full()
                    .bg(if i < at { Hsla::from(rgb(colors.primary_text)).opacity(0.7) } else { rgb(colors.quiet).into() }),
            );
        }
        rail = rail
            .child(
                div()
                    .absolute()
                    .top(px((TRACK - THUMB) / 2.))
                    .left(center - px(THUMB / 2.))
                    .size(px(THUMB))
                    .rounded_full()
                    .bg(rgb(0xffffff))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_sm(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(move |s, e: &MouseDownEvent, _, cx| {
                s.effort_dragging = true;
                s.slide_effort(e.position.x, cx);
            }))
            .on_mouse_move(cx.listener(|s, e: &MouseMoveEvent, _, cx| {
                if s.effort_dragging && e.pressed_button == Some(MouseButton::Left) {
                    s.slide_effort(e.position.x, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, _| s.effort_dragging = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, _| s.effort_dragging = false));

        let reset = {
            let (id, default) = (setting.id.clone(), default.clone());
            row()
                .id("effort-reset")
                .size(px(20.))
                .justify_center()
                .rounded(px(5.))
                .text_color(rgb(if can_reset { colors.muted } else { colors.quiet }))
                .when(can_reset, |s| {
                    s.cursor_pointer()
                        .hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text)))
                        .tooltip(|w, cx| Tooltip::new("Back to the default").build(w, cx))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if let Some(default) = &default {
                                s.set_setting(id.clone(), default.clone(), cx);
                            }
                        }))
                })
                .child(icon(Glyph::RotateCcw).size(px(12.)))
        };
        let model_line = model.map(|name| {
            row()
                .id("effort-model")
                .gap(px(2.))
                .px_1()
                .rounded(px(4.))
                .text_size(px(11.))
                .text_color(rgb(colors.muted))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(colors.text)))
                .child(name)
                .child(icon(IconName::ChevronRight).size(px(11.)))
                // The model is the other half of how hard the agent thinks.
                .on_click(cx.listener(|s, _, window, cx| {
                    s.close_effort(cx);
                    let weak = cx.weak_entity();
                    let build: crate::menu::Builder = Rc::new(move |cx: &App| {
                        weak.upgrade()
                            .and_then(|w| w.read(cx).setting(Role::Model).map(|m| chat::choices(m, weak.clone(), false)))
                            .unwrap_or_default()
                    });
                    s.open_popover("model", Point::default(), Corner::BottomRight, 300., None, build, window, cx);
                }))
        });
        Some(
            col()
                .id("effort-popover")
                .occlude()
                .w(px(232.))
                .px_2()
                .pt_2()
                .pb(px(10.))
                .gap(px(8.))
                .bg(rgb(colors.popover))
                .border_1()
                .border_color(rgb(colors.popover_border))
                .rounded(px(10.))
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                    if s.effort_open {
                        s.dismissed = Some(("effort", Instant::now()));
                        s.close_effort(cx);
                    }
                }))
                .child(
                    row()
                        .items_start()
                        .child(row().size(px(20.)).justify_center().text_color(rgb(colors.quiet)).child(icon(Glyph::Gauge).size(px(12.))))
                        .child(
                            col()
                                .flex_1()
                                .items_center()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(colors.blue))
                                        .child(setting.current_name().to_string()),
                                )
                                .children(model_line),
                        )
                        .child(reset),
                )
                .child(rail)
                .into_any_element(),
        )
    }
}
