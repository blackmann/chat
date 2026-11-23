//! The dashboard's numbers, worked out from every turn and usage limit reading
//! kept. Plain data in, plain data out: the view only lays it out.
use crate::{agent::Tokens, store::LimitRow};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use std::collections::{HashMap, HashSet};

/// One finished turn, as read back from the database.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    pub thread: Option<i64>,
    /// The project's folder, or '' for regular chats.
    pub project: String,
    /// Unix ms.
    pub started_at: i64,
    pub ended_at: i64,
    pub tokens: Tokens,
    pub models: Vec<(String, Tokens)>,
    pub cost: f64,
    pub currency: Option<String>,
    pub tools: Vec<(String, u32, u32)>,
    pub adds: u64,
    pub dels: u64,
    pub files: u64,
    pub allowed: u32,
    pub denied: u32,
    /// Rebuilt from an old thread: no tokens, and a share of its cost.
    pub rebuilt: bool,
}

/// Everything kept, read in one go.
#[derive(Default)]
pub struct History {
    pub turns: Vec<Turn>,
    pub limits: Vec<LimitRow>,
    /// Thread titles by id, for those not deleted.
    pub titles: HashMap<i64, String>,
}

/// How far back the dashboard looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    Quarter,
    Year,
    All,
}

impl Range {
    pub const ALL: [Range; 5] = [Range::Week, Range::Month, Range::Quarter, Range::Year, Range::All];
    pub fn label(self) -> &'static str {
        match self {
            Range::Week => "7d",
            Range::Month => "30d",
            Range::Quarter => "90d",
            Range::Year => "1y",
            Range::All => "All",
        }
    }
    /// The range in words, to open a sentence.
    pub fn phrase(self) -> &'static str {
        match self {
            Range::Week => "Over the past week",
            Range::Month => "Over the past 30 days",
            Range::Quarter => "Over the past 90 days",
            Range::Year => "Over the past year",
            Range::All => "Since you started",
        }
    }
    /// The range for eyebrows: "Last 30 days".
    pub fn title(self) -> &'static str {
        match self {
            Range::Week => "Last 7 days",
            Range::Month => "Last 30 days",
            Range::Quarter => "Last 90 days",
            Range::Year => "Last 12 months",
            Range::All => "All time",
        }
    }
    fn days(self) -> Option<i64> {
        match self {
            Range::Week => Some(7),
            Range::Month => Some(30),
            Range::Quarter => Some(90),
            Range::Year => Some(365),
            Range::All => None,
        }
    }
}

/// What activity is measured in: tokens, or turns where the range reaches
/// back before tokens were counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Tokens,
    Turns,
}

impl Unit {
    pub fn word(self) -> &'static str {
        match self {
            Unit::Tokens => "tokens",
            Unit::Turns => "turns",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: NaiveDate,
    pub tokens: u64,
    pub turns: u32,
    /// Its tokens were counted: some turn that day wasn't rebuilt.
    pub counted: bool,
    pub threads: u32,
    /// Past today: drawn as a placeholder.
    pub future: bool,
    /// Projects worked on, as (folder, tokens, turns), the busiest first.
    pub projects: Vec<(String, u64, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spend {
    /// Running total by day of this month, up to today.
    pub this: Vec<f64>,
    /// The first day of this month anything was kept for (0-based): before
    /// it, nothing was counted, rather than nothing spent.
    pub from: usize,
    /// Running total by day of last month, all of it, and the first day kept;
    /// `None` when nothing was kept then.
    pub last: Option<(Vec<f64>, usize)>,
    /// Days in this month.
    pub days: u32,
    pub month: String,
    pub last_month: String,
}

impl Spend {
    /// The spend per day so far, over the days kept.
    fn pace(&self) -> Option<f64> {
        let so_far = *self.this.last()?;
        Some(so_far / self.this.len().saturating_sub(self.from).max(1) as f64)
    }

    /// Where this month ends up at the pace so far.
    pub fn projected(&self) -> Option<f64> {
        self.at(self.days as usize - 1)
    }

    /// The running total on a day (0-based): spent so far, or projected
    /// at the pace so far for days still to come.
    pub fn at(&self, day: usize) -> Option<f64> {
        if let Some(v) = self.this.get(day) {
            return Some(*v);
        }
        let so_far = *self.this.last()?;
        let pace = self.pace()?;
        (day < self.days as usize).then(|| so_far + pace * (day + 1 - self.this.len()) as f64)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectStat {
    pub name: String,
    /// The folder, '' for regular chats and for "others".
    pub path: String,
    pub tokens: u64,
    pub cost: f64,
    pub threads: usize,
    pub turns: usize,
    /// What it's ranked by, in the stats' unit.
    pub weight: u64,
    /// Unix ms.
    pub last_active: i64,
    /// Activity by day over the last 30 days, oldest first, in the stats' unit.
    pub daily: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Week {
    pub start: NaiveDate,
    /// The most of the weekly limit used that week, 0–100.
    pub peak: Option<f64>,
    /// How many five-hour windows ran out.
    pub hits: u32,
    pub adds: u64,
    pub dels: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadBest {
    pub thread: i64,
    pub title: Option<String>,
    pub project: String,
    /// When it was last active, Unix ms.
    pub at: i64,
    pub value: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bests {
    /// By time the agent worked, in ms.
    pub longest: Option<ThreadBest>,
    pub priciest: Option<ThreadBest>,
    pub most_files: Option<ThreadBest>,
    pub biggest_day: Option<Day>,
}

pub struct Stats {
    pub range: Range,
    /// Nothing kept at all yet.
    pub empty: bool,
    /// What the rhythm, projects and part of day are measured in.
    pub unit: Unit,
    /// When tokens began to be counted, if some turns are older: rebuilt from
    /// threads saved before.
    pub tokens_since: Option<NaiveDate>,
    pub tokens: u64,
    /// Tokens over the same span just before, when there is one.
    pub prior_tokens: Option<u64>,
    pub cost: f64,
    pub currency: String,
    pub turns: usize,
    pub threads: usize,
    pub projects: usize,
    pub active_days: usize,
    /// Days the range spans, up to today.
    pub span_days: usize,
    pub agent_ms: i64,
    pub median_turn_ms: i64,
    pub allowed: u32,
    pub denied: u32,
    pub top_project: Option<String>,
    /// The last 53 weeks, Sunday first, a column a week.
    pub heat: Vec<Day>,
    /// Token counts that split counted days into four even groups.
    pub heat_steps: [u64; 3],
    /// Turn counts that do the same for days from before tokens were counted.
    pub heat_turn_steps: [u64; 3],
    pub streak: u32,
    pub longest_streak: u32,
    /// Activity by the hour turns started, local time, in `unit`.
    pub hours: [u64; 24],
    /// Activity by weekday, Sunday first, in `unit`.
    pub weekdays: [u64; 7],
    pub spend: Spend,
    pub projects_top: Vec<ProjectStat>,
    pub projects_rest: Option<ProjectStat>,
    /// Output tokens by model, most first.
    pub models: Vec<(String, u64)>,
    /// (kind, calls, denied), most called first.
    pub tools: Vec<(String, u32, u32)>,
    /// The last 12 weeks, oldest first.
    pub weeks: Vec<Week>,
    pub cache: Tokens,
    pub bests: Bests,
}

/// How a project is named: its folder's name, or "Chats".
pub fn project_name(path: &str) -> String {
    if path.is_empty() {
        return "Chats".into();
    }
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// A model's id as people say it: `claude-opus-5-5` reads "Opus 5.5".
pub fn model_name(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id).trim();
    let Some(rest) = base.strip_prefix("claude-") else { return base.to_string() };
    // Drop a trailing date stamp, like -20250514.
    let parts: Vec<&str> = rest.split('-').filter(|p| !(p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))).collect();
    let (names, numbers): (Vec<&str>, Vec<&str>) = parts.iter().partition(|p| !p.chars().all(|c| c.is_ascii_digit()));
    let mut name: String = names
        .iter()
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !numbers.is_empty() {
        name.push(' ');
        name.push_str(&numbers.join("."));
    }
    name
}

fn local(ms: i64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or_else(Local::now)
}

/// The local day a time in Unix ms falls on.
pub fn local_date(ms: i64) -> NaiveDate {
    local(ms).date_naive()
}

impl Stats {
    pub fn compute(history: &History, range: Range, now: DateTime<Local>) -> Stats {
        let today = now.date_naive();
        let now_ms = now.timestamp_millis();
        let first = history.turns.iter().map(|t| t.started_at).min();
        let start = match range.days() {
            Some(days) => (now - Duration::days(days)).timestamp_millis(),
            None => first.unwrap_or(now_ms),
        };
        let span_days = ((now_ms - start) / 86_400_000).max(1) as usize;
        let in_range: Vec<&Turn> = history.turns.iter().filter(|t| t.started_at >= start).collect();
        let unit = if in_range.iter().any(|t| t.rebuilt) { Unit::Turns } else { Unit::Tokens };
        let weight = |t: &Turn| match unit {
            Unit::Tokens => t.tokens.total(),
            Unit::Turns => 1,
        };
        let tokens_since = history
            .turns
            .iter()
            .any(|t| t.rebuilt)
            .then(|| history.turns.iter().filter(|t| !t.rebuilt).map(|t| t.started_at).min())
            .flatten()
            .map(local_date);

        let tokens = in_range.iter().map(|t| t.tokens.total()).sum();
        // Not against a span with turns from before tokens were counted.
        let prior_tokens = range.days().filter(|_| first.is_some_and(|f| f < start) && unit == Unit::Tokens).and_then(|days| {
            let from = start - days * 86_400_000;
            let prior: Vec<&Turn> = history.turns.iter().filter(|t| (from..start).contains(&t.started_at)).collect();
            (!prior.iter().any(|t| t.rebuilt)).then(|| prior.iter().map(|t| t.tokens.total()).sum())
        });
        let cost = in_range.iter().map(|t| t.cost).sum();
        let currency = history.turns.iter().rev().find_map(|t| t.currency.clone()).unwrap_or_else(|| "USD".into());
        let threads: HashSet<i64> = in_range.iter().filter_map(|t| t.thread).collect();
        let projects: HashSet<&str> = in_range.iter().map(|t| t.project.as_str()).collect();
        let active: HashSet<NaiveDate> = in_range.iter().map(|t| local(t.started_at).date_naive()).collect();
        let mut lengths: Vec<i64> = in_range.iter().map(|t| (t.ended_at - t.started_at).max(0)).collect();
        lengths.sort_unstable();
        let agent_ms = lengths.iter().sum();
        let median_turn_ms = lengths.get(lengths.len() / 2).copied().unwrap_or(0);

        // By day, over everything: the year's grid, streaks and the biggest day.
        // By day: tokens, turns, whether any turn was counted live, threads.
        let mut by_day: HashMap<NaiveDate, (u64, u32, bool, HashSet<i64>, HashMap<&str, (u64, u32)>)> = HashMap::new();
        for t in &history.turns {
            let day = by_day.entry(local(t.started_at).date_naive()).or_default();
            day.0 += t.tokens.total();
            day.1 += 1;
            day.2 |= !t.rebuilt;
            day.3.extend(t.thread);
            let p = day.4.entry(t.project.as_str()).or_default();
            p.0 += t.tokens.total();
            p.1 += 1;
        }
        let day = |date: NaiveDate| {
            let Some((tokens, turns, counted, threads, projects)) = by_day.get(&date) else {
                return Day { date, tokens: 0, turns: 0, counted: false, threads: 0, future: date > today, projects: Vec::new() };
            };
            let mut projects: Vec<(String, u64, u32)> = projects.iter().map(|(p, (n, k))| (p.to_string(), *n, *k)).collect();
            projects.sort_by(|a, b| (b.1, b.2).cmp(&(a.1, a.2)).then_with(|| a.0.cmp(&b.0)));
            Day { date, tokens: *tokens, turns: *turns, counted: *counted, threads: threads.len() as u32, future: date > today, projects }
        };
        let end = today + Duration::days(6 - today.weekday().num_days_from_sunday() as i64);
        let grid_start = end - Duration::days(53 * 7 - 1);
        let heat: Vec<Day> = grid_start.iter_days().take(53 * 7).map(day).collect();
        let steps = |mut counts: Vec<u64>| {
            counts.sort_unstable();
            let q = |f: f64| counts.get((counts.len().saturating_sub(1) as f64 * f) as usize).copied().unwrap_or(0);
            [q(0.25), q(0.5), q(0.75)]
        };
        let heat_steps = steps(heat.iter().filter(|d| d.counted && d.tokens > 0).map(|d| d.tokens).collect());
        let heat_turn_steps = steps(heat.iter().filter(|d| !d.counted && d.turns > 0).map(|d| d.turns as u64).collect());
        let active_on = |d: NaiveDate| by_day.get(&d).is_some_and(|day| day.1 > 0);
        // Today without anything yet doesn't break a streak: it's not over.
        let mut streak = 0;
        let mut d = if active_on(today) { today } else { today - Duration::days(1) };
        while active_on(d) {
            streak += 1;
            d -= Duration::days(1);
        }
        let mut days: Vec<NaiveDate> = by_day.iter().filter(|(_, day)| day.1 > 0).map(|(d, _)| *d).collect();
        days.sort_unstable();
        let (mut longest_streak, mut run, mut prev) = (0u32, 0u32, None::<NaiveDate>);
        for d in days {
            run = if prev.is_some_and(|p| d - p == Duration::days(1)) { run + 1 } else { 1 };
            longest_streak = longest_streak.max(run);
            prev = Some(d);
        }
        let biggest_day = by_day.iter().max_by_key(|(d, day)| (day.0, std::cmp::Reverse(**d))).filter(|(_, day)| day.0 > 0).map(|(d, _)| day(*d));

        let mut hours = [0u64; 24];
        let mut weekdays = [0u64; 7];
        for t in &in_range {
            let at = local(t.started_at);
            hours[at.hour() as usize] += weight(t);
            weekdays[at.weekday().num_days_from_sunday() as usize] += weight(t);
        }

        // Spend: this month against last.
        let month_start = today.with_day(1).unwrap_or(today);
        let last_start = (month_start - Duration::days(1)).with_day(1).unwrap_or(month_start);
        let days_in = |start: NaiveDate| {
            let next = if start.month() == 12 { NaiveDate::from_ymd_opt(start.year() + 1, 1, 1) } else { NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1) };
            next.map_or(30, |n| (n - start).num_days() as u32)
        };
        let mut this = vec![0.; today.day() as usize];
        let mut last = vec![0.; days_in(last_start) as usize];
        for t in &history.turns {
            let d = local(t.ended_at).date_naive();
            if d >= month_start && d <= today {
                this[d.day0() as usize] += t.cost;
            } else if d >= last_start && d < month_start {
                last[d.day0() as usize] += t.cost;
            }
        }
        let running = |v: &mut Vec<f64>| {
            let mut sum = 0.;
            for x in v.iter_mut() {
                sum += *x;
                *x = sum;
            }
        };
        running(&mut this);
        running(&mut last);
        let first_day = first.map(local_date);
        let from = first_day.filter(|d| *d >= month_start).map_or(0, |d| (d.day0() as usize).min(this.len().saturating_sub(1)));
        let last = first_day.filter(|d| *d < month_start).map(|d| (last, if d >= last_start { d.day0() as usize } else { 0 }));
        let spend = Spend {
            this,
            from,
            last,
            days: days_in(month_start),
            month: month_start.format("%B").to_string(),
            last_month: last_start.format("%B").to_string(),
        };

        // Projects, by tokens.
        let mut by_project: HashMap<&str, (ProjectStat, HashSet<i64>)> = HashMap::new();
        let thirty = today - Duration::days(29);
        for t in &in_range {
            let (p, threads) = by_project.entry(t.project.as_str()).or_insert_with(|| {
                (ProjectStat { name: project_name(&t.project), path: t.project.clone(), daily: vec![0; 30], ..Default::default() }, HashSet::new())
            });
            p.tokens += t.tokens.total();
            p.weight += weight(t);
            p.turns += 1;
            p.cost += t.cost;
            p.last_active = p.last_active.max(t.ended_at);
            threads.extend(t.thread);
            let d = local(t.started_at).date_naive();
            if d >= thirty && d <= today {
                p.daily[(d - thirty).num_days() as usize] += weight(t);
            }
        }
        let mut ranked: Vec<ProjectStat> = by_project
            .into_values()
            .map(|(mut p, threads)| {
                p.threads = threads.len();
                p
            })
            .collect();
        ranked.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.name.cmp(&b.name)));
        let top_project = ranked.first().map(|p| p.name.clone());
        let rest: Vec<ProjectStat> = if ranked.len() > 6 { ranked.split_off(5) } else { Vec::new() };
        let projects_rest = (!rest.is_empty()).then(|| {
            let mut others = ProjectStat { name: format!("{} others", rest.len()), daily: vec![0; 30], ..Default::default() };
            for p in &rest {
                others.tokens += p.tokens;
                others.weight += p.weight;
                others.turns += p.turns;
                others.cost += p.cost;
                others.threads += p.threads;
                others.last_active = others.last_active.max(p.last_active);
                for (a, b) in others.daily.iter_mut().zip(&p.daily) {
                    *a += b;
                }
            }
            others
        });

        let mut models: HashMap<String, u64> = HashMap::new();
        let mut tools: HashMap<&str, (u32, u32)> = HashMap::new();
        let mut cache = Tokens::default();
        let (mut allowed, mut denied) = (0, 0);
        for t in &in_range {
            for (m, n) in &t.models {
                *models.entry(model_name(m)).or_default() += n.output;
            }
            for (kind, calls, d) in &t.tools {
                let e = tools.entry(kind).or_default();
                e.0 += calls;
                e.1 += d;
            }
            cache.add(&t.tokens);
            allowed += t.allowed;
            denied += t.denied;
        }
        let mut models: Vec<(String, u64)> = models.into_iter().filter(|(_, n)| *n > 0).collect();
        models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut tools: Vec<(String, u32, u32)> = tools.into_iter().map(|(k, (c, d))| (k.to_string(), c, d)).collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // The last twelve weeks, Sunday to Saturday.
        let week_start = today - Duration::days(today.weekday().num_days_from_sunday() as i64);
        let weeks: Vec<Week> = (0..12)
            .rev()
            .map(|i| {
                let start = week_start - Duration::weeks(i);
                let within = |ms: i64| {
                    let d = local(ms).date_naive();
                    d >= start && d < start + Duration::weeks(1)
                };
                let readings = history.limits.iter().filter(|l| within(l.at));
                let peak = readings.clone().filter(|l| l.window == "seven_day").filter_map(|l| l.used).fold(None, |m: Option<f64>, u| Some(m.map_or(u, |m| m.max(u))));
                let hits = readings.filter(|l| l.window == "five_hour" && l.status == "rejected").map(|l| l.resets_at).collect::<HashSet<_>>().len() as u32;
                let (adds, dels) = history.turns.iter().filter(|t| within(t.started_at)).fold((0, 0), |(a, d), t| (a + t.adds, d + t.dels));
                Week { start, peak, hits, adds, dels }
            })
            .collect();

        // Personal bests, over everything.
        let mut per_thread: HashMap<i64, (i64, f64, u64, &str, i64)> = HashMap::new();
        for t in &history.turns {
            let Some(id) = t.thread else { continue };
            let e = per_thread.entry(id).or_insert((0, 0., 0, t.project.as_str(), 0));
            e.0 += (t.ended_at - t.started_at).max(0);
            e.1 += t.cost;
            e.2 += t.files;
            e.4 = e.4.max(t.ended_at);
        }
        let best = |value: &dyn Fn(&(i64, f64, u64, &str, i64)) -> f64| {
            per_thread
                .iter()
                .map(|(id, e)| (*id, e, value(e)))
                .filter(|(_, _, v)| *v > 0.)
                .max_by(|a, b| a.2.total_cmp(&b.2).then(b.0.cmp(&a.0)))
                .map(|(id, e, v)| ThreadBest { thread: id, title: history.titles.get(&id).cloned(), project: project_name(e.3), at: e.4, value: v })
        };
        let bests = Bests {
            longest: best(&|e| e.0 as f64),
            priciest: best(&|e| e.1),
            most_files: best(&|e| e.2 as f64),
            biggest_day,
        };

        Stats {
            range,
            empty: history.turns.is_empty(),
            unit,
            tokens_since,
            tokens,
            prior_tokens,
            cost,
            currency,
            turns: in_range.len(),
            threads: threads.len(),
            projects: projects.len(),
            active_days: active.len(),
            span_days,
            agent_ms,
            median_turn_ms,
            allowed,
            denied,
            top_project,
            heat,
            heat_steps,
            heat_turn_steps,
            streak,
            longest_streak,
            hours,
            weekdays,
            spend,
            projects_top: ranked,
            projects_rest,
            models,
            tools,
            weeks,
            cache,
            bests,
        }
    }

    /// How dark a day sits on the grid: 0 for none, 1–4 by quartile, of
    /// tokens on days they were counted, else of turns.
    pub fn level(&self, day: &Day) -> u8 {
        let (n, steps) = if day.counted { (day.tokens, self.heat_steps) } else { (day.turns as u64, self.heat_turn_steps) };
        match n {
            0 if day.turns == 0 => 0,
            n if n <= steps[0] => 1,
            n if n <= steps[1] => 2,
            n if n <= steps[2] => 3,
            _ => 4,
        }
    }

    /// When most of the work happens, as a phrase to end a sentence: the part
    /// of the day holding the most tokens.
    pub fn part_of_day(&self) -> Option<&'static str> {
        let sum = |hours: &[usize]| hours.iter().map(|h| self.hours[*h]).sum::<u64>();
        let parts = [
            ("in the morning", sum(&[5, 6, 7, 8, 9, 10, 11])),
            ("in the afternoon", sum(&[12, 13, 14, 15, 16])),
            ("in the evening", sum(&[17, 18, 19, 20])),
            ("after 9 pm", sum(&[21, 22, 23, 0, 1, 2, 3, 4])),
        ];
        parts.iter().filter(|p| p.1 > 0).max_by_key(|p| p.1).map(|p| p.0)
    }

    /// The share of activity on turns started from 9 pm to 5 am.
    pub fn late_share(&self) -> f64 {
        let total: u64 = self.hours.iter().sum();
        let late: u64 = [21, 22, 23, 0, 1, 2, 3, 4].iter().map(|h| self.hours[*h]).sum();
        if total == 0 { 0. } else { late as f64 / total as f64 }
    }

    /// The busiest hour, as a start hour 0–23.
    pub fn peak_hour(&self) -> Option<usize> {
        let max = *self.hours.iter().max()?;
        (max > 0).then(|| self.hours.iter().position(|h| *h == max).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, h, 0, 0).single().unwrap().timestamp_millis()
    }

    fn turn(thread: i64, project: &str, start: i64, tokens: u64, cost: f64) -> Turn {
        Turn {
            thread: Some(thread),
            project: project.into(),
            started_at: start,
            ended_at: start + 60_000,
            tokens: Tokens { input: tokens / 2, output: tokens / 2, ..Default::default() },
            models: vec![("claude-opus-5-5".into(), Tokens { output: tokens / 2, ..Default::default() })],
            cost,
            currency: Some("USD".into()),
            ..Default::default()
        }
    }

    #[test]
    fn names_models_as_people_say_them() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_name("claude-haiku-5-5[1m]"), "Haiku 5.5");
        assert_eq!(model_name("gpt-6.1-sol"), "gpt-6.1-sol");
        assert_eq!(project_name("/Users/me/code/chat"), "chat");
        assert_eq!(project_name(""), "Chats");
    }

    #[test]
    fn totals_streaks_and_spend() {
        let now = Local.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).single().unwrap();
        let history = History {
            turns: vec![
                turn(1, "/a/chat", at(2026, 10, 30, 22), 1_000, 1.0),
                turn(1, "/a/chat", at(2026, 11, 20, 23), 4_000, 2.0),
                turn(2, "/a/blog", at(2026, 11, 21, 10), 2_000, 0.5),
                turn(3, "", at(2026, 11, 22, 9), 500, 0.25),
            ],
            limits: vec![
                LimitRow { at: at(2026, 11, 21, 10), agent: "Claude".into(), window: "seven_day".into(), status: "allowed".into(), used: Some(40.), resets_at: None },
                LimitRow { at: at(2026, 11, 21, 11), agent: "Claude".into(), window: "seven_day".into(), status: "allowed".into(), used: Some(55.), resets_at: None },
                LimitRow { at: at(2026, 11, 21, 12), agent: "Claude".into(), window: "five_hour".into(), status: "rejected".into(), used: Some(100.), resets_at: Some(9) },
                LimitRow { at: at(2026, 11, 21, 13), agent: "Claude".into(), window: "five_hour".into(), status: "rejected".into(), used: Some(100.), resets_at: Some(9) },
            ],
            titles: [(1, "Build the dashboard".to_string())].into(),
        };
        let s = Stats::compute(&history, Range::Week, now);
        assert_eq!(s.tokens, 6_500, "the October turn is out of range");
        assert_eq!((s.threads, s.projects, s.active_days), (3, 3, 3));
        assert_eq!(s.top_project.as_deref(), Some("chat"));
        assert_eq!(s.streak, 3);
        assert_eq!(s.longest_streak, 3);
        assert_eq!(s.part_of_day(), Some("after 9 pm"));
        assert_eq!(s.models, vec![("Opus 5.5".to_string(), 3_250)]);
        assert_eq!(s.heat.len(), 53 * 7);
        assert!(s.heat.iter().any(|d| d.future), "the rest of this week is still to come");
        let big = s.heat.iter().find(|d| d.tokens == 4_000).unwrap();
        assert_eq!(s.level(big), 4);
        assert_eq!(s.unit, Unit::Tokens);
        assert_eq!(s.tokens_since, None);
        assert_eq!(s.spend.this.len(), 22);
        assert_eq!(s.spend.this.last(), Some(&2.75));
        assert_eq!(s.spend.last.as_ref().map(|(l, from)| (l.last().copied(), *from)), Some((Some(1.0), 29)), "kept from Oct 30");
        assert_eq!(s.spend.projected(), Some(2.75 / 22. * 30.));
        // Kept only from the 20th: the pace is over the three days since.
        let late = History { turns: history.turns[1..].to_vec(), ..Default::default() };
        let spend = Stats::compute(&late, Range::Week, now).spend;
        assert_eq!((spend.from, spend.last.is_none()), (19, true), "no October to compare");
        assert_eq!(spend.at(22), Some(2.75 + 2.75 / 3.));
        assert_eq!(spend.projected(), Some(2.75 + 2.75 / 3. * 8.));
        // The 22nd is a Sunday: the 21st closes the week before.
        let week = &s.weeks[10];
        assert_eq!((week.peak, week.hits), (Some(55.), 1), "one five-hour window ran out, read twice");
        let longest = s.bests.longest.unwrap();
        assert_eq!((longest.thread, longest.title.as_deref()), (1, Some("Build the dashboard")));
        let biggest = s.bests.biggest_day.unwrap();
        assert_eq!(biggest.tokens, 4_000);
        assert!(biggest.projects.windows(2).all(|p| p[0].1 >= p[1].1), "busiest project first");
        assert_eq!(biggest.projects.iter().map(|p| p.1).sum::<u64>(), 4_000);
    }

    #[test]
    fn nothing_kept_is_empty() {
        let s = Stats::compute(&History::default(), Range::All, Local::now());
        assert!(s.empty && s.tokens == 0 && s.bests.longest.is_none() && s.part_of_day().is_none());
    }

    #[test]
    fn rebuilt_turns_count_as_turns() {
        let now = Local.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).single().unwrap();
        let old = |thread, start| Turn { tokens: Tokens::default(), models: Vec::new(), rebuilt: true, ..turn(thread, "/a/old", start, 0, 0.5) };
        let history = History {
            turns: vec![
                old(1, at(2026, 9, 1, 10)),
                old(1, at(2026, 9, 1, 11)),
                old(1, at(2026, 9, 2, 22)),
                old(1, at(2026, 10, 10, 14)),
                turn(2, "/a/new", at(2026, 11, 20, 10), 9_000, 1.0),
            ],
            ..Default::default()
        };
        let year = Stats::compute(&history, Range::Year, now);
        assert_eq!(year.unit, Unit::Turns, "the year reaches back past counted tokens");
        assert_eq!(year.tokens_since, NaiveDate::from_ymd_opt(2026, 11, 20));
        assert_eq!(year.projects_top[0].name, "old", "ranked by turns: 3 against 1");
        assert_eq!(year.hours[10], 2);
        assert_eq!(year.active_days, 4);
        let sept1 = year.heat.iter().find(|d| d.date == NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()).unwrap();
        assert!(!sept1.counted && sept1.turns == 2 && year.level(sept1) == 4);
        let month = Stats::compute(&history, Range::Month, now);
        assert_eq!(month.unit, Unit::Tokens, "the month only has counted turns");
        assert_eq!(month.prior_tokens, None, "the month before has turns without tokens: no fair comparison");
        assert_eq!(Stats::compute(&history, Range::Week, now).prior_tokens, Some(0));
    }
}
