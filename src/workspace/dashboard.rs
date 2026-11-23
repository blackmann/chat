//! The dashboard: what the agents have spent and done, across every project.
//! It opens from "Chat" in the title bar, in place of the threads and tabs.
//! Its numbers come from the turns kept in the database (see `crate::stats`);
//! this only lays them out.
use super::ui::*;
use super::*;
use crate::stats::{History, Range, Stats, Unit};
use gpui_component::tooltip::Tooltip;
use chrono::Datelike;
use std::f32::consts::TAU;

/// Widest the page grows; past this it centres.
const PAGE: f32 = 1120.;
const CLOCK: f32 = 212.;
const SPEND_H: f32 = 210.;
const TRIO_H: f32 = 104.;

pub(super) struct Dashboard {
    pub(super) range: Range,
    /// Everything kept, once read.
    history: Option<Rc<History>>,
    pub(super) stats: Option<Rc<Stats>>,
    failed: Option<String>,
    scroll: ScrollHandle,
    _load: Option<Task<()>>,
    /// The hour under the pointer on the clock.
    hour: Option<usize>,
    /// The day of the month under the pointer on the spend chart.
    day: Option<usize>,
    clock_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
    spend_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
}

impl Workspace {
    pub(super) fn toggle_dashboard(&mut self, cx: &mut Context<Self>) {
        if self.dashboard.take().is_none() {
            self.open_dashboard(cx);
        }
        cx.notify();
    }

    pub(super) fn dashboard_open(&self) -> bool {
        self.dashboard.is_some()
    }

    pub(super) fn close_dashboard(&mut self, cx: &mut Context<Self>) {
        if self.dashboard.take().is_some() {
            cx.notify();
        }
    }

    fn open_dashboard(&mut self, cx: &mut Context<Self>) {
        // Turns finished since the last save count too.
        self.save_now();
        let db = self.store.as_ref().map(|s| s.path().to_path_buf());
        let load = db.map(|db| {
            cx.spawn(async move |this, cx| {
                let read = cx.background_executor().spawn(async move { crate::store::history(&db) }).await;
                let _ = this.update(cx, |s, cx| {
                    if let Some(d) = &mut s.dashboard {
                        match read {
                            Ok(history) => {
                                let history = Rc::new(history);
                                d.stats = Some(Rc::new(Stats::compute(&history, d.range, chrono::Local::now())));
                                d.history = Some(history);
                            }
                            Err(e) => d.failed = Some(format!("Couldn't read usage: {e}")),
                        }
                        cx.notify();
                    }
                });
            })
        });
        self.dashboard = Some(Dashboard {
            range: Range::Month,
            history: None,
            stats: None,
            failed: self.store.is_none().then(|| "Usage isn't kept: the database couldn't be opened.".into()),
            scroll: ScrollHandle::new(),
            _load: load,
            hour: None,
            day: None,
            clock_bounds: Rc::default(),
            spend_bounds: Rc::default(),
        });
    }

    pub(super) fn set_range(&mut self, range: Range, cx: &mut Context<Self>) {
        let Some(d) = &mut self.dashboard else { return };
        d.range = range;
        if let Some(history) = &d.history {
            d.stats = Some(Rc::new(Stats::compute(history, range, chrono::Local::now())));
        }
        cx.notify();
    }

    /// Where a saved thread is listed now, as (project, thread).
    fn find_stored(&self, id: i64) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(p, project)| project.threads.iter().position(|t| t.stored == Some(id)).map(|t| (p, t)))
    }

    fn open_stored(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some((project, thread)) = self.find_stored(id) else { return };
        self.select_project(project, cx);
        self.select_thread(thread, cx);
        self.dashboard = None;
        cx.notify();
    }

    pub(super) fn dashboard_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let Some(d) = &self.dashboard else { return div().into_any_element() };
        let page = col().w_full().max_w(px(PAGE)).px(px(40.)).pt(px(32.)).pb(px(48.));
        let page = match (&d.stats, &d.failed) {
            (_, Some(why)) => page.child(notice(colors, "Nothing to show", why.clone())),
            (None, None) => page.child(row().gap_2().text_color(rgb(colors.muted)).child(spinner(colors)).child("Reading usage…")),
            (Some(s), None) if s.empty => page.child(self.dash_header(s, cx)).child(notice(
                colors,
                "Nothing counted yet",
                "Usage is kept from now on: each turn an agent finishes adds its tokens, spend and tools here.".into(),
            )),
            (Some(s), None) => page
                .child(self.dash_header(s, cx))
                .child(figures(colors, s))
                .child(heat_section(colors, s))
                .child(
                    row()
                        .mt(px(44.))
                        .gap(px(40.))
                        .items_start()
                        .child(div().w(relative(0.42)).flex_shrink_0().child(self.rhythm_section(colors, s, d, cx)))
                        .child(div().flex_1().min_w_0().child(self.spend_section(colors, s, d, cx))),
                )
                .child(
                    row()
                        .mt(px(44.))
                        .gap(px(40.))
                        .items_start()
                        .child(div().flex_1().min_w_0().child(projects_section(colors, s)))
                        .child(div().w(relative(0.38)).flex_shrink_0().child(mix_section(colors, s))),
                )
                .child(trio(colors, s))
                .child(self.bests_section(colors, s, cx)),
        };
        div()
            .id("dashboard")
            .debug_selector(|| "dashboard".into())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&d.scroll)
            .bg(rgb(colors.background))
            .child(row().w_full().justify_center().child(page))
            .into_any_element()
    }

    fn dash_header(&self, s: &Stats, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let current = s.range;
        let ranges = row().flex_shrink_0().border_1().border_color(rgb(colors.border)).rounded(px(6.)).overflow_hidden().children(
            Range::ALL.iter().enumerate().map(|(i, &range)| {
                let on = range == current;
                div()
                    .id(("dash-range", i))
                    .px(px(11.))
                    .py(px(3.))
                    .text_size(px(crate::theme::scaled_ui(12.)))
                    .text_color(rgb(if on { colors.text } else { colors.muted }))
                    .when(i > 0, |s| s.border_l_1().border_color(rgb(colors.border)))
                    .when(on, |s| s.bg(rgb(colors.selected)))
                    .when(!on, |s| s.hover(|s| s.bg(rgb(colors.hover))).cursor_pointer())
                    .on_click(cx.listener(move |s, _, _, cx| s.set_range(range, cx)))
                    .child(range.label())
            }),
        );
        row()
            .items_start()
            .gap(px(24.))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .child(eyebrow(colors, s.range.title()))
                    .when(!s.empty, |c| c.child(lede(colors, s))),
            )
            .child(ranges)
    }

    fn rhythm_section(&self, colors: Palette, s: &Stats, d: &Dashboard, cx: &mut Context<Self>) -> Div {
        let hours = s.hours;
        let unit = s.unit;
        let shown = d.hour.or(s.peak_hour());
        let peak = s.peak_hour();
        let hovered = d.hour;
        let bounds = d.clock_bounds.clone();
        let now = chrono::Local::now();
        let hour_name = |h: usize| format!("{}{}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "a" } else { "p" });
        let labels = [(0usize, "12a"), (6, "6a"), (12, "12p"), (18, "6p")].map(|(h, t)| {
            let a = h as f32 / 24. * TAU;
            let r = CLOCK / 2. + 2.;
            div()
                .absolute()
                .left(px(CLOCK / 2. + 12. + a.sin() * r - 14.))
                .top(px(CLOCK / 2. + 12. - a.cos() * r - 7.))
                .w(px(28.))
                .h(px(14.))
                .flex()
                .justify_center()
                .text_size(px(crate::theme::scaled_ui(10.)))
                .text_color(rgb(colors.quiet))
                .child(t)
        });
        let center = col()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .child(div().text_size(px(crate::theme::scaled_ui(10.))).text_color(rgb(colors.muted)).child(if hovered.is_some() { unit.word() } else { "peak" }))
            .child(div().text_size(px(crate::theme::scaled_ui(15.))).font_weight(FontWeight::SEMIBOLD).child(match (hovered, shown) {
                (Some(h), _) => amount(unit, hours[h]),
                (None, Some(h)) => format!("{}–{}", hour_name(h), hour_name((h + 1) % 24)),
                _ => "—".into(),
            }))
            .when_some(hovered, |c, h| {
                c.child(div().text_size(px(crate::theme::scaled_ui(10.))).text_color(rgb(colors.quiet)).child(format!("{}–{}", hour_name(h), hour_name((h + 1) % 24))))
            });
        let clock = div()
            .id("dash-clock")
            .relative()
            .flex_shrink_0()
            .size(px(CLOCK + 24.))
            .child(
                canvas(
                    move |b, _, _| bounds.set(b),
                    move |b, _, window, _| paint_rhythm(b, &hours, peak, hovered, now, colors, window),
                )
                .absolute()
                .left(px(12.))
                .top(px(12.))
                .size(px(CLOCK)),
            )
            .children(labels)
            .child(center)
            .on_mouse_move(cx.listener(|s, e: &MouseMoveEvent, _, cx| {
                let Some(d) = &mut s.dashboard else { return };
                let b = d.clock_bounds.get();
                let (dx, dy) = (f32::from(e.position.x - b.center().x), f32::from(e.position.y - b.center().y));
                let r = (dx * dx + dy * dy).sqrt();
                let hour = (r > CLOCK * 0.2 && r < CLOCK * 0.56).then(|| ((dx.atan2(-dy) + TAU) % TAU / TAU * 24.) as usize % 24);
                if hour != d.hour {
                    d.hour = hour;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|s, over: &bool, _, cx| {
                if !over && let Some(d) = &mut s.dashboard {
                    d.hour = None;
                    cx.notify();
                }
            }));
        let weekday_names = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        let busiest = (0..7).max_by_key(|i| s.weekdays[*i]).filter(|i| s.weekdays[*i] > 0);
        let quietest = (0..7).filter(|i| s.weekdays[*i] > 0).min_by_key(|i| s.weekdays[*i]);
        let facts = [
            ("After 9 pm", format!("{}% of {}", (s.late_share() * 100.).round(), unit.word())),
            ("Median turn", fmt_duration(s.median_turn_ms)),
            ("Busiest day", busiest.map_or("—".into(), |i| weekday_names[i].to_string())),
            ("Quietest day", quietest.map_or("—".into(), |i| weekday_names[i].to_string())),
        ];
        let max_day = s.weekdays.iter().copied().max().unwrap_or(0).max(1);
        let week = row().mt(px(16.)).h(px(58.)).gap(px(4.)).items_end().children((0..7).map(|i| {
            let v = s.weekdays[i];
            let top = Some(i) == busiest;
            col()
                .id(("dash-weekday", i))
                .flex_1()
                .h_full()
                .justify_end()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .w_full()
                        .h(px((v as f32 / max_day as f32 * 40.).max(if v > 0 { 2. } else { 0. })))
                        .rounded_t(px(3.))
                        .bg(if top { rgb(colors.blue) } else { mix(colors.row_border, colors.blue, 0.6) }),
                )
                .child(div().text_size(px(crate::theme::scaled_ui(10.))).text_color(rgb(colors.quiet)).child(["S", "M", "T", "W", "T", "F", "S"][i]))
                .tooltip(move |w, cx| Tooltip::new(format!("{} · {} {}", weekday_names[i], amount(unit, v), unit.word())).build(w, cx))
        }));
        col()
            .child(section_head(colors, "Daily rhythm", format!("{} by the hour they start", match unit {
                Unit::Tokens => "tokens of turns",
                Unit::Turns => "turns",
            })))
            .child(
                row().gap(px(20.)).items_center().child(clock).child(
                    col()
                        .flex_1()
                        .min_w_0()
                        .children(facts.into_iter().map(|(k, v)| {
                            row()
                                .justify_between()
                                .gap_2()
                                .py(px(7.))
                                .border_b_1()
                                .border_color(rgb(colors.row_border))
                                .text_size(px(crate::theme::scaled_ui(12.)))
                                .child(div().text_color(rgb(colors.muted)).child(k))
                                .child(div().whitespace_nowrap().child(v))
                        }))
                        .child(week),
                ),
            )
    }

    fn spend_section(&self, colors: Palette, s: &Stats, d: &Dashboard, cx: &mut Context<Self>) -> Div {
        let spend = s.spend.clone();
        let projected = spend.projected().unwrap_or(0.);
        let last_total = spend.last.as_ref().and_then(|(l, _)| l.last().copied());
        let top = spend.this.last().copied().unwrap_or(0.).max(projected).max(last_total.unwrap_or(0.));
        let step = nice_step(top);
        let ymax = ((top / step).ceil() * step).max(step);
        let currency = s.currency.clone();
        let hover = d.day;
        let bounds = d.spend_bounds.clone();
        let legend = row()
            .gap(px(14.))
            .text_size(px(crate::theme::scaled_ui(11.)))
            .text_color(rgb(colors.muted))
            .child(row().gap(px(5.)).child(div().w(px(14.)).h(px(2.)).bg(rgb(colors.text))).child(spend.month.clone()))
            .when(spend.last.is_some(), |r| r.child(row().gap(px(5.)).child(div().w(px(14.)).h(px(2.)).bg(rgb(colors.quiet))).child(spend.last_month.clone())));
        // Axis and end labels, over the chart.
        let (left, right, top_pad, bottom) = (44., 72., 10., 22.);
        let mut labels: Vec<AnyElement> = Vec::new();
        let small = px(crate::theme::scaled_ui(10.));
        let mut v = 0.;
        while v <= ymax + 1e-9 {
            let y = top_pad + (1. - v / ymax) as f32 * (SPEND_H - top_pad - bottom);
            labels.push(
                div().absolute().left_0().w(px(left - 8.)).top(px(y - 7.)).flex().justify_end().text_size(small).text_color(rgb(colors.quiet)).child(fmt_money(v, &currency)).into_any_element(),
            );
            v += step;
        }
        let month_short = spend.month.chars().take(3).collect::<String>();
        let mut ticks: Vec<usize> = vec![1, 8, 15, 22];
        if spend.days as usize - 22 >= 4 {
            ticks.push(spend.days as usize);
        }
        labels.push(
            div()
                .absolute()
                .top(px(SPEND_H - bottom + 6.))
                .left(px(left))
                .right(px(right))
                .h(px(14.))
                .children(ticks.into_iter().map(|day| {
                    let f = (day - 1) as f32 / (spend.days.max(2) - 1) as f32;
                    div()
                        .absolute()
                        .left(relative(f))
                        .w(px(48.))
                        .ml(px(-24.))
                        .flex()
                        .justify_center()
                        .text_size(small)
                        .text_color(rgb(colors.quiet))
                        .child(format!("{month_short} {day}"))
                }))
                .into_any_element(),
        );
        let end = |value: f64, name: String, strong: bool| {
            let value = value.clamp(0., ymax);
            // Kept clear of the date labels along the bottom.
            let y = (top_pad + (1. - value / ymax) as f32 * (SPEND_H - top_pad - bottom)).min(SPEND_H - bottom - 22.);
            col()
                .absolute()
                .right_0()
                .w(px(right - 8.))
                .top(px(y - 8.))
                .child(div().text_size(px(crate::theme::scaled_ui(11.))).font_weight(FontWeight::SEMIBOLD).text_color(rgb(if strong { colors.text } else { colors.muted })).child(fmt_money(value, &currency)))
                .child(div().text_size(small).text_color(rgb(colors.quiet)).child(name))
                .into_any_element()
        };
        if let Some(last) = last_total {
            labels.push(end(last, spend.last_month.clone(), false));
        }
        if spend.this.last().is_some() {
            // Keep the two end labels apart when the lines end close together.
            let at = match last_total {
                Some(last) if (projected - last).abs() / ymax < 0.12 => projected + if projected >= last { 0.12 * ymax } else { -0.12 * ymax },
                _ => projected,
            };
            labels.push(end(at.clamp(0., ymax), "projected".into(), true));
        }
        let readout = hover.and_then(|day| {
            let this = spend.this.get(day).copied();
            let last = spend.last.as_ref().and_then(|(l, from)| (day >= *from).then(|| l.get(day).copied()).flatten());
            let main = spend.at(day)?;
            Some(
                col()
                    .absolute()
                    .top_0()
                    .left(px(left + 6.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .bg(rgb(colors.popover))
                    .border_1()
                    .border_color(rgb(colors.popover_border))
                    .text_size(px(crate::theme::scaled_ui(11.)))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{month_short} {}{}", day + 1, if this.is_none() { " · projected" } else { "" })))
                    .child(
                        div().text_color(rgb(colors.muted)).child(format!(
                            "{}{}",
                            fmt_money(main, &currency),
                            last.map_or(String::new(), |l| format!(" · {} {}", spend.last_month.chars().take(3).collect::<String>(), fmt_money(l, &currency)))
                        )),
                    ),
            )
        });
        let painted = spend.clone();
        let chart = div()
            .id("dash-spend")
            .relative()
            .w_full()
            .h(px(SPEND_H))
            .child(
                canvas(
                    move |b, _, _| bounds.set(b),
                    move |b, _, window, _| paint_spend(b, &painted, ymax, step, hover, (left, right, top_pad, bottom), colors, window),
                )
                .size_full(),
            )
            .children(labels)
            .children(readout)
            .on_mouse_move(cx.listener(move |s, e: &MouseMoveEvent, _, cx| {
                let Some(d) = &mut s.dashboard else { return };
                let b = d.spend_bounds.get();
                let w = f32::from(b.size.width) - left - right;
                let x = f32::from(e.position.x - b.left()) - left;
                let days = s.dashboard_days();
                let day = (w > 0. && x >= -6. && x <= w + 6.).then(|| ((x / w * (days - 1) as f32).round().clamp(0., (days - 1) as f32)) as usize);
                if let Some(d) = &mut s.dashboard
                    && day != d.day
                {
                    d.day = day;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|s, over: &bool, _, cx| {
                if !over && let Some(d) = &mut s.dashboard {
                    d.day = None;
                    cx.notify();
                }
            }));
        let so_far = spend.this.last().copied().unwrap_or(0.);
        col()
            .child(row().justify_between().items_center().mb(px(14.)).child(heading(format!("Spend in {}", spend.month))).child(legend))
            .child(chart)
            .child(
                row()
                    .mt(px(6.))
                    .gap(px(16.))
                    .text_size(px(crate::theme::scaled_ui(11.)))
                    .text_color(rgb(colors.muted))
                    .child(format!("{} so far", fmt_money(so_far, &s.currency)))
                    .child(format!("{} at this pace", fmt_money(projected, &s.currency))),
            )
    }

    fn dashboard_days(&self) -> usize {
        self.dashboard.as_ref().and_then(|d| d.stats.as_ref()).map_or(30, |s| s.spend.days.max(2) as usize)
    }

    fn bests_section(&self, colors: Palette, s: &Stats, cx: &mut Context<Self>) -> Div {
        let date = |ms: i64| crate::stats::local_date(ms).format("%b %-d").to_string();
        let best = |key: &'static str, k: &'static str, b: &Option<crate::stats::ThreadBest>, value: &dyn Fn(f64) -> String| -> AnyElement {
            let Some(b) = b else { return record(colors, k, "—".into(), "Nothing yet".into(), String::new(), None) };
            let link = self.find_stored(b.thread).map(|_| {
                let id = b.thread;
                div()
                    .id(key)
                    .mt(px(6.))
                    .text_size(px(crate::theme::scaled_ui(11.)))
                    .text_color(rgb(colors.blue))
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |s, _, _, cx| s.open_stored(id, cx)))
                    .child("Open thread →")
                    .into_any_element()
            });
            record(colors, k, value(b.value), b.title.clone().unwrap_or_else(|| "A deleted thread".into()), format!("{} · {}", b.project, date(b.at)), link)
        };
        let currency = s.currency.clone();
        let big = s.bests.biggest_day.as_ref().map(|d| {
            record(
                colors,
                "Biggest day",
                fmt_tokens(d.tokens),
                d.date.format("%a, %b %-d, %Y").to_string(),
                format!("tokens across {} thread{}", d.threads, if d.threads == 1 { "" } else { "s" }),
                None,
            )
        });
        col().mt(px(44.)).child(section_head(colors, "Personal bests", "all time")).child(
            row()
                .items_start()
                .child(best("best-longest", "Longest thread", &s.bests.longest, &|v| fmt_duration(v as i64)))
                .child(best("best-priciest", "Priciest thread", &s.bests.priciest, &|v| fmt_money(v, &currency)))
                .child(best("best-files", "Most files edited", &s.bests.most_files, &|v| format!("{}", v as u64)))
                .children(big),
        )
    }
}

// ── Pieces ─────────────────────────────────────────────────────────────────

fn small(size: f32) -> Pixels {
    px(crate::theme::scaled_ui(size))
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().text_size(small(13.)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

fn section_head(colors: Palette, title: &str, sub: impl Into<SharedString>) -> Div {
    row()
        .justify_between()
        .items_baseline()
        .mb(px(14.))
        .child(heading(title.to_string()))
        .child(div().text_size(small(12.)).text_color(rgb(colors.muted)).child(sub.into()))
}

fn eyebrow(colors: Palette, text: &str) -> Div {
    div().mb(px(10.)).text_size(small(11.)).text_color(rgb(colors.quiet)).child(text.to_uppercase())
}

fn notice(colors: Palette, title: &str, body: String) -> Div {
    col()
        .mt(px(60.))
        .items_center()
        .gap(px(6.))
        .child(div().text_size(small(15.)).font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
        .child(div().max_w(px(420.)).text_center().text_color(rgb(colors.muted)).child(body))
}

/// The opening sentence, its figures in bold.
fn lede(colors: Palette, s: &Stats) -> Div {
    let mut text = String::new();
    let mut marks: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let strong = HighlightStyle { color: Some(rgb(colors.text).into()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
    let mut say = |t: &str, bold: bool| {
        if bold {
            marks.push((text.len()..text.len() + t.len(), strong));
        }
        text.push_str(t);
    };
    match s.unit {
        Unit::Tokens => {
            say(&format!("{} you moved ", s.range.phrase()), false);
            say(&format!("{} tokens", fmt_tokens(s.tokens)), true);
        }
        Unit::Turns => {
            say(&format!("{} you ran ", s.range.phrase()), false);
            say(&plural(s.turns, "turn"), true);
        }
    }
    say(" through ", false);
    say(&plural(s.threads, "thread"), true);
    say(" across ", false);
    say(&plural(s.projects, "project"), true);
    if s.cost > 0. {
        say(", for about ", false);
        say(&fmt_money(s.cost, &s.currency), true);
    }
    say(".", false);
    if let (Unit::Turns, Some(since)) = (s.unit, s.tokens_since) {
        say(&format!(" Since tokens began to be counted on {}, ", since.format("%b %-d")), false);
        say(&format!("{} tokens", fmt_tokens(s.tokens)), true);
        say(".", false);
    }
    if let Some(part) = s.part_of_day() {
        say(" Most of it happens ", false);
        say(part, true);
        if let Some(top) = &s.top_project
            && s.projects > 1
        {
            say(", mostly in ", false);
            say(top, true);
        }
        say(".", false);
    }
    div()
        .max_w(px(780.))
        .text_size(small(24.))
        .line_height(relative(1.38))
        .text_color(rgb(colors.muted))
        .child(StyledText::new(text).with_highlights(marks))
}

fn figures(colors: Palette, s: &Stats) -> Div {
    let delta = s.prior_tokens.filter(|p| *p > 0).map(|p| {
        let change = (s.tokens as f64 - p as f64) / p as f64 * 100.;
        format!("{}{:.0}% vs the {} before", if change >= 0. { "+" } else { "−" }, change.abs(), s.range.label())
    });
    let decided = s.allowed + s.denied;
    let cells = [
        (
            "Tokens",
            fmt_tokens(s.tokens),
            String::new(),
            match (s.unit, s.tokens_since) {
                (Unit::Turns, Some(since)) => format!("counted since {}", since.format("%b %-d")),
                _ => delta.unwrap_or_else(|| plural(s.turns, "turn")),
            },
        ),
        (
            "Spend",
            fmt_money(s.cost, &s.currency),
            String::new(),
            if s.active_days > 0 { format!("{} per active day", fmt_money(s.cost / s.active_days as f64, &s.currency)) } else { String::new() },
        ),
        ("Active days", s.active_days.to_string(), format!(" / {}", s.span_days), format!("{:.0}% of days", s.active_days as f64 / s.span_days.max(1) as f64 * 100.)),
        ("Agent time", fmt_hours(s.agent_ms), String::new(), format!("median turn {}", fmt_duration(s.median_turn_ms))),
        (
            "Approvals",
            decided.to_string(),
            String::new(),
            if decided > 0 { format!("{:.0}% allowed · {} denied", s.allowed as f64 / decided as f64 * 100., s.denied) } else { "none asked".into() },
        ),
    ];
    row().mt(px(28.)).border_t_1().border_color(rgb(colors.border)).items_start().children(cells.into_iter().enumerate().map(|(i, (k, v, unit, note))| {
        col()
            .flex_1()
            .min_w_0()
            .pt(px(14.))
            .pr(px(16.))
            .when(i > 0, |c| c.pl(px(18.)).border_l_1().border_color(rgb(colors.row_border)))
            .child(div().text_size(small(11.)).text_color(rgb(colors.muted)).child(k))
            .child(
                row()
                    .items_baseline()
                    .mt(px(2.))
                    .child(div().text_size(small(24.)).font_weight(FontWeight::SEMIBOLD).child(v))
                    .child(div().text_size(small(13.)).text_color(rgb(colors.muted)).child(unit)),
            )
            .child(div().text_size(small(11.)).text_color(rgb(colors.quiet)).truncate().child(note))
    }))
}

fn heat_section(colors: Palette, s: &Stats) -> Div {
    const CELL: f32 = 11.;
    // Some days on the grid are from before tokens were counted.
    let mixed = s.heat.iter().any(|d| d.turns > 0 && !d.counted);
    let level_color = |l: u8| match l {
        0 => rgb(colors.row_border),
        1 => mix(colors.row_border, colors.blue, 0.28),
        2 => mix(colors.row_border, colors.blue, 0.52),
        3 => mix(colors.row_border, colors.blue, 0.76),
        _ => rgb(colors.blue),
    };
    let weeks = s.heat.len() / 7;
    let mut months: Vec<AnyElement> = Vec::new();
    let mut last_month = None;
    for w in 0..weeks.saturating_sub(2) {
        let day = s.heat[w * 7].date;
        if last_month != Some(day.month()) && day.day() <= 7 {
            last_month = Some(day.month());
            months.push(
                div().absolute().left(relative(w as f32 / weeks as f32)).text_size(small(10.)).text_color(rgb(colors.quiet)).child(day.format("%b").to_string()).into_any_element(),
            );
        }
    }
    let columns = (0..weeks).map(|w| {
        col().flex_1().min_w(px(4.)).gap(px(3.)).children((0..7).map(|d| {
            let i = w * 7 + d;
            let day = &s.heat[i];
            let cell = div().id(("heat", i)).h(px(CELL)).w_full().rounded(px(2.));
            if day.future {
                return cell.border_1().border_dashed().border_color(rgb(colors.row_border));
            }
            let tip = day.clone();
            cell.bg(level_color(s.level(day))).hover(|s| s.border_1().border_color(rgb(colors.text))).tooltip(move |w, cx| {
                let tip = tip.clone();
                Tooltip::element(move |_, _| heat_tip(colors, &tip)).build(w, cx)
            })
        }))
    });
    let days = col().w(px(26.)).flex_shrink_0().gap(px(3.)).children(["", "Mon", "", "Wed", "", "Fri", ""].map(|t| div().h(px(CELL)).text_size(small(9.)).line_height(px(CELL)).text_color(rgb(colors.quiet)).child(t)));
    let legend = row()
        .justify_end()
        .mt(px(10.))
        .gap(px(3.))
        .text_size(small(10.))
        .text_color(rgb(colors.quiet))
        .child(div().mr(px(3.)).child(if mixed { "Less" } else { "Fewer tokens" }))
        .children((0..5).map(|l| div().size(px(CELL)).rounded(px(2.)).bg(level_color(l))))
        .child(div().ml(px(3.)).child("More"));
    let streak_bar = |f: f32| div().mt(px(5.)).h(px(4.)).w_full().rounded(px(2.)).bg(rgb(colors.row_border)).child(div().h_full().w(relative(f.clamp(0., 1.))).rounded(px(2.)).bg(rgb(colors.blue)));
    let stat = |k: &str, v: String, unit: String| {
        col()
            .child(div().text_size(small(11.)).text_color(rgb(colors.muted)).child(k.to_string()))
            .child(row().items_baseline().gap(px(4.)).child(div().text_size(small(17.)).font_weight(FontWeight::SEMIBOLD).child(v)).child(div().text_size(small(12.)).text_color(rgb(colors.muted)).child(unit)))
    };
    let days_word = |n: u32| if n == 1 { "day" } else { "days" }.to_string();
    let side = col()
        .w(px(200.))
        .flex_shrink_0()
        .pt(px(16.))
        .gap(px(14.))
        .child(stat("Current streak", s.streak.to_string(), days_word(s.streak)).child(streak_bar(s.streak as f32 / s.longest_streak.max(1) as f32)))
        .child(stat("Longest streak", s.longest_streak.to_string(), days_word(s.longest_streak)).child(streak_bar(1.)))
        .when_some(s.bests.biggest_day.as_ref(), |c, d| c.child(stat("Biggest day", fmt_tokens(d.tokens), d.date.format("%b %-d, %Y").to_string())));
    let active = s.heat.iter().filter(|d| d.turns > 0).count();
    let counted = s.heat.iter().filter(|d| d.counted).count();
    let per_day = if counted > 0 { s.heat.iter().filter(|d| d.counted).map(|d| d.tokens).sum::<u64>() / counted as u64 } else { 0 };
    let sub = match s.tokens_since.filter(|_| mixed) {
        Some(since) => format!("{active} active days in the last year · shaded by turns before {}, by tokens since", since.format("%b %-d")),
        None => format!("{active} active days in the last year · {} tokens on an average one", fmt_tokens(per_day)),
    };
    col()
        .mt(px(44.))
        .child(section_head(colors, "Activity", sub))
        .child(
            row()
                .items_start()
                .gap(px(32.))
                .child(
                    col()
                        .flex_1()
                        .min_w_0()
                        .child(row().child(div().w(px(26.)).flex_shrink_0()).child(div().relative().flex_1().h(px(14.)).children(months)))
                        .child(row().mt(px(4.)).items_start().child(days).child(row().flex_1().min_w_0().gap(px(3.)).children(columns)))
                        .child(legend),
                )
                .child(side),
        )
}

/// A day on the grid, up close: the same width every time, so it doesn't
/// jump about as the pointer crosses the grid.
fn heat_tip(colors: Palette, day: &crate::stats::Day) -> Div {
    const SHOWN: usize = 4;
    let total = if day.turns == 0 {
        "Nothing".to_string()
    } else if day.counted {
        format!("{} tokens", fmt_tokens(day.tokens))
    } else {
        plural(day.turns as usize, "turn")
    };
    let detail = match (day.turns, day.counted) {
        (0, _) => None,
        (_, true) => Some(format!("{} · {}", plural(day.turns as usize, "turn"), plural(day.threads as usize, "thread"))),
        (_, false) => Some(format!("{} · before tokens were counted", plural(day.threads as usize, "thread"))),
    };
    let projects = day.projects.iter().take(SHOWN).map(|(path, tokens, turns)| {
        let amount = if *tokens > 0 { fmt_tokens(*tokens) } else { plural(*turns as usize, "turn") };
        row()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().truncate().text_color(rgb(colors.text)).child(crate::stats::project_name(path)))
            .child(div().flex_shrink_0().text_color(rgb(colors.muted)).child(amount))
    });
    let more = day.projects.len().saturating_sub(SHOWN);
    col()
        .w(px(216.))
        .py(px(4.))
        .gap(px(2.))
        .text_size(small(12.))
        .child(
            row()
                .gap(px(8.))
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_color(rgb(colors.text)).child(day.date.format("%a, %b %-d").to_string()))
                .child(div().flex_shrink_0().text_color(rgb(colors.text)).child(total)),
        )
        .when_some(detail, |c, d| c.child(div().text_size(small(11.)).text_color(rgb(colors.quiet)).child(d)))
        .when(!day.projects.is_empty(), |c| {
            c.child(div().my(px(4.)).h(px(1.)).bg(rgb(colors.row_border)))
                .children(projects)
                .when(more > 0, |c| c.child(div().text_size(small(11.)).text_color(rgb(colors.quiet)).child(format!("+ {more} more"))))
        })
}

fn projects_section(colors: Palette, s: &Stats) -> Div {
    let top = s.projects_top.first().map_or(1, |p| p.weight).max(1);
    let all: u64 = s.projects_top.iter().chain(&s.projects_rest).map(|p| p.weight).sum::<u64>().max(1);
    let by_turns = s.unit == Unit::Turns;
    let header = row()
        .pb(px(8.))
        .border_b_1()
        .border_color(rgb(colors.border))
        .text_size(small(11.))
        .text_color(rgb(colors.quiet))
        .child(div().w(px(22.)))
        .child(div().flex_1().child("Project"))
        .child(div().w(px(110.)).child("Share"))
        .child(div().w(px(64.)).text_right().child(if by_turns { "Turns" } else { "Tokens" }))
        .child(div().w(px(64.)).text_right().child("Spend"))
        .child(div().w(px(60.)).text_right().child("Threads"))
        .child(div().w(px(100.)).pl(px(16.)).child("30 days"));
    let rows = s.projects_top.iter().map(|p| (p, false)).chain(s.projects_rest.iter().map(|p| (p, true))).enumerate().map(|(i, (p, rest))| {
        let tip = format!("{} · {:.0}% of {} · {} tokens · last active {}", p.name, p.weight as f64 / all as f64 * 100., s.unit.word(), fmt_tokens(p.tokens), ago(p.last_active));
        let daily = p.daily.clone();
        let detail = if rest || p.path.is_empty() { String::new() } else { crate::model::tilde(std::path::Path::new(&p.path)) };
        row()
            .id(("dash-project", i))
            .py(px(8.))
            .border_b_1()
            .border_color(rgb(colors.row_border))
            .text_size(small(12.))
            .hover(|s| s.bg(rgb(colors.hover)))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(div().w(px(22.)).pl(px(4.)).text_size(small(11.)).text_color(rgb(colors.quiet)).child(if rest { String::new() } else { (i + 1).to_string() }))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).truncate().child(p.name.clone()))
                    .when(!detail.is_empty(), |c| c.child(div().text_size(small(11.)).text_color(rgb(colors.quiet)).truncate().child(detail))),
            )
            .child(
                div().w(px(110.)).pr(px(10.)).child(
                    div().h(px(6.)).w_full().rounded(px(3.)).bg(rgb(colors.row_border)).child(
                        div().h_full().w(relative(p.weight as f32 / top as f32)).rounded(px(3.)).bg(rgb(if rest { colors.quiet } else { colors.blue })),
                    ),
                ),
            )
            .child(div().w(px(64.)).text_right().child(if by_turns { p.turns.to_string() } else { fmt_tokens(p.tokens) }))
            .child(div().w(px(64.)).text_right().child(fmt_money(p.cost, &s.currency)))
            .child(div().w(px(60.)).text_right().child(p.threads.to_string()))
            .child(div().w(px(100.)).pl(px(16.)).h(px(20.)).child(canvas(|_, _, _| {}, move |b, _, window, _| paint_spark(b, &daily, colors, window)).size_full()))
    });
    col().child(section_head(colors, "Top projects", format!("by {}", s.unit.word()))).child(header).children(rows)
}

fn mix_section(colors: Palette, s: &Stats) -> Div {
    // Adjacent colours were picked to stay apart for colour-blind readers.
    let hues = [colors.lane(1), colors.lane(2), colors.lane(0)];
    let total: u64 = s.models.iter().map(|m| m.1).sum::<u64>().max(1);
    let mut shown: Vec<(String, u64, u32)> = s.models.iter().take(3).enumerate().map(|(i, (n, v))| (n.clone(), *v, hues[i])).collect();
    let other: u64 = s.models.iter().skip(3).map(|m| m.1).sum();
    if other > 0 {
        shown.push(("Other".into(), other, colors.quiet));
    }
    let bar = row().h(px(24.)).gap(px(2.)).children(shown.iter().enumerate().map(|(i, (name, v, hue))| {
        let tip = format!("{name} · {:.0}% · {} output tokens", *v as f64 / total as f64 * 100., fmt_tokens(*v));
        div().id(("dash-model", i)).h_full().w(relative(*v as f32 / total as f32)).min_w(px(3.)).flex_shrink().rounded(px(4.)).bg(rgb(*hue)).tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
    }));
    let labels = row().mt(px(8.)).gap(px(2.)).items_start().children(shown.iter().map(|(name, v, hue)| {
        let share = *v as f32 / total as f32;
        col()
            .w(relative(share.max(0.2)))
            .flex_shrink()
            .min_w_0()
            .overflow_hidden()
            .child(div().text_size(small(13.)).font_weight(FontWeight::SEMIBOLD).child(format!("{:.0}%", share * 100.)))
            .child(row().gap(px(5.)).text_size(small(11.)).text_color(rgb(colors.muted)).child(div().flex_shrink_0().size(px(8.)).rounded(px(2.)).bg(rgb(*hue))).child(div().truncate().child(name.clone())))
    }));
    let max = s.tools.first().map_or(1, |t| t.1).max(1);
    const TICKS: u32 = 20;
    let tools = s.tools.iter().take(8).enumerate().map(|(i, (name, calls, denied))| {
        let on = ((*calls as f32 / max as f32) * TICKS as f32).ceil() as u32;
        let red = if *denied > 0 { ((*denied as f32 / *calls.max(&1) as f32) * on as f32).ceil().max(1.) as u32 } else { 0 };
        let tip = format!("{name} · {calls} calls{}", if *denied > 0 { format!(" · {denied} denied") } else { String::new() });
        row()
            .id(("dash-tool", i))
            .py(px(3.))
            .gap(px(10.))
            .text_size(small(12.))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .child(div().w(px(64.)).child(name.clone()))
            .child(row().flex_1().h(px(12.)).gap(px(2.)).children((0..TICKS).map(|t| {
                let color = if t >= on {
                    rgb(colors.row_border)
                } else if t >= on - red {
                    rgb(colors.red)
                } else {
                    with_alpha(colors.text, 0.75).into()
                };
                div().w(px(3.)).h_full().rounded(px(1.)).bg(color)
            })))
            .child(div().w(px(48.)).text_right().text_color(rgb(colors.muted)).child(fmt_tokens(*calls as u64)))
    });
    col()
        .child(section_head(colors, "Model mix", "share of output tokens"))
        .when(shown.is_empty(), |c| c.child(div().text_color(rgb(colors.muted)).child("No model reported yet.")))
        .when(!shown.is_empty(), |c| c.child(bar).child(labels))
        .child(
            col()
                .mt(px(30.))
                .child(section_head(colors, "What the agent reached for", "tool calls").mb(px(8.)))
                .children(tools)
                .child(
                    row()
                        .mt(px(8.))
                        .gap(px(6.))
                        .text_size(small(11.))
                        .text_color(rgb(colors.quiet))
                        .child(div().w(px(3.)).h(px(10.)).rounded(px(1.)).bg(rgb(colors.red)))
                        .child("red ticks are calls you denied"),
                ),
        )
}

fn trio(colors: Palette, s: &Stats) -> Div {
    let cell = |first: bool| col().flex_1().min_w_0().py(px(20.)).when(!first, |c| c.pl(px(24.)).border_l_1().border_color(rgb(colors.row_border))).pr(px(24.));
    let title = |t: &str, sub: &str| {
        col().mb(px(14.)).child(div().text_size(small(12.)).font_weight(FontWeight::SEMIBOLD).child(t.to_string())).child(div().text_size(small(11.)).text_color(rgb(colors.muted)).child(sub.to_string()))
    };
    let week_tip = |w: &crate::stats::Week| w.start.format("Week of %b %-d").to_string();
    let n = s.weeks.len().max(1);
    let hits = |weeks: &[crate::stats::Week], tip: &dyn Fn(&crate::stats::Week) -> String, key: &'static str| {
        weeks
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let text = tip(w);
                div().id((key, i)).absolute().top_0().h_full().left(relative(i as f32 / n as f32)).w(relative(1. / n as f32)).tooltip(move |win, cx| Tooltip::new(text.clone()).build(win, cx))
            })
            .collect::<Vec<_>>()
    };
    let axis = |left: String, right: String| row().justify_between().mt(px(4.)).text_size(small(10.)).text_color(rgb(colors.quiet)).child(left).child(right);
    let first_week = s.weeks.first().map(|w| w.start.format("%b %-d").to_string()).unwrap_or_default();

    let any_limits = s.weeks.iter().any(|w| w.peak.is_some() || w.hits > 0);
    let weeks = s.weeks.clone();
    let limits = cell(true).child(title("Limit pressure", "peak weekly limit used, last 12 weeks")).child(if any_limits {
        col()
            .child(
                div()
                    .relative()
                    .h(px(TRIO_H))
                    .child(canvas(|_, _, _| {}, move |b, _, window, _| paint_limits(b, &weeks, colors, window)).size_full())
                    .children(hits(&s.weeks, &|w| format!("{} · peak {} · five-hour limit hit {}×", week_tip(w), w.peak.map_or("—".into(), |p| format!("{p:.0}%")), w.hits), "dash-limit")),
            )
            .child(axis(first_week.clone(), "• five-hour limit hit · this week".into()))
            .into_any_element()
    } else {
        div().text_size(small(12.)).text_color(rgb(colors.muted)).child("No limit readings yet. Claude subscriptions report them as you work.").into_any_element()
    });

    let c = &s.cache;
    let input_all = (c.input + c.cache_read + c.cache_write).max(1);
    let read_share = c.cache_read as f32 / input_all as f32;
    let seg = |id: &'static str, v: u64, color: Rgba, name: &'static str| {
        let tip = format!("{name} · {} tokens", fmt_tokens(v));
        div().id(id).h_full().w(relative(v as f32 / input_all as f32)).flex_shrink().rounded(px(2.)).bg(color).tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
    };
    let kv = |k: &str, v: String| row().justify_between().mt(px(3.)).text_size(small(11.)).text_color(rgb(colors.muted)).child(k.to_string()).child(div().text_color(rgb(colors.text)).child(v));
    let cache = cell(false)
        .child(title("Cache", "input tokens served from the prompt cache"))
        .child(
            row()
                .items_baseline()
                .child(div().text_size(small(38.)).font_weight(FontWeight::SEMIBOLD).line_height(relative(1.05)).child(format!("{:.0}", read_share * 100.)))
                .child(div().text_size(small(15.)).text_color(rgb(colors.muted)).child("%")),
        )
        .child(
            row()
                .mt(px(14.))
                .mb(px(8.))
                .h(px(8.))
                .gap(px(2.))
                .child(seg("cache-read", c.cache_read, rgb(colors.blue), "Cache reads"))
                .child(seg("cache-write", c.cache_write, mix(colors.row_border, colors.blue, 0.45), "Cache writes"))
                .child(seg("cache-none", c.input, rgb(colors.row_border), "Uncached input")),
        )
        .child(kv("Reads", fmt_tokens(c.cache_read)))
        .child(kv("Writes", fmt_tokens(c.cache_write)))
        .child(kv("Uncached input", fmt_tokens(c.input)));

    let adds: u64 = s.weeks.iter().map(|w| w.adds).sum();
    let dels: u64 = s.weeks.iter().map(|w| w.dels).sum();
    let weeks = s.weeks.clone();
    let code = cell(false).child(title("Code changed", "lines the agent added and removed, by week")).child(
        col()
            .child(
                div()
                    .relative()
                    .h(px(TRIO_H))
                    .child(canvas(|_, _, _| {}, move |b, _, window, _| paint_diff(b, &weeks, colors, window)).size_full())
                    .children(hits(&s.weeks, &|w| format!("{} · +{} −{}", week_tip(w), w.adds, w.dels), "dash-diff")),
            )
            .child(
                row()
                    .justify_between()
                    .mt(px(4.))
                    .text_size(small(10.))
                    .child(row().gap(px(8.)).child(div().text_color(rgb(colors.green)).child(format!("+{adds}"))).child(div().text_color(rgb(colors.red)).child(format!("−{dels}"))))
                    .child(div().text_color(rgb(colors.quiet)).child(format!("net {}{}", if adds >= dels { "+" } else { "−" }, adds.abs_diff(dels)))),
            ),
    );
    row().mt(px(44.)).items_start().border_t_1().border_b_1().border_color(rgb(colors.border)).child(limits).child(cache).child(code)
}

fn record(colors: Palette, k: &str, v: String, title: String, detail: String, link: Option<AnyElement>) -> AnyElement {
    col()
        .flex_1()
        .min_w_0()
        .pr(px(22.))
        .child(div().text_size(small(11.)).text_color(rgb(colors.quiet)).child(k.to_uppercase()))
        .child(div().mt(px(4.)).mb(px(2.)).text_size(small(20.)).font_weight(FontWeight::SEMIBOLD).child(v))
        .child(div().text_size(small(12.)).truncate().child(title))
        .child(div().text_size(small(11.)).text_color(rgb(colors.muted)).truncate().child(detail))
        .children(link)
        .into_any_element()
}

// ── Painting ───────────────────────────────────────────────────────────────

fn with_alpha(color: u32, a: f32) -> Hsla {
    Hsla::from(rgb(color)).opacity(a)
}

/// `a` blended toward `b` by `t`, in RGB.
fn mix(a: u32, b: u32, t: f32) -> Rgba {
    let (a, b) = (rgb(a), rgb(b));
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1. }
}

fn stroke(window: &mut Window, points: &[Point<Pixels>], width: f32, color: impl Into<Background>, dash: Option<&[Pixels]>) {
    if points.len() < 2 {
        return;
    }
    let mut p = PathBuilder::stroke(px(width));
    if let Some(dash) = dash {
        p = p.dash_array(dash);
    }
    p.move_to(points[0]);
    for &pt in &points[1..] {
        p.line_to(pt);
    }
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

/// A bar of `w` by `h` standing on `base`, its top corners rounded.
fn bar_up(window: &mut Window, x: Pixels, base: Pixels, w: Pixels, h: Pixels, color: impl Into<Background>) {
    if h <= px(0.) {
        return;
    }
    let r = px(3.).min(h).min(w / 2.);
    window.paint_quad(fill(Bounds::new(point(x, base - h), size(w, h)), color).corner_radii(Corners { top_left: r, top_right: r, bottom_left: px(0.), bottom_right: px(0.) }));
}

fn paint_rhythm(b: Bounds<Pixels>, hours: &[u64; 24], peak: Option<usize>, hovered: Option<usize>, now: chrono::DateTime<chrono::Local>, colors: Palette, window: &mut Window) {
    use chrono::Timelike;
    let c = b.center();
    let r1 = f32::from(b.size.width) / 2. - 2.;
    let r0 = r1 * 0.42;
    let at = |a: f32, r: f32| point(c.x + px(a.sin() * r), c.y - px(a.cos() * r));
    let ring = |r: f32, color: u32, window: &mut Window| {
        let rb = Bounds::new(point(c.x - px(r), c.y - px(r)), size(px(r * 2.), px(r * 2.)));
        window.paint_quad(quad(rb, px(r), transparent_black(), px(1.), rgb(color), BorderStyle::Solid));
    };
    ring(r1, colors.row_border, window);
    ring(r0 + (r1 - r0) / 2., colors.row_border, window);
    ring(r0 - 2., colors.border, window);
    let max = hours.iter().copied().max().unwrap_or(0).max(1);
    for (h, &v) in hours.iter().enumerate() {
        let gap = 0.035;
        let (a0, a1) = (h as f32 / 24. * TAU + gap, (h + 1) as f32 / 24. * TAU - gap);
        let inner = r0 + 2.;
        let outer = inner + (r1 - inner) * (v as f32 / max as f32).max(0.03);
        let mut p = PathBuilder::fill();
        p.move_to(at(a0, outer));
        p.arc_to(point(px(outer), px(outer)), px(0.), false, true, at(a1, outer));
        p.line_to(at(a1, inner));
        p.arc_to(point(px(inner), px(inner)), px(0.), false, false, at(a0, inner));
        p.close();
        let color: Hsla = if Some(h) == hovered || (hovered.is_none() && Some(h) == peak) { rgb(colors.blue).into() } else { mix(colors.row_border, colors.blue, 0.6).into() };
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }
    }
    // Now, in the usage widget's red.
    let a = (now.hour() as f32 + now.minute() as f32 / 60.) / 24. * TAU;
    stroke(window, &[at(a, r0 - 8.), at(a, r1 + 3.)], 2., rgb(colors.red), None);
}

fn paint_spend(b: Bounds<Pixels>, spend: &crate::stats::Spend, ymax: f64, step: f64, hover: Option<usize>, pad: (f32, f32, f32, f32), colors: Palette, window: &mut Window) {
    let (left, right, top, bottom) = pad;
    let w = f32::from(b.size.width) - left - right;
    let h = SPEND_H - top - bottom;
    let days = spend.days.max(2) as f32;
    let x = |d: usize| b.left() + px(left + d as f32 / (days - 1.) * w);
    let y = |v: f64| b.top() + px(top + (1. - (v / ymax) as f32) * h);
    let mut v = 0.;
    while v <= ymax + 1e-9 {
        stroke(window, &[point(b.left() + px(left), y(v)), point(b.left() + px(left + w), y(v))], 1., rgb(colors.row_border), None);
        v += step;
    }
    let line = |values: &[f64], offset: usize| values.iter().enumerate().map(|(i, v)| point(x(i + offset), y(*v))).collect::<Vec<_>>();
    // Each line starts where keeping did: before then, nothing was counted.
    if let Some((last, from)) = &spend.last {
        stroke(window, &line(&last[*from..], *from), 2., rgb(colors.quiet), Some(&[px(5.), px(4.)]));
    }
    let this = line(&spend.this[spend.from.min(spend.this.len())..], spend.from);
    if let (Some(&first), Some(&last)) = (this.first(), this.last()) {
        // The area under this month.
        let mut area = PathBuilder::fill();
        area.move_to(point(first.x, y(0.)));
        for &p in &this {
            area.line_to(p);
        }
        area.line_to(point(last.x, y(0.)));
        area.close();
        if let Ok(path) = area.build() {
            window.paint_path(path, with_alpha(colors.blue, 0.14));
        }
        if let Some(projected) = spend.projected() {
            stroke(window, &[last, point(x(spend.days as usize - 1), y(projected))], 2., with_alpha(colors.text, 0.55), Some(&[px(1.), px(4.)]));
        }
        stroke(window, &this, 2., rgb(colors.text), None);
        let dot = |p: Point<Pixels>, window: &mut Window| {
            window.paint_quad(fill(Bounds::new(point(p.x - px(5.), p.y - px(5.)), size(px(10.), px(10.))), rgb(colors.background)).corner_radii(px(5.)));
            window.paint_quad(fill(Bounds::new(point(p.x - px(3.5), p.y - px(3.5)), size(px(7.), px(7.))), rgb(colors.text)).corner_radii(px(3.5)));
        };
        dot(last, window);
        if let Some(d) = hover {
            stroke(window, &[point(x(d), y(ymax)), point(x(d), y(0.))], 1., with_alpha(colors.muted, 0.6), None);
            if let Some(v) = spend.at(d).filter(|_| d >= spend.from) {
                dot(point(x(d), y(v)), window);
            }
        }
    }
}

fn paint_spark(b: Bounds<Pixels>, daily: &[u64], colors: Palette, window: &mut Window) {
    let max = daily.iter().copied().max().unwrap_or(0).max(1) as f32;
    let n = daily.len().max(2) as f32 - 1.;
    // Smoothed a little, so the line reads as a trend rather than noise.
    let mut acc = 0.;
    let points: Vec<Point<Pixels>> = daily
        .iter()
        .enumerate()
        .map(|(i, v)| {
            acc = if i == 0 { *v as f32 } else { acc * 0.5 + *v as f32 * 0.5 };
            point(b.left() + b.size.width * (i as f32 / n), b.bottom() - px(2.) - (b.size.height - px(4.)) * (acc / max))
        })
        .collect();
    stroke(window, &points, 1.5, rgb(colors.muted), None);
}

fn paint_limits(b: Bounds<Pixels>, weeks: &[crate::stats::Week], colors: Palette, window: &mut Window) {
    let n = weeks.len().max(1) as f32;
    let (top, base) = (b.top() + px(14.), b.bottom() - px(2.));
    let y = |v: f64| base - (base - top) * (v.min(110.) / 100.) as f32;
    stroke(window, &[point(b.left(), y(100.)), point(b.right(), y(100.))], 1., rgb(colors.muted), Some(&[px(2.), px(3.)]));
    let bw = b.size.width / n;
    for (i, w) in weeks.iter().enumerate() {
        let x = b.left() + bw * i as f32 + px(3.);
        let width = bw - px(6.);
        if let Some(peak) = w.peak {
            let color: Hsla = if peak >= 85. { rgb(colors.amber).into() } else { mix(colors.background, colors.blue, 0.7).into() };
            bar_up(window, x, base, width, base - y(peak), color);
        }
        let from = w.peak.map_or(base, y);
        for k in 0..w.hits.min(6) {
            let cy = from - px(6. + k as f32 * 5.);
            let cx = x + width / 2.;
            window.paint_quad(fill(Bounds::new(point(cx - px(1.7), cy - px(1.7)), size(px(3.4), px(3.4))), rgb(colors.text)).corner_radii(px(1.7)));
        }
    }
    stroke(window, &[point(b.left(), base), point(b.right(), base)], 1., rgb(colors.border), None);
}

fn paint_diff(b: Bounds<Pixels>, weeks: &[crate::stats::Week], colors: Palette, window: &mut Window) {
    let n = weeks.len().max(1) as f32;
    let max = weeks.iter().map(|w| w.adds.max(w.dels)).max().unwrap_or(0).max(1) as f32;
    let mid = b.top() + b.size.height * 0.58;
    let (up, down) = (mid - b.top() - px(2.), b.bottom() - mid - px(2.));
    let bw = b.size.width / n;
    for (i, w) in weeks.iter().enumerate() {
        let x = b.left() + bw * i as f32 + px(3.);
        let width = bw - px(6.);
        bar_up(window, x, mid - px(1.), width, up * (w.adds as f32 / max), rgb(colors.green));
        let h = (down * (w.dels as f32 / max)).min(down);
        if h > px(0.) {
            let r = px(3.).min(h).min(width / 2.);
            window.paint_quad(fill(Bounds::new(point(x, mid + px(1.)), size(width, h)), rgb(colors.red)).corner_radii(Corners { top_left: px(0.), top_right: px(0.), bottom_left: r, bottom_right: r }));
        }
    }
}

// ── Words and numbers ──────────────────────────────────────────────────────

/// A count in `unit`: tokens shortened, turns whole.
fn amount(unit: Unit, n: u64) -> String {
    match unit {
        Unit::Tokens => fmt_tokens(n),
        Unit::Turns => n.to_string(),
    }
}

pub fn fmt_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9).replace(".0B", "B"),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
        n if n >= 10_000 => format!("{}k", (n as f64 / 1e3).round() as u64),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        n => n.to_string(),
    }
}

pub fn fmt_money(v: f64, currency: &str) -> String {
    let amount = if v >= 100. || v == v.round() { format!("{v:.0}") } else { format!("{v:.2}") };
    match currency {
        "USD" => format!("${amount}"),
        "EUR" => format!("€{amount}"),
        "GBP" => format!("£{amount}"),
        other => format!("{amount} {other}"),
    }
}

fn fmt_duration(ms: i64) -> String {
    let s = ms / 1000;
    match s {
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

fn fmt_hours(ms: i64) -> String {
    let h = ms as f64 / 3_600_000.;
    if h >= 10. { format!("{h:.0}h") } else if h >= 1. { format!("{h:.1}h") } else { format!("{}m", ms / 60_000) }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn ago(ms: i64) -> String {
    let days = (crate::model::now_ms() - ms) / 86_400_000;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        d if d < 14 => format!("{d}d ago"),
        d => format!("{}w ago", d / 7),
    }
}

/// A round step for an axis that reaches `top` in about four steps.
fn nice_step(top: f64) -> f64 {
    let raw = (top / 4.).max(0.25);
    let magnitude = 10f64.powf(raw.log10().floor());
    [1., 2., 2.5, 5., 10.].iter().map(|m| m * magnitude).find(|s| *s >= raw).unwrap_or(10. * magnitude)
}
