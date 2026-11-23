//! Missions: small challenges to take on while agents work. They aren't tied
//! to a thread or project; there is one list for the whole app.
//!
//! A mission runs through phases: something to memorize for a few seconds,
//! a wait, then recalling it. Each kind of mission says how long it shows,
//! and what it waits for: some time, some turns ending, or both (whichever
//! comes first). A mission with no wait is recalled at once.
//!
//! The puzzle comes from a seed, so only the seed, the phase and the answer
//! are kept, and a mission picks up where it left off after a restart.
use serde::{Deserialize, Serialize};

/// The kinds of mission there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Memorize a PIN; recall it later.
    Pin,
}

impl Kind {
    pub const ALL: [Kind; 1] = [Kind::Pin];

    pub fn key(self) -> &'static str {
        match self {
            Kind::Pin => "pin",
        }
    }
    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }
    pub fn title(self) -> &'static str {
        match self {
            Kind::Pin => "Remember the PIN",
        }
    }
    pub fn brief(self) -> &'static str {
        match self {
            Kind::Pin => "A PIN shows for a few seconds. Hold on to it; you'll be asked for it after your next turn ends.",
        }
    }
    /// The highest level; levels start at 0.
    pub fn max_level(self) -> u8 {
        match self {
            Kind::Pin => 6,
        }
    }
    /// How long the puzzle stays on show.
    fn memorize_ms(self, level: u8) -> i64 {
        match self {
            Kind::Pin => 2_500 + 600 * pin_len(level) as i64,
        }
    }
    /// What recall waits for once the puzzle is hidden.
    pub fn wait(self, _level: u8) -> Wait {
        match self {
            Kind::Pin => Wait { turns: Some(1), ms: Some(3 * 60_000) },
        }
    }
}

/// What recall waits for: `turns` turns to end, or `ms` to pass, whichever
/// comes first. Neither means recall comes straight away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    pub turns: Option<u32>,
    pub ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    /// The puzzle is on show until `until` (Unix ms).
    Memorize { until: i64 },
    /// Hidden since `since`; `turns` turns have ended since.
    Waiting { since: i64, turns: u32 },
    /// Asking for the answer.
    Recall,
    /// Answered, or given up on (the mission's `answer` is `None`), at `at`.
    Done { at: i64, score: u32, perfect: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mission {
    pub id: i64,
    pub kind: Kind,
    pub seed: u64,
    pub level: u8,
    pub started_at: i64,
    pub phase: Phase,
    pub answer: Option<String>,
}

impl Mission {
    pub fn new(id: i64, kind: Kind, seed: u64, level: u8, now: i64) -> Mission {
        let level = level.min(kind.max_level());
        Mission { id, kind, seed, level, started_at: now, phase: Phase::Memorize { until: now + kind.memorize_ms(level) }, answer: None }
    }

    pub fn wait(&self) -> Wait {
        self.kind.wait(self.level)
    }

    /// The PIN to remember, for a PIN mission.
    pub fn pin(&self) -> String {
        let mut rng = Rng(self.seed);
        (0..pin_len(self.level)).map(|_| char::from(b'0' + (rng.next() % 10) as u8)).collect()
    }

    /// The text to recall.
    pub fn solution(&self) -> String {
        match self.kind {
            Kind::Pin => self.pin(),
        }
    }

    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Done { .. })
    }
    pub fn recalling(&self) -> bool {
        self.phase == Phase::Recall
    }

    /// Move on with the time. Returns whether the phase changed.
    pub fn tick(&mut self, now: i64) -> bool {
        let before = self.phase.clone();
        if let Phase::Memorize { until } = self.phase
            && now >= until
        {
            self.phase = Phase::Waiting { since: until.min(now), turns: 0 };
        }
        if let Phase::Waiting { since, turns } = self.phase {
            let wait = self.wait();
            let timed_out = wait.ms.is_some_and(|ms| now - since >= ms);
            let turned = wait.turns.is_some_and(|n| turns >= n);
            if timed_out || turned || (wait.ms.is_none() && wait.turns.is_none()) {
                self.phase = Phase::Recall;
            }
        }
        self.phase != before
    }

    /// A turn ended somewhere. Returns whether the phase changed.
    pub fn turn_ended(&mut self, now: i64) -> bool {
        if let Phase::Waiting { turns, .. } = &mut self.phase {
            *turns += 1;
            self.tick(now);
            return true;
        }
        false
    }

    /// When the phase next changes by itself, if it will.
    pub fn deadline(&self) -> Option<i64> {
        match self.phase {
            Phase::Memorize { until } => Some(until),
            Phase::Waiting { since, .. } => self.wait().ms.map(|ms| since + ms),
            _ => None,
        }
    }

    /// Check `answer`, ending the mission. Only while recalling.
    pub fn submit(&mut self, answer: &str, now: i64) -> bool {
        if !self.recalling() {
            return false;
        }
        let answer: String = answer.chars().filter(|c| !c.is_whitespace()).collect();
        let (score, perfect) = score(&self.solution(), &answer, self.level);
        self.answer = Some(answer);
        self.phase = Phase::Done { at: now, score, perfect };
        true
    }

    /// End the mission without an answer.
    pub fn give_up(&mut self, now: i64) {
        if !self.done() {
            self.answer = None;
            self.phase = Phase::Done { at: now, score: 0, perfect: false };
        }
    }

    /// Which places of the answer match the solution.
    pub fn marks(&self) -> Vec<bool> {
        let answer = self.answer.as_deref().unwrap_or("");
        let solution: Vec<char> = self.solution().chars().collect();
        let mut given = answer.chars();
        solution.iter().map(|c| given.next() == Some(*c)).collect()
    }

    pub fn score(&self) -> Option<u32> {
        match self.phase {
            Phase::Done { score, .. } => Some(score),
            _ => None,
        }
    }
}

fn pin_len(level: u8) -> usize {
    4 + level as usize
}

/// 10 points a place right, scaled up by level, and as much again for all of them.
fn score(solution: &str, answer: &str, level: u8) -> (u32, bool) {
    let right = solution.chars().zip(answer.chars()).filter(|(a, b)| a == b).count() as u32;
    let perfect = answer == solution;
    let base = right * 10 * (1 + level as u32 / 2);
    (if perfect { base * 2 } else { base }, perfect)
}

/// The app's missions: the one under way, and those done, newest first.
#[derive(Default)]
pub struct Missions {
    pub active: Option<Mission>,
    pub history: Vec<Mission>,
    next_id: i64,
}

/// Finished missions kept in memory.
pub const HISTORY: usize = 50;

impl Missions {
    /// From what was saved, newest first.
    pub fn from_saved(mut saved: Vec<Mission>) -> Missions {
        let next_id = saved.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let active = saved.iter().position(|m| !m.done()).map(|i| saved.remove(i));
        // Only the newest unfinished one carries on; any older ones are left out.
        saved.retain(|m| m.done());
        saved.truncate(HISTORY);
        Missions { active, history: saved, next_id }
    }

    /// Start a mission of `kind` at the level earned so far.
    pub fn start(&mut self, kind: Kind, now: i64) -> &Mission {
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id += 1;
        let seed = (now as u64) ^ (id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let level = self.next_level(kind);
        self.active.insert(Mission::new(id, kind, seed, level, now))
    }

    /// Put the finished active mission into the history.
    pub fn file(&mut self) {
        if let Some(m) = self.active.take_if(|m| m.done()) {
            self.history.insert(0, m);
            self.history.truncate(HISTORY);
        }
    }

    /// Up a level after a perfect answer, down one after getting under half.
    pub fn next_level(&self, kind: Kind) -> u8 {
        let Some(last) = self.history.iter().find(|m| m.kind == kind) else { return 0 };
        let marks = last.marks();
        let right = marks.iter().filter(|m| **m).count();
        let level = if matches!(last.phase, Phase::Done { perfect: true, .. }) {
            last.level.saturating_add(1)
        } else if right * 2 < marks.len() {
            last.level.saturating_sub(1)
        } else {
            last.level
        };
        level.min(kind.max_level())
    }

    /// A recall is waiting for an answer.
    pub fn ready(&self) -> bool {
        self.active.as_ref().is_some_and(Mission::recalling)
    }
}

/// SplitMix64: small, and the same everywhere, so a seed always makes the same puzzle.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_makes_the_same_pin() {
        let a = Mission::new(1, Kind::Pin, 42, 0, 0);
        let b = Mission::new(2, Kind::Pin, 42, 0, 1_000);
        assert_eq!(a.pin(), b.pin());
        assert_eq!(a.pin().len(), 4);
        assert!(a.pin().chars().all(|c| c.is_ascii_digit()));
        assert_eq!(Mission::new(3, Kind::Pin, 42, 2, 0).pin().len(), 6);
    }

    #[test]
    fn runs_from_memorize_to_recall_after_a_turn() {
        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        let until = m.deadline().unwrap();
        assert!(!m.tick(until - 1));
        assert!(m.tick(until));
        assert_eq!(m.phase, Phase::Waiting { since: until, turns: 0 });
        assert!(m.turn_ended(until + 10));
        assert!(m.recalling());
    }

    #[test]
    fn recall_comes_when_the_wait_times_out() {
        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        let until = m.deadline().unwrap();
        m.tick(until);
        let due = m.deadline().unwrap();
        assert_eq!(due, until + 3 * 60_000);
        assert!(!m.tick(due - 1));
        assert!(m.tick(due));
        assert!(m.recalling());
    }

    #[test]
    fn turns_before_hiding_dont_count() {
        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        assert!(!m.turn_ended(10));
        assert!(matches!(m.phase, Phase::Memorize { .. }));
    }

    #[test]
    fn scores_places_right_and_doubles_a_perfect_answer() {
        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        m.phase = Phase::Recall;
        let pin = m.pin();
        assert!(m.submit(&pin, 5));
        assert_eq!(m.phase, Phase::Done { at: 5, score: 80, perfect: true });

        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        m.phase = Phase::Recall;
        let wrong: String = pin.chars().enumerate().map(|(i, c)| if i == 0 { if c == '0' { '1' } else { '0' } } else { c }).collect();
        m.submit(&wrong, 5);
        assert_eq!(m.score(), Some(30));
        assert_eq!(m.marks(), vec![false, true, true, true]);
    }

    #[test]
    fn can_only_answer_while_recalling() {
        let mut m = Mission::new(1, Kind::Pin, 7, 0, 0);
        assert!(!m.submit("1234", 1));
        assert!(!m.done());
        m.give_up(2);
        assert_eq!(m.score(), Some(0));
    }

    #[test]
    fn levels_follow_how_the_last_one_went() {
        let mut ms = Missions::default();
        assert_eq!(ms.next_level(Kind::Pin), 0);
        let pin = ms.start(Kind::Pin, 0).pin();
        let m = ms.active.as_mut().unwrap();
        m.phase = Phase::Recall;
        m.submit(&pin, 1);
        ms.file();
        assert_eq!(ms.next_level(Kind::Pin), 1);

        ms.start(Kind::Pin, 2);
        ms.active.as_mut().unwrap().give_up(3);
        ms.file();
        assert_eq!(ms.next_level(Kind::Pin), 0);
        assert_eq!(ms.history.len(), 2);
        assert!(ms.history[0].id > ms.history[1].id);
    }

    #[test]
    fn picks_up_the_unfinished_one_from_saved() {
        let mut done = Mission::new(1, Kind::Pin, 1, 0, 0);
        done.give_up(1);
        let open = Mission::new(2, Kind::Pin, 2, 0, 2);
        let ms = Missions::from_saved(vec![open.clone(), done.clone()]);
        assert_eq!(ms.active, Some(open));
        assert_eq!(ms.history, vec![done]);
        let mut ms = ms;
        assert_eq!(ms.start(Kind::Pin, 3).id, 3);
    }
}
