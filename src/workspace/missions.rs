//! The Missions page in the right panel: pick a challenge, play it through
//! its phases, and see how the last ones went. Missions belong to the app,
//! so the page is the same whichever project is showing.
use super::git_panel::bar_button;
use super::ui::*;
use super::*;
use crate::missions::{Kind, Mission, Phase};
use crate::model::now_ms;
use gpui_component::{
    IconName,
    input::{Input, InputEvent},
};

/// Done missions listed under Recent.
const RECENT: usize = 8;

/// The field a mission's answer is typed in: Enter sends it.
pub(super) fn answer_input(window: &mut Window, cx: &mut Context<Workspace>) -> Entity<InputState> {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Type what you remember")
            .validate(|text, _| text.len() <= 16 && text.chars().all(|c| c.is_ascii_digit() || c == ' '))
    });
    cx.subscribe_in(&input, window, |s, _, e: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = e {
            s.submit_mission(window, cx);
        }
    })
    .detach();
    input
}

/// "2:05", or "42s" under a minute.
fn countdown(ms: i64) -> String {
    let secs = (ms.max(0) + 999) / 1000;
    if secs < 60 { format!("{secs}s") } else { format!("{}:{:02}", secs / 60, secs % 60) }
}

impl Workspace {
    fn missions_visible(&self) -> bool {
        self.side_open && self.side == Side::Missions
    }

    /// Start a mission of `kind`, unless one is under way.
    fn start_mission(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if self.missions.active.as_ref().is_some_and(|m| !m.done()) {
            return;
        }
        self.missions.file();
        self.missions.start(kind, now_ms());
        self.mission_input.update(cx, |s, cx| s.set_value("", window, cx));
        self.save_mission();
        self.advance_mission(cx);
    }

    /// Move the mission on with the clock, and wake again when it next can:
    /// often while its page shows, for the countdown, or else at its deadline.
    pub(super) fn advance_mission(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let Some(mission) = &mut self.missions.active else {
            self.mission_tick = None;
            return;
        };
        if mission.tick(now) {
            self.save_mission();
        }
        let Some(deadline) = self.missions.active.as_ref().and_then(Mission::deadline) else {
            self.mission_tick = None;
            cx.notify();
            return;
        };
        let step = if self.missions_visible() { 250 } else { i64::MAX };
        let wait = (deadline - now).clamp(10, step);
        self.mission_tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(wait as u64)).await;
            let _ = this.update(cx, |s, cx| s.advance_mission(cx));
        }));
        cx.notify();
    }

    /// A turn ended on some thread: a mission waiting on turns counts it.
    pub(super) fn mission_turn_ended(&mut self, cx: &mut Context<Self>) {
        if self.missions.active.as_mut().is_some_and(|m| m.turn_ended(now_ms())) {
            self.save_mission();
            self.advance_mission(cx);
        }
    }

    fn submit_mission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = self.mission_input.read(cx).value().to_string();
        if answer.trim().is_empty() {
            return;
        }
        if self.missions.active.as_mut().is_some_and(|m| m.submit(&answer, now_ms())) {
            self.mission_input.update(cx, |s, cx| s.set_value("", window, cx));
            self.save_mission();
            self.advance_mission(cx);
        }
    }

    fn give_up_mission(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.missions.active {
            m.give_up(now_ms());
            self.save_mission();
            self.advance_mission(cx);
        }
    }

    /// Put a finished mission away, back to the list of missions.
    fn close_mission(&mut self, cx: &mut Context<Self>) {
        self.missions.file();
        cx.notify();
    }

    /// Bring the keys to the answer field, when a recall is waiting for one.
    pub(super) fn focus_mission_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.missions.ready() {
            self.mission_input.update(cx, |s, cx| s.focus(window, cx));
        }
        // The countdown ticks faster while its page shows.
        self.advance_mission(cx);
    }

    /// Write the active mission as it stands, off the main thread.
    fn save_mission(&self) {
        let (Some(store), Some(mission)) = (&self.store, &self.missions.active) else { return };
        let (writer, mission) = (store.writer(), mission.clone());
        std::thread::spawn(move || {
            if let Err(e) = store::write_mission(&writer, &mission) {
                eprintln!("chat: couldn't save a mission: {e}");
            }
        });
    }

    pub(super) fn missions_page(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let body = match &self.missions.active {
            Some(mission) => self.mission_card(colors, mission, cx),
            None => self.mission_picker(colors, cx),
        };
        let recent: Vec<&Mission> = self.missions.history.iter().take(RECENT).collect();
        col()
            .pb_3()
            .child(div().px_3().pt_3().child(body))
            .when(!recent.is_empty(), |s| {
                let total: u32 = self.missions.history.iter().filter_map(Mission::score).sum();
                s.child(div().pt_3().child(section(colors, "Recent", None)))
                    .children(recent.into_iter().map(|m| recent_row(colors, m)))
                    .child(row().px_3().pt_2().child(label(colors, format!("{total} points in your last {}", self.missions.history.len()))))
            })
    }

    fn mission_picker(&self, colors: Palette, cx: &mut Context<Self>) -> Div {
        col()
            .gap_3()
            .child(label(colors, "Small challenges for while your agents work. They follow you across projects.").line_height(relative(1.45)))
            .children(Kind::ALL.into_iter().map(|kind| {
                let level = self.missions.next_level(kind);
                card(colors)
                    .p_3()
                    .gap_2()
                    .child(
                        row()
                            .gap_2()
                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(kind.title()))
                            .child(pill(format!("Level {}", level + 1), colors.blue, colors.blue_tint)),
                    )
                    .child(label(colors, kind.brief()).line_height(relative(1.45)))
                    .child(
                        row().pt_1().child(
                            bar_button(colors, "start-mission", Some(IconName::ArrowRight), "Start", true)
                                .debug_selector(|| "start-mission".into())
                                .on_click(cx.listener(move |s, _, window, cx| s.start_mission(kind, window, cx))),
                        ),
                    )
            }))
    }

    fn mission_card(&self, colors: Palette, mission: &Mission, cx: &mut Context<Self>) -> Div {
        let now = now_ms();
        let heading = row()
            .gap_2()
            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(mission.kind.title()))
            .child(pill(format!("Level {}", mission.level + 1), colors.blue, colors.blue_tint));
        let give_up = || {
            bar_button(colors, "give-up-mission", None, "Give up", true).on_click(cx.listener(|s, _, _, cx| s.give_up_mission(cx)))
        };
        let body = match mission.phase {
            Phase::Memorize { until } => {
                let total = (until - mission.started_at).max(1);
                let left = (until - now).clamp(0, total);
                col()
                    .gap_3()
                    .child(label(colors, "Memorize this."))
                    .child(digits(colors, &mission.solution(), None))
                    .child(
                        row()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .overflow_hidden()
                                    .bg(rgb(colors.hover))
                                    .child(div().h_full().w(relative(left as f32 / total as f32)).bg(rgb(colors.blue))),
                            )
                            .child(label(colors, countdown(left))),
                    )
            }
            Phase::Waiting { since, turns } => {
                let wait = mission.wait();
                let mut when = Vec::new();
                if let Some(n) = wait.turns {
                    let left = n.saturating_sub(turns);
                    when.push(if left == 1 { "when your next turn ends".to_string() } else { format!("after {left} more turns end") });
                }
                if let Some(ms) = wait.ms {
                    when.push(format!("in {}", countdown(since + ms - now)));
                }
                col()
                    .gap_3()
                    .child(digits(colors, &"•".repeat(mission.solution().chars().count()), None))
                    .child(label(colors, format!("Hold on to it. You'll be asked for it {}.", when.join(", or "))).line_height(relative(1.45)))
                    .child(row().child(give_up()))
            }
            Phase::Recall => col()
                .gap_3()
                .child(label(colors, "What was the PIN?"))
                .child(
                    Input::new(&self.mission_input)
                        .font_family(mono())
                        .text_size(px(crate::theme::scaled_mono(16.))),
                )
                .child(
                    row()
                        .gap_2()
                        .child(
                            bar_button(colors, "submit-mission", None, "Check", true)
                                .on_click(cx.listener(|s, _, window, cx| s.submit_mission(window, cx))),
                        )
                        .child(give_up()),
                ),
            Phase::Done { score, perfect, .. } => {
                let kind = mission.kind;
                let verdict = match (&mission.answer, perfect) {
                    (None, _) => "Given up.".to_string(),
                    (Some(_), true) => format!("Perfect! +{score} points."),
                    (Some(_), false) => format!("{} of {} right. +{score} points.", mission.marks().iter().filter(|m| **m).count(), mission.marks().len()),
                };
                col()
                    .gap_3()
                    .child(digits(colors, &mission.solution(), mission.answer.is_some().then(|| mission.marks())))
                    .when_some(mission.answer.as_ref().filter(|_| !perfect), |s, answer| {
                        s.child(label(colors, format!("You said {answer}")).font_family(mono()))
                    })
                    .child(div().text_color(rgb(if perfect { colors.green } else { colors.text })).child(verdict))
                    .child(
                        row()
                            .gap_2()
                            .child(
                                bar_button(colors, "again-mission", Some(IconName::ArrowRight), "Another", true)
                                    .on_click(cx.listener(move |s, _, window, cx| s.start_mission(kind, window, cx))),
                            )
                            .child(bar_button(colors, "close-mission", None, "Done", true).debug_selector(|| "close-mission".into()).on_click(cx.listener(|s, _, _, cx| s.close_mission(cx)))),
                    )
            }
        };
        card(colors).p_3().gap_3().child(heading).child(body)
    }
}

/// The puzzle's characters in large type, each coloured right or wrong when marked.
fn digits(colors: Palette, text: &str, marks: Option<Vec<bool>>) -> Div {
    row().gap(px(6.)).flex_wrap().children(text.chars().enumerate().map(|(i, c)| {
        let tone = marks.as_ref().and_then(|m| m.get(i)).map(|right| if *right { (colors.green, colors.green_tint) } else { (colors.red, colors.red_tint) });
        div()
            .w(px(26.))
            .h(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .bg(rgb(tone.map_or(colors.hover, |t| t.1)))
            .text_color(rgb(tone.map_or(colors.text, |t| t.0)))
            .font_family(mono())
            .text_size(px(crate::theme::scaled_mono(18.)))
            .font_weight(FontWeight::MEDIUM)
            .child(c.to_string())
    }))
}

fn recent_row(colors: Palette, mission: &Mission) -> Div {
    let (score, perfect) = match mission.phase {
        Phase::Done { score, perfect, .. } => (score, perfect),
        _ => (0, false),
    };
    let (fg, bg) = if perfect {
        (colors.green, colors.green_tint)
    } else if mission.answer.is_none() {
        (colors.muted, colors.hover)
    } else {
        (colors.amber, colors.amber_tint)
    };
    row()
        .h(px(28.))
        .px_3()
        .gap_2()
        .child(div().flex_1().min_w_0().truncate().child(mission.kind.title()))
        .child(label(colors, format!("Level {}", mission.level + 1)))
        .child(pill(if mission.answer.is_none() { "—".to_string() } else { format!("+{score}") }, fg, bg))
}

#[cfg(test)]
mod tests {
    use super::countdown;

    #[test]
    fn counts_down_in_seconds_then_minutes() {
        assert_eq!(countdown(0), "0s");
        assert_eq!(countdown(1), "1s");
        assert_eq!(countdown(42_000), "42s");
        assert_eq!(countdown(125_000), "2:05");
    }
}
