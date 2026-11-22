//! The usage widget: opens from the status bar's limit item. The weekly limit is
//! a bar across the week, and the five-hour one a wedge on a clock face, so how
//! much is used reads against how much time has gone.
use super::ui::*;
use super::*;
use crate::agent::{LimitStatus, RateLimit};
use std::f32::consts::{PI, TAU};

const WIDTH: f32 = 176.;
const PAD: f32 = 8.;
/// The clock fills the popover's width inside its padding.
const CLOCK: f32 = WIDTH - PAD * 2.;
const BAR: f32 = 16.;
/// The clock's strokes and marks were drawn for a 236px face; they scale with it.
const DRAWN_FOR: f32 = 236.;
const WEEK: i64 = 7 * 24 * 3600;
const FIVE_HOURS: i64 = 5 * 3600;
/// How often the open widget redraws, so the hands and the bar's "now" keep up.
const TICK: Duration = Duration::from_secs(30);

impl Workspace {
    pub(super) fn toggle_usage(&mut self, cx: &mut Context<Self>) {
        if self.dismissed.take().is_some_and(|(a, at)| a == "usage" && at.elapsed() < Duration::from_millis(400)) {
            return;
        }
        if self.usage_tick.is_some() {
            self.close_usage(cx);
            return;
        }
        self.usage_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub(super) fn usage_open(&self) -> bool {
        self.usage_tick.is_some()
    }

    pub(super) fn close_usage(&mut self, cx: &mut Context<Self>) {
        if self.usage_tick.take().is_some() {
            cx.notify();
        }
    }

    /// The current agent's live five-hour and weekly limits.
    fn usage_limits(&self) -> (Option<RateLimit>, Option<RateLimit>) {
        let now = chrono::Utc::now().timestamp();
        let Some(limits) = self.current_thread().and_then(|t| self.limits.get(&t.agent)) else {
            return (None, None);
        };
        let live = |w: &str| limits.get(w).filter(|l| l.resets_at.is_none_or(|at| at > now)).cloned();
        (live("five_hour"), live("seven_day"))
    }

    pub(super) fn usage_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = palette(cx);
        let (five, week) = self.usage_limits();
        if five.is_none() && week.is_none() {
            return None;
        }
        let now = chrono::Utc::now().timestamp();
        let has_week = week.is_some();
        let small = px(crate::theme::scaled_ui(10.));
        let section = || col().px(px(PAD)).py(px(7.));

        let week_section = week.map(|limit| {
            let used = limit.used.unwrap_or(0.) as f32;
            let resets = limit.resets_at.unwrap_or(now + WEEK);
            let start = resets - WEEK;
            let gone = ((now - start) as f32 / WEEK as f32).clamp(0., 1.);
            let pct_color = match limit.status {
                LimitStatus::Allowed => colors.text,
                LimitStatus::Warning => colors.amber,
                LimitStatus::Rejected => colors.red,
            };
            let date = |at: i64| {
                chrono::DateTime::from_timestamp(at, 0)
                    .map(|d| d.with_timezone(&chrono::Local).format("%b %-d").to_string())
                    .unwrap_or_default()
            };
            section()
                .child(
                    row()
                        .justify_between()
                        .mb(px(4.))
                        .child(limit.short_name())
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(pct_color))
                                .child(limit.used.map(|u| format!("{}%", u.round() as u32)).unwrap_or_else(|| "—".into())),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h(px(BAR))
                        .child(canvas(|_, _, _| {}, move |b, _, window, _| paint_bar(b, used / 100., colors, window)).size_full())
                        // Now, against the week: ahead of it means usage is outpacing time.
                        .child(div().absolute().top(px(-2.)).h(px(BAR + 4.)).left(relative(gone)).ml(px(-1.)).w(px(2.)).rounded(px(1.)).bg(rgb(colors.red))),
                )
                .child(
                    row()
                        .justify_between()
                        .mt(px(3.))
                        .text_size(small)
                        .text_color(rgb(colors.quiet))
                        .child(date(start))
                        .child(date(resets)),
                )
        });

        let five_section = five.map(|limit| {
            let used = limit.used.unwrap_or(0.) as f32;
            let resets = limit.resets_at.unwrap_or(now + FIVE_HOURS);
            let fill = match limit.status {
                LimitStatus::Allowed => colors.fill_blue,
                LimitStatus::Warning => colors.fill_amber,
                LimitStatus::Rejected => colors.fill_red,
            };
            let reset_time = chrono::DateTime::from_timestamp(resets, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%-I:%M %p").to_string())
                .unwrap_or_default();
            let numerals = [(0usize, "12"), (3, "3"), (6, "6"), (9, "9")].map(|(h, n)| {
                let (x, y) = on_dial(h as f32 / 12. * TAU, CLOCK / 2. - CLOCK * 0.117);
                div()
                    .absolute()
                    .left(px(CLOCK / 2. + x - 10.))
                    .top(px(CLOCK / 2. + y - 7.))
                    .w(px(20.))
                    .h(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(crate::theme::scaled_ui(10.)))
                    .text_color(rgb(colors.muted))
                    .child(n)
            });
            let legend = |swatch: Div, name: &'static str, value: String, right: bool| {
                col()
                    .when(right, |s| s.items_end())
                    .child(row().gap(px(5.)).text_size(small).text_color(rgb(colors.muted)).child(swatch).child(name))
                    .child(div().when(!right, |s| s.pl(px(13.))).font_weight(FontWeight::SEMIBOLD).child(value))
            };
            section()
                // A divider, when the bar sits above.
                .when(has_week, |s| s.border_t_1())
                .border_color(rgb(colors.popover_border))
                .child(
                    div()
                        .relative()
                        .size(px(CLOCK))
                        .child(
                            canvas(|_, _, _| {}, move |b, _, window, _| {
                                paint_clock(b, chrono::Local::now(), resets, used / 100., fill, colors, window)
                            })
                            .size_full(),
                        )
                        .children(numerals),
                )
                .child(
                    row()
                        .justify_between()
                        .items_start()
                        .mt(px(4.))
                        .child(legend(div().size(px(8.)).rounded(px(2.)).bg(rgb(fill)), "Used", limit.used.map(|u| format!("{}%", u.round() as u32)).unwrap_or_else(|| "—".into()), false))
                        .child(legend(div().w(px(8.)).h(px(2.)).bg(rgb(colors.red)), "Resets", reset_time, true)),
                )
        });

        Some(
            col()
                .id("usage-popover")
                .debug_selector(|| "usage-popover".into())
                .occlude()
                .w(px(WIDTH))
                .bg(rgb(colors.popover))
                .border_1()
                .border_color(rgb(colors.popover_border))
                .rounded(px(8.))
                .shadow_lg()
                .text_size(px(crate::theme::scaled_ui(11.)))
                .text_color(rgb(colors.text))
                .on_mouse_down_out(cx.listener(|s, _, _, cx| {
                    if s.usage_open() {
                        s.dismissed = Some(("usage", Instant::now()));
                        s.close_usage(cx);
                    }
                }))
                .children(week_section)
                .children(five_section)
                .into_any_element(),
        )
    }
}

/// A point `r` out from the centre, `a` radians clockwise from twelve.
fn on_dial(a: f32, r: f32) -> (f32, f32) {
    (a.sin() * r, -a.cos() * r)
}

fn with_alpha(color: u32, a: f32) -> Hsla {
    Hsla::from(rgb(color)).opacity(a)
}

/// The weekly bar's used part blends from blue into amber, then red at the cap.
const RAMP: [f32; 4] = [0., 0.6, 0.85, 1.];

fn paint_bar(b: Bounds<Pixels>, used: f32, colors: crate::theme::Palette, window: &mut Window) {
    let radius = px(4.);
    let (w, h) = (b.size.width, b.size.height);
    window.paint_quad(fill(b, with_alpha(colors.fill_blue, 0.08)).corner_radii(radius));
    // Hatching for what's left, at 45° whatever the bar's width.
    let used = used.clamp(0., 1.);
    let hatch = Bounds::new(point(b.left() + w * used, b.top()), size(w * (1. - used), h));
    window.with_content_mask(Some(ContentMask { bounds: hatch }), |window| {
        let step = px(5.);
        let mut x = b.left() - h;
        while x < b.right() {
            let mut line = PathBuilder::stroke(px(1.));
            line.move_to(point(x, b.bottom()));
            line.line_to(point(x + h, b.top()));
            if let Ok(path) = line.build() {
                window.paint_path(path, with_alpha(colors.fill_blue, 0.45));
            }
            x += step;
        }
    });
    if used <= 0. {
        return;
    }
    let stops = [colors.fill_blue, colors.fill_blue, colors.fill_amber, colors.fill_red];
    let shown = Bounds::new(b.origin, size(w * used, h));
    window.with_content_mask(Some(ContentMask { bounds: shown }), |window| {
        for i in 0..RAMP.len() - 1 {
            let seg = Bounds::new(point(b.left() + w * RAMP[i], b.top()), size(w * (RAMP[i + 1] - RAMP[i]), h));
            let (from, to) = (rgb(stops[i]), rgb(stops[i + 1]));
            let corners = Corners {
                top_left: if i == 0 { radius } else { px(0.) },
                bottom_left: if i == 0 { radius } else { px(0.) },
                top_right: if i == RAMP.len() - 2 { radius } else { px(0.) },
                bottom_right: if i == RAMP.len() - 2 { radius } else { px(0.) },
            };
            window.paint_quad(fill(seg, linear_gradient(90., linear_color_stop(from, 0.), linear_color_stop(to, 1.))).corner_radii(corners));
        }
    });
}

/// Where the hour hand points at `t`, on a twelve-hour dial.
fn dial_angle(t: chrono::DateTime<chrono::Local>) -> f32 {
    use chrono::Timelike;
    ((t.hour() % 12) as f32 + t.minute() as f32 / 60. + t.second() as f32 / 3600.) / 12. * TAU
}

fn paint_clock(
    b: Bounds<Pixels>,
    now: chrono::DateTime<chrono::Local>,
    resets: i64,
    used: f32,
    fill_color: u32,
    colors: crate::theme::Palette,
    window: &mut Window,
) {
    let size_ = f32::from(b.size.width.min(b.size.height));
    let c = b.center();
    let at = |a: f32, r: f32| {
        let (x, y) = on_dial(a, r);
        point(c.x + px(x), c.y + px(y))
    };
    let k = size_ / DRAWN_FOR;
    let face = size_ / 2. - 1.;
    let wedge_r = face * 50. / 54.;
    let line = |from: Point<Pixels>, to: Point<Pixels>, width: f32, color: Hsla, window: &mut Window| {
        let mut p = PathBuilder::stroke(px(width));
        p.move_to(from);
        p.line_to(to);
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }
    };

    // The window runs five hours back from its reset.
    let reset_at = chrono::DateTime::from_timestamp(resets, 0).map(|d| d.with_timezone(&chrono::Local)).unwrap_or(now);
    let span = FIVE_HOURS as f32 / (12. * 3600.) * TAU;
    let a1 = dial_angle(reset_at);
    let a0 = a1 - span;
    let a_used = a0 + span * used.clamp(0., 1.);

    // What's left: a light tint, hatched.
    if a1 - a_used > 0.001 {
        if let Ok(path) = sector(c, wedge_r, a_used, a1).build() {
            window.paint_path(path, with_alpha(fill_color, 0.1));
        }
        let (dir, normal) = ((0.5f32.sqrt(), -(0.5f32.sqrt())), (0.5f32.sqrt(), 0.5f32.sqrt()));
        let step = 5. * k;
        let mut offset = -wedge_r;
        while offset < wedge_r {
            let p = (normal.0 * offset, normal.1 * offset);
            if let Some((t0, t1)) = clip_to_sector(p, dir, wedge_r, a_used, a1) {
                let from = point(c.x + px(p.0 + dir.0 * t0), c.y + px(p.1 + dir.1 * t0));
                let to = point(c.x + px(p.0 + dir.0 * t1), c.y + px(p.1 + dir.1 * t1));
                line(from, to, 1.1 * k, with_alpha(fill_color, 0.55), window);
            }
            offset += step;
        }
    }
    // What's used: solid.
    if a_used - a0 > 0.001 {
        if let Ok(path) = sector(c, wedge_r, a0, a_used).build() {
            window.paint_path(path, with_alpha(fill_color, 0.9));
        }
    }

    // The face.
    let ring = Bounds::new(point(c.x - px(face), c.y - px(face)), size(px(face * 2.), px(face * 2.)));
    window.paint_quad(quad(ring, px(face), transparent_black(), px(1.5 * k), rgb(colors.popover_border), BorderStyle::Solid));
    for i in 0..12 {
        let a = i as f32 / 12. * TAU;
        let major = i % 3 == 0;
        let len = if major { 11. } else { 6. } * k;
        let color = if major { colors.muted } else { colors.quiet };
        line(at(a, face - len), at(a, face), if major { 2. } else { 1.2 } * k, rgb(color).into(), window);
    }

    // The reset, in red.
    line(at(a1, 15. * k), at(a1, face + 4. * k), 2. * k, rgb(colors.red).into(), window);

    // The hands, at the time now.
    use chrono::Timelike;
    let minute = (now.minute() as f32 + now.second() as f32 / 60.) / 60. * TAU;
    let hand = Hsla::from(rgb(colors.text));
    line(c, at(dial_angle(now), face * 0.52), 3.5 * k, hand, window);
    line(c, at(minute, face * 0.74), 2.5 * k, hand, window);
    let dot = |r: f32, color: Hsla, window: &mut Window| {
        let b = Bounds::new(point(c.x - px(r), c.y - px(r)), size(px(r * 2.), px(r * 2.)));
        window.paint_quad(fill(b, color).corner_radii(px(r)));
    };
    dot(4.3 * k, hand, window);
    dot(1.8 * k, rgb(colors.popover).into(), window);
}

/// A pie slice from `a0` to `a1`, clockwise.
fn sector(c: Point<Pixels>, r: f32, a0: f32, a1: f32) -> PathBuilder {
    let at = |a: f32| {
        let (x, y) = on_dial(a, r);
        point(c.x + px(x), c.y + px(y))
    };
    let mut p = PathBuilder::fill();
    p.move_to(c);
    p.line_to(at(a0));
    p.arc_to(point(px(r), px(r)), px(0.), a1 - a0 > PI, true, at(a1));
    p.close();
    p
}

/// The part of the line `p + t·d` inside the slice from `a0` to `a1` (at most
/// half a turn, so convex) of radius `r` about the origin, as a range of `t`.
fn clip_to_sector(p: (f32, f32), d: (f32, f32), r: f32, a0: f32, a1: f32) -> Option<(f32, f32)> {
    // Inside the circle.
    let (b, cc) = (p.0 * d.0 + p.1 * d.1, p.0 * p.0 + p.1 * p.1 - r * r);
    let disc = b * b - cc;
    if disc <= 0. {
        return None;
    }
    let (mut t0, mut t1) = (-b - disc.sqrt(), -b + disc.sqrt());
    // Clockwise of the first edge, and anticlockwise of the second.
    let cross = |u: (f32, f32), v: (f32, f32)| u.0 * v.1 - u.1 * v.0;
    for (a, sign) in [(a0, 1.), (a1, -1.)] {
        let u = on_dial(a, 1.);
        let (k, m) = (sign * cross(u, p), sign * cross(u, d));
        // k + t·m >= 0
        if m.abs() < 1e-6 {
            if k < 0. {
                return None;
            }
        } else if m > 0. {
            t0 = t0.max(-k / m);
        } else {
            t1 = t1.min(-k / m);
        }
    }
    (t1 > t0).then_some((t0, t1))
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
    use super::clip_to_sector;
    use std::f32::consts::PI;

    #[test]
    fn clips_lines_to_the_slice() {
        // A quarter from twelve to three: the upper right of the circle.
        let (a0, a1) = (0., PI / 2.);
        // A horizontal line just above the centre crosses it from x = 0 to the rim.
        let (t0, t1) = clip_to_sector((0., -1.), (1., 0.), 10., a0, a1).unwrap();
        assert!(t0.abs() < 1e-4 && (t1 - 99f32.sqrt()).abs() < 1e-4);
        // Below the centre it misses.
        assert!(clip_to_sector((0., 1.), (1., 0.), 10., a0, a1).is_none());
    }
}
