//! The mission card, opened from the mail icon at the left of the status bar.
//! It never opens by itself: the icon gets a dot when a mission waits on you,
//! and clicking anywhere else puts the card away again.
//!
//! The PIN shows in a row of eight cells, edge to edge, centred, with the
//! cells it doesn't use hatched. Accepting rolls the digits in, then a bar
//! drains while they're on show. Recall takes digits from the keyboard; a
//! wrong answer shakes, and a miss on the last try rolls each wrong digit on
//! to the right one, with the ones remembered marked green.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use crate::missions::{Mission, Phase, SLOTS, TRIES};
use crate::model::now_ms;
use gpui_component::tooltip::Tooltip;
use std::f32::consts::PI;

const ANCHOR: &str = "missions";
const CARD_W: f32 = 232.;
const CELL_H: f32 = 40.;
/// How long a wrong answer shakes before the cells clear for another try.
const SHAKE: Duration = Duration::from_millis(380);
/// After the shake, before a missed PIN's digits start rolling right.
const FIX_DELAY_MS: f32 = 450.;

/// How long digit `i` takes to roll in, from the moment the mission's accepted.
fn roll_ms(i: usize) -> i64 {
    420 + i as i64 * 90
}

/// Eases out hard, so a reel slows into place.
fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(4)
}

/// The card's own state, beside the mission.
pub(super) struct MissionUi {
    pub(super) open: bool,
    /// Digits typed toward the answer.
    pub(super) entry: String,
    focus: FocusHandle,
    /// Hides the PIN when its time on show is up.
    tick: Option<Task<()>>,
    /// Wrong answers shaken, to key each shake's animation.
    shakes: u64,
    /// A wrong answer is being shaken off; typing waits for it.
    shaking: bool,
}

impl MissionUi {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self { open: false, entry: String::new(), focus: cx.focus_handle(), tick: None, shakes: 0, shaking: false }
    }
}

impl Workspace {
    pub(super) fn missions_on(&self) -> bool {
        self.settings.missions != Some(false)
    }

    /// A turn ended somewhere: a mission waiting on turns counts it, and an
    /// empty mailbox may get a new one.
    pub(super) fn mission_turn_ended(&mut self, cx: &mut Context<Self>) {
        if self.missions_on() && self.missions.turn_ended(now_ms()) {
            self.save_mission();
            cx.notify();
        }
    }

    fn toggle_mission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The click that closed the card by landing outside it, on this button.
        if self.dismissed.take().is_some_and(|(a, at)| a == ANCHOR && at.elapsed() < Duration::from_millis(400)) {
            return;
        }
        if self.mission_ui.open {
            self.close_mission_card(window, cx);
        } else if self.missions.active.is_some() {
            self.mission_ui.open = true;
            if !self.mission_ui.shaking {
                self.mission_ui.entry.clear();
            }
            self.mission_ui.focus.focus(window);
            cx.notify();
        }
    }

    /// Put the card away; a finished mission goes with it, emptying the mailbox.
    pub(super) fn close_mission(&mut self, cx: &mut Context<Self>) {
        self.mission_ui.open = false;
        self.mission_ui.entry.clear();
        self.missions.put_away();
        cx.notify();
    }

    fn close_mission_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_keys = self.mission_ui.focus.is_focused(window);
        self.close_mission(cx);
        if had_keys {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
    }

    fn accept_mission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = &mut self.missions.active else { return };
        if !m.accept(now_ms(), roll_ms(m.pin.len() - 1)) {
            return;
        }
        self.save_mission();
        self.hide_mission_on_time(window, cx);
        cx.notify();
    }

    /// Hide the PIN when its time on show is up, card and all. Timers can
    /// wake a little early, so it checks the clock and waits again if so.
    fn hide_mission_on_time(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mission_ui.tick = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(Some(deadline)) = this.update(cx, |s, _| s.missions.active.as_ref().and_then(Mission::deadline)) else { return };
                let wait = (deadline - now_ms()).clamp(10, 60_000) as u64;
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                let hidden = this.update_in(cx, |s, window, cx| {
                    let hid = s.missions.active.as_mut().is_some_and(|m| m.tick(now_ms()));
                    if hid {
                        s.save_mission();
                        if s.mission_ui.open {
                            s.close_mission_card(window, cx);
                        }
                        cx.notify();
                    }
                    hid
                });
                if !matches!(hidden, Ok(false)) {
                    break;
                }
            }
        }));
    }

    fn mission_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        // Shortcuts carry on to the rest of the app.
        if k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
            return;
        }
        cx.stop_propagation();
        let Some(m) = &self.missions.active else { return };
        match (k.key.as_str(), m.phase) {
            ("escape", _) => self.close_mission_card(window, cx),
            ("enter", Phase::Offered) => self.accept_mission(window, cx),
            (_, Phase::Recall { .. }) if self.mission_ui.shaking => {}
            ("backspace", Phase::Recall { .. }) => {
                self.mission_ui.entry.pop();
                cx.notify();
            }
            (key, Phase::Recall { .. }) if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
                if self.mission_ui.entry.len() < m.pin.len() {
                    self.mission_ui.entry.push_str(key);
                }
                if self.mission_ui.entry.len() == m.pin.len() {
                    self.submit_mission(window, cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn submit_mission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.mission_ui.entry.clone();
        let Some(right) = self.missions.submit(&entry, now_ms()) else { return };
        self.save_mission();
        if right {
            return;
        }
        self.mission_ui.shakes += 1;
        // A last wrong try shows the PIN instead; any other clears for the next.
        if matches!(self.missions.active.as_ref().map(|m| m.phase), Some(Phase::Recall { .. })) {
            self.mission_ui.shaking = true;
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(SHAKE).await;
                let _ = this.update(cx, |s, cx| {
                    s.mission_ui.shaking = false;
                    s.mission_ui.entry.clear();
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Write the mission as it stands, off the main thread.
    fn save_mission(&self) {
        let (Some(store), Some(mission)) = (&self.store, &self.missions.active) else { return };
        let (writer, mission) = (store.writer(), mission.clone());
        std::thread::spawn(move || {
            if let Err(e) = store::write_mission(&writer, &mission) {
                eprintln!("chat: couldn't save a mission: {e}");
            }
        });
    }

    /// The mail icon: a dot when a mission waits on you, greyed with none.
    pub(super) fn mission_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.missions_on() {
            return None;
        }
        let colors = palette(cx);
        let mission = self.missions.active.as_ref();
        let open = self.mission_ui.open;
        let tip: SharedString = match mission.map(|m| m.phase) {
            None => "No missions".into(),
            Some(Phase::Offered) => "A mission is waiting".into(),
            Some(Phase::Recall { .. }) => "Time to enter the PIN".into(),
            Some(_) => "Mission".into(),
        };
        let dot = mission.filter(|m| m.needs_you()).map(|m| {
            let due = matches!(m.phase, Phase::Recall { .. });
            let dot = div()
                .absolute()
                .top(px(3.))
                .right(px(3.))
                .size(px(6.))
                .rounded_full()
                .border_1()
                .border_color(rgb(colors.chrome))
                .bg(rgb(if due { colors.red } else { colors.orange }));
            if due {
                dot.with_animation("mission-due", Animation::new(Duration::from_millis(1600)).repeat().with_easing(pulsating_between(0.35, 1.)), |d, t| d.opacity(t))
                    .into_any_element()
            } else {
                dot.into_any_element()
            }
        });
        let button = div()
            .id("mission-button")
            .debug_selector(|| "mission-button".into())
            .relative()
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .when(open, |s| s.bg(rgb(colors.hover)))
            .map(|s| match mission {
                Some(_) => s
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .child(icon(Glyph::Mail).text_color(rgb(if open { colors.text } else { colors.muted })))
                    .on_click(cx.listener(|s, _, window, cx| s.toggle_mission(window, cx))),
                None => s.opacity(0.5).child(icon(Glyph::Mail).text_color(rgb(colors.quiet))),
            })
            .children(dot)
            .when(!open, |s| s.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx)));
        Some(self.anchor(ANCHOR, button).into_any_element())
    }

    /// The card, placed above the mail icon, while it's open.
    pub(super) fn mission_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.mission_ui.open || !self.missions_on() {
            return None;
        }
        let mission = self.missions.active.as_ref()?;
        let at = self.anchors.borrow().get(ANCHOR).map(|b| b.origin - point(px(0.), px(6.)));
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(at.unwrap_or_default())
                    .snap_to_window_with_margin(px(8.))
                    .child(self.mission_card(mission, cx)),
            )
            .with_priority(2),
        )
    }

    fn mission_card(&self, m: &Mission, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = palette(cx);
        let n = m.pin.len();
        let (title, sub): (&str, String) = match m.phase {
            Phase::Offered | Phase::Showing { .. } => ("Memorize", format!("{n}-digit PIN")),
            Phase::Waiting { .. } => ("Memorize", "hidden".into()),
            Phase::Recall { .. } => ("Enter PIN", "type the digits".into()),
            Phase::Done { solved: true } => ("Correct", "streak +1".into()),
            Phase::Done { solved: false } => ("Missed", "streak reset".into()),
        };
        let digit = |text: SharedString| div().font_family(mono()).text_size(px(crate::theme::scaled_ui(18.))).child(text);
        let cells: Vec<AnyElement> = match m.phase {
            Phase::Offered => (0..n).map(|_| digit("/".into()).text_color(rgb(colors.quiet)).into_any_element()).collect(),
            Phase::Showing { since, .. } => (0..n).map(|i| roll_in(colors, m, i, since)).collect(),
            Phase::Waiting { .. } => (0..n).map(|_| pip(colors).into_any_element()).collect(),
            Phase::Recall { .. } => {
                let entry = &self.mission_ui.entry;
                (0..n)
                    .map(|i| match entry.as_bytes().get(i) {
                        Some(&b) => digit(char::from(b).to_string().into()).into_any_element(),
                        None if i == entry.len() => div()
                            .size_full()
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(pip(colors))
                            .child(div().absolute().bottom_0().left_0().right_0().h(px(2.)).bg(rgb(colors.blue)))
                            .into_any_element(),
                        None => pip(colors).into_any_element(),
                    })
                    .collect()
            }
            Phase::Done { solved } => {
                let guess = m.guess.as_deref().unwrap_or("");
                m.pin
                    .chars()
                    .enumerate()
                    .map(|(i, want)| {
                        let given = guess.chars().nth(i);
                        if solved {
                            digit(want.to_string().into()).into_any_element()
                        } else if given == Some(want) {
                            // Remembered: marked green.
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(rgb(colors.green_tint))
                                .text_color(rgb(colors.green))
                                .child(digit(want.to_string().into()))
                                .into_any_element()
                        } else {
                            roll_to_right(colors, m.id, i, given.unwrap_or('0'), want)
                        }
                    })
                    .collect()
            }
        };
        // A wrong answer shakes the row, tinted red as it goes.
        let shake = matches!(m.phase, Phase::Recall { .. } | Phase::Done { solved: false }) && self.mission_ui.shakes > 0;
        let row_of_cells = strip(colors, cells).when(m.phase == (Phase::Done { solved: true }), |s| {
            s.child(div().absolute().inset_0().bg(with_alpha(colors.green, 0.12)))
        });
        let row_of_cells = if shake {
            let red = colors.red;
            row_of_cells
                .with_animation(
                    ElementId::NamedInteger("mission-shake".into(), self.mission_ui.shakes),
                    Animation::new(SHAKE),
                    move |s, t| {
                        let x = (t * PI * 5.).sin() * 7. * (1. - t);
                        s.left(px(x)).child(div().absolute().inset_0().bg(with_alpha(red, 0.2 * (1. - t))))
                    },
                )
                .into_any_element()
        } else {
            row_of_cells.into_any_element()
        };

        let streak = self.missions.streak;
        let streak_el = row()
            .gap(px(4.))
            .child(icon(Glyph::Flame).size(px(13.)).text_color(rgb(if streak > 0 { colors.orange } else { colors.quiet })))
            .child(streak.to_string());
        let note = |text: String| div().text_color(rgb(colors.muted)).child(text);
        let turns = |t: u32| format!("asks in {t} turn{}", if t == 1 { "" } else { "s" });
        let foot_right = match m.phase {
            Phase::Offered => div()
                .id("mission-accept")
                .debug_selector(|| "mission-accept".into())
                .px(px(12.))
                .py(px(3.))
                .rounded(px(6.))
                .cursor_pointer()
                .bg(rgb(colors.blue))
                .text_color(rgb(colors.primary_text))
                .hover(|s| s.opacity(0.9))
                .child("Accept")
                .on_click(cx.listener(|s, _, window, cx| s.accept_mission(window, cx)))
                .into_any_element(),
            Phase::Showing { .. } => note(turns(m.wait_turns)).into_any_element(),
            Phase::Waiting { turns_left } => note(turns(turns_left)).into_any_element(),
            Phase::Recall { tries } => note(format!("{}/{TRIES}", tries + 1)).into_any_element(),
            Phase::Done { .. } => div().into_any_element(),
        };
        let timer = match m.phase {
            Phase::Showing { since, until } => {
                let start = since + roll_ms(n - 1);
                let blue = colors.blue;
                Some(
                    div().h(px(3.)).bg(rgb(colors.border)).child(
                        div().h_full().bg(rgb(blue)).with_animation(
                            ElementId::NamedInteger("mission-timer".into(), m.id as u64),
                            // Repeats only to keep frames coming; the clock says where it is.
                            Animation::new(Duration::from_secs(1)).repeat(),
                            move |bar, _| {
                                let left = (until - now_ms().max(start)) as f32 / (until - start).max(1) as f32;
                                bar.w(relative(left.clamp(0., 1.)))
                            },
                        ),
                    ),
                )
            }
            _ => None,
        };

        col()
            .id("mission-card")
            .debug_selector(|| "mission-card".into())
            .track_focus(&self.mission_ui.focus)
            .occlude()
            .w(px(CARD_W))
            .overflow_hidden()
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .text_size(px(crate::theme::scaled_ui(12.)))
            .text_color(rgb(colors.text))
            .on_key_down(cx.listener(Self::mission_key))
            .on_mouse_down_out(cx.listener(|s, _, window, cx| {
                if s.mission_ui.open {
                    s.dismissed = Some((ANCHOR, Instant::now()));
                    s.close_mission_card(window, cx);
                }
            }))
            .child(
                row()
                    .justify_between()
                    .px(px(12.))
                    .pt(px(10.))
                    .pb(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(crate::theme::scaled_ui(13.))).child(title))
                    .child(div().text_size(px(crate::theme::scaled_ui(11.))).text_color(rgb(colors.quiet)).child(sub)),
            )
            .child(row_of_cells)
            .child(row().justify_between().px(px(12.)).h(px(40.)).child(streak_el).child(foot_right))
            .children(timer)
    }
}

/// `SLOTS` cells edge to edge, `cells` centred among them, the rest hatched.
fn strip(colors: Palette, cells: Vec<AnyElement>) -> Div {
    let lead = (SLOTS - cells.len().min(SLOTS)) / 2;
    let mut cells = cells.into_iter();
    let row = row().relative().h(px(CELL_H)).bg(rgb(colors.background)).border_y_1().border_color(rgb(colors.border));
    row.children((0..SLOTS).map(|i| {
        let content = if i >= lead { cells.next() } else { None };
        div()
            .relative()
            .flex_1()
            .flex_basis(px(0.))
            .h_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .when(i + 1 < SLOTS, |s| s.border_r_1())
            .border_color(rgb(colors.border))
            .map(|s| match content {
                Some(c) => s.child(c),
                None => s.bg(rgb(colors.panel)).child(hatch(colors)),
            })
    }))
}

/// Thin 45° lines across a cell that takes no digit.
fn hatch(colors: Palette) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                let h = b.size.height;
                let mut x = b.left() - h;
                while x < b.right() {
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(point(x, b.bottom()));
                    line.line_to(point(x + h, b.top()));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, with_alpha(colors.quiet, 0.35));
                    }
                    x += px(5.);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

fn pip(colors: Palette) -> Div {
    div().size(px(6.)).rounded_full().bg(rgb(colors.quiet))
}

fn with_alpha(color: u32, a: f32) -> Hsla {
    Hsla::from(rgb(color)).opacity(a)
}

/// A column of glyphs, each a cell high, slid up by `offset` cells.
fn reel(glyphs: Vec<(String, Hsla)>) -> Div {
    col().absolute().left_0().right_0().top_0().font_family(mono()).text_size(px(crate::theme::scaled_ui(18.))).children(
        glyphs.into_iter().map(|(g, color)| row().h(px(CELL_H)).flex_shrink_0().justify_center().text_color(color).child(g)),
    )
}

/// A digit of a PIN on show, spun in from a slash through a few others.
/// Where it is comes from the clock, so the card can close and reopen.
fn roll_in(colors: Palette, m: &Mission, i: usize, since: i64) -> AnyElement {
    let spins = 6 + i;
    // Filler digits that stay the same frame to frame.
    let mut seed = (m.id as u64).wrapping_mul(31).wrapping_add(i as u64 * 7919);
    let mut glyphs = vec![("/".to_string(), Hsla::from(rgb(colors.quiet)))];
    for _ in 0..spins {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        glyphs.push((((seed >> 33) % 10).to_string(), Hsla::from(rgb(colors.text))));
    }
    glyphs.push((m.pin[i..i + 1].to_string(), Hsla::from(rgb(colors.text))));
    let steps = glyphs.len() - 1;
    let dur = roll_ms(i);
    div()
        .size_full()
        .relative()
        .child(reel(glyphs).with_animation(
            ElementId::NamedInteger("mission-roll".into(), m.id as u64 * SLOTS as u64 + i as u64),
            Animation::new(Duration::from_secs(1)).repeat(),
            move |r, _| {
                let t = ease_out((now_ms() - since) as f32 / dur as f32);
                r.top(px(-t * steps as f32 * CELL_H))
            },
        ))
        .into_any_element()
}

/// A wrong digit of a missed PIN: red, then counting up to the right one
/// once the shake is over, its red fading as it lands.
fn roll_to_right(colors: Palette, id: i64, i: usize, given: char, want: char) -> AnyElement {
    let (from, to) = (given.to_digit(10).unwrap_or(0), want.to_digit(10).unwrap_or(0));
    let mut steps = (to + 10 - from) % 10;
    if steps < 3 {
        steps += 10;
    }
    let mut glyphs = vec![(from.to_string(), Hsla::from(rgb(colors.red)))];
    glyphs.extend((1..=steps).map(|k| (((from + k) % 10).to_string(), Hsla::from(rgb(colors.text)))));
    let delay = FIX_DELAY_MS + i as f32 * 70.;
    let dur = 500. + steps as f32 * 25.;
    let total = delay + dur;
    let tint = colors.red;
    div()
        .size_full()
        .relative()
        .with_animation(
            ElementId::NamedInteger("mission-fix".into(), id as u64 * SLOTS as u64 + i as u64),
            Animation::new(Duration::from_millis(total as u64)),
            move |cell, t| {
                let ms = t * total;
                let p = ease_out((ms - delay) / dur);
                // Red through the shake and the first of the roll, then fading out.
                let fade = (1. - ((ms - delay - dur * 0.4) / (dur * 0.6)).clamp(0., 1.)) * 0.18;
                cell.bg(with_alpha(tint, fade)).child(reel(glyphs.clone()).top(px(-p * steps as f32 * CELL_H)))
            },
        )
        .into_any_element()
}
