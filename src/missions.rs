//! Missions: tiny games that arrive now and then while agents work, from the
//! mail icon in the status bar. They belong to the app, not a thread.
//!
//! The PIN mission is the first: it's offered, and once accepted its digits
//! show for a moment, then hide. A few turns later (agent turns ending,
//! anywhere) it asks for the PIN back, with two tries. Getting it right adds
//! to the streak; missing it twice ends the streak.
//!
//! A mission keeps its place across restarts: what's saved is its phase, the
//! turns still to wait and the tries used. One showing its digits when the app
//! quit comes back hidden, waiting.

/// The cells a PIN shows in; PINs sit in the middle of them.
pub const SLOTS: usize = 8;
/// Tries at recalling a PIN.
pub const TRIES: u8 = 2;
/// How long each digit stays on show, once they've all rolled in.
pub const VIEW_MS_PER_DIGIT: i64 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting in the mailbox to be accepted.
    Offered,
    /// The digits are on show from `since` (Unix ms) until `until`.
    Showing { since: i64, until: i64 },
    /// Hidden; asks for the PIN once `turns_left` more turns end.
    Waiting { turns_left: u32 },
    /// Asking for the PIN; `tries` wrong answers given so far.
    Recall { tries: u8 },
    /// Answered: right, or wrong on the last try.
    Done { solved: bool },
}

impl Phase {
    fn key(self) -> &'static str {
        match self {
            // Saved hidden: the digits aren't shown again after a restart.
            Phase::Offered => "offered",
            Phase::Showing { .. } | Phase::Waiting { .. } => "waiting",
            Phase::Recall { .. } => "recall",
            Phase::Done { .. } => "done",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mission {
    pub id: i64,
    pub pin: String,
    pub phase: Phase,
    /// Turns to wait between hiding the PIN and asking for it.
    pub wait_turns: u32,
    /// The last full answer given, wrong or right.
    pub guess: Option<String>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

/// A row as saved, before it's checked.
pub struct Saved {
    pub id: i64,
    pub pin: String,
    pub phase: String,
    pub wait_turns: i64,
    pub turns_left: i64,
    pub tries: i64,
    pub guess: Option<String>,
    pub solved: Option<bool>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

/// A row to save.
pub struct Row<'a> {
    pub id: i64,
    pub kind: &'static str,
    pub pin: &'a str,
    pub phase: &'static str,
    pub wait_turns: i64,
    pub turns_left: i64,
    pub tries: i64,
    pub guess: Option<&'a str>,
    pub solved: Option<bool>,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

impl Mission {
    pub fn new(id: i64, seed: u64, now: i64) -> Mission {
        let mut rng = Rng(seed);
        // 4 to 6 digits for now; fitting the length to how the player does comes later.
        let len = 4 + (rng.next() % 3) as usize;
        let pin = (0..len).map(|_| char::from(b'0' + (rng.next() % 10) as u8)).collect();
        let wait_turns = 2 + (rng.next() % 2) as u32;
        Mission { id, pin, phase: Phase::Offered, wait_turns, guess: None, created_at: now, ended_at: None }
    }

    /// Start showing the digits; `roll_ms` is how long they take to roll in,
    /// after which each digit has its time on show.
    pub fn accept(&mut self, now: i64, roll_ms: i64) -> bool {
        if self.phase != Phase::Offered {
            return false;
        }
        let until = now + roll_ms + VIEW_MS_PER_DIGIT * self.pin.len() as i64;
        self.phase = Phase::Showing { since: now, until };
        true
    }

    /// Move on with the clock. Returns whether the phase changed.
    pub fn tick(&mut self, now: i64) -> bool {
        if let Phase::Showing { until, .. } = self.phase
            && now >= until
        {
            self.phase = Phase::Waiting { turns_left: self.wait_turns };
            return true;
        }
        false
    }

    /// When the phase next changes by itself.
    pub fn deadline(&self) -> Option<i64> {
        match self.phase {
            Phase::Showing { until, .. } => Some(until),
            _ => None,
        }
    }

    /// A turn ended somewhere. Returns whether anything changed.
    pub fn turn_ended(&mut self) -> bool {
        let Phase::Waiting { turns_left } = &mut self.phase else { return false };
        *turns_left = turns_left.saturating_sub(1);
        if *turns_left == 0 {
            self.phase = Phase::Recall { tries: 0 };
        }
        true
    }

    /// Check a full answer, while recalling. `Some(true)` when it's right,
    /// `Some(false)` when it's wrong (the mission ends if that was the last try).
    pub fn submit(&mut self, answer: &str, now: i64) -> Option<bool> {
        let Phase::Recall { tries } = self.phase else { return None };
        if answer.len() != self.pin.len() {
            return None;
        }
        self.guess = Some(answer.to_string());
        let right = answer == self.pin;
        if right || tries + 1 >= TRIES {
            self.phase = Phase::Done { solved: right };
            self.ended_at = Some(now);
        } else {
            self.phase = Phase::Recall { tries: tries + 1 };
        }
        Some(right)
    }

    /// Waiting on the player: offered, or asking for the PIN.
    pub fn needs_you(&self) -> bool {
        matches!(self.phase, Phase::Offered | Phase::Recall { .. })
    }

    pub fn row(&self) -> Row<'_> {
        let (turns_left, tries, solved) = match self.phase {
            Phase::Offered => (self.wait_turns, 0, None),
            Phase::Showing { .. } => (self.wait_turns, 0, None),
            Phase::Waiting { turns_left } => (turns_left, 0, None),
            Phase::Recall { tries } => (0, tries, None),
            Phase::Done { solved } => (0, 0, Some(solved)),
        };
        Row {
            id: self.id,
            kind: "pin",
            pin: &self.pin,
            phase: self.phase.key(),
            wait_turns: self.wait_turns as i64,
            turns_left: turns_left as i64,
            tries: tries as i64,
            guess: self.guess.as_deref(),
            solved,
            created_at: self.created_at,
            ended_at: self.ended_at,
        }
    }

    /// From a saved row; `None` for one this build can't make sense of.
    pub fn from_saved(s: Saved) -> Option<Mission> {
        if s.pin.is_empty() || s.pin.len() > SLOTS || !s.pin.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let phase = match s.phase.as_str() {
            "offered" => Phase::Offered,
            "waiting" if s.turns_left <= 0 => Phase::Recall { tries: 0 },
            "waiting" => Phase::Waiting { turns_left: s.turns_left as u32 },
            "recall" => Phase::Recall { tries: s.tries.clamp(0, TRIES as i64 - 1) as u8 },
            "done" => Phase::Done { solved: s.solved? },
            _ => return None,
        };
        Some(Mission {
            id: s.id,
            pin: s.pin,
            phase,
            wait_turns: s.wait_turns.clamp(1, 10) as u32,
            guess: s.guess,
            created_at: s.created_at,
            ended_at: s.ended_at,
        })
    }
}

/// The mission in the mailbox, if any, and how the last ones went.
#[derive(Default)]
pub struct Missions {
    /// Offered, under way, or done but not yet put away.
    pub active: Option<Mission>,
    /// Right answers in a row, up to now.
    pub streak: u32,
    next_id: i64,
    /// Turns ended since the mailbox was last emptied; a new mission waits a few.
    idle_turns: u32,
}

/// Turns that end, at least, between one mission and the next being offered.
const MIN_IDLE_TURNS: u32 = 2;
/// After that, the chance a turn ending brings a mission, in 1/N.
const OFFER_ODDS: u64 = 3;

impl Missions {
    /// From what was saved, newest first.
    pub fn from_saved(saved: Vec<Mission>) -> Missions {
        let next_id = saved.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        // Only the newest can still be under way; a finished one was put away
        // already, or is as good as.
        let active = saved.first().filter(|m| !matches!(m.phase, Phase::Done { .. })).cloned();
        let streak = saved
            .iter()
            .filter_map(|m| match m.phase {
                Phase::Done { solved } => Some(solved),
                _ => None,
            })
            .take_while(|solved| *solved)
            .count() as u32;
        Missions { active, streak, next_id, idle_turns: 0 }
    }

    /// Offer a new mission, if the mailbox is empty.
    pub fn offer(&mut self, now: i64) -> Option<&Mission> {
        if self.active.is_some() {
            return None;
        }
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id += 1;
        let seed = (now as u64) ^ (id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.idle_turns = 0;
        Some(self.active.insert(Mission::new(id, seed, now)))
    }

    /// A turn ended somewhere: a waiting mission counts it, and an empty
    /// mailbox may get a new one. Returns whether anything changed.
    pub fn turn_ended(&mut self, now: i64) -> bool {
        if let Some(m) = &mut self.active {
            return m.turn_ended();
        }
        self.idle_turns += 1;
        let lucky = Rng(now as u64 ^ 0x5bd1_e995).next().is_multiple_of(OFFER_ODDS);
        self.idle_turns >= MIN_IDLE_TURNS && lucky && self.offer(now).is_some()
    }

    /// Answer the active mission, keeping the streak.
    pub fn submit(&mut self, answer: &str, now: i64) -> Option<bool> {
        let m = self.active.as_mut()?;
        let right = m.submit(answer, now)?;
        match m.phase {
            Phase::Done { solved: true } => self.streak += 1,
            Phase::Done { solved: false } => self.streak = 0,
            _ => {}
        }
        Some(right)
    }

    /// Put a finished mission away, emptying the mailbox.
    pub fn put_away(&mut self) -> bool {
        self.active.take_if(|m| matches!(m.phase, Phase::Done { .. })).is_some()
    }
}

/// SplitMix64: small, and the same everywhere.
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

    fn recalling(pin: &str) -> Mission {
        let mut m = Mission::new(1, 7, 0);
        m.pin = pin.into();
        m.phase = Phase::Recall { tries: 0 };
        m
    }

    #[test]
    fn makes_a_pin_of_four_to_six_digits() {
        for seed in 0..50 {
            let m = Mission::new(1, seed, 0);
            assert!((4..=6).contains(&m.pin.len()), "{}", m.pin);
            assert!(m.pin.bytes().all(|b| b.is_ascii_digit()));
            assert!((2..=3).contains(&m.wait_turns));
        }
    }

    #[test]
    fn runs_from_offer_through_waiting_to_recall() {
        let mut m = Mission::new(1, 7, 0);
        assert!(!m.turn_ended(), "an offer doesn't count turns");
        assert!(m.accept(100, 500));
        let until = m.deadline().unwrap();
        assert_eq!(until, 100 + 500 + VIEW_MS_PER_DIGIT * m.pin.len() as i64);
        assert!(!m.turn_ended(), "nor does the PIN on show");
        assert!(!m.tick(until - 1));
        assert!(m.tick(until));
        for _ in 1..m.wait_turns {
            assert!(m.turn_ended());
            assert!(matches!(m.phase, Phase::Waiting { .. }));
        }
        assert!(m.turn_ended());
        assert_eq!(m.phase, Phase::Recall { tries: 0 });
    }

    #[test]
    fn gives_two_tries() {
        let mut m = recalling("2748");
        assert_eq!(m.submit("27", 1), None, "a partial answer isn't checked");
        assert_eq!(m.submit("2193", 1), Some(false));
        assert_eq!(m.phase, Phase::Recall { tries: 1 });
        assert_eq!(m.submit("2193", 2), Some(false));
        assert_eq!(m.phase, Phase::Done { solved: false });
        assert_eq!(m.guess.as_deref(), Some("2193"));
        assert_eq!(m.submit("2748", 3), None, "done is done");

        let mut m = recalling("2748");
        m.submit("0000", 1);
        assert_eq!(m.submit("2748", 2), Some(true));
        assert_eq!(m.phase, Phase::Done { solved: true });
    }

    #[test]
    fn keeps_the_streak() {
        let mut ms = Missions::from_saved(vec![]);
        ms.offer(0);
        ms.active.as_mut().unwrap().pin = "11".into();
        ms.active.as_mut().unwrap().phase = Phase::Recall { tries: 0 };
        assert_eq!(ms.submit("11", 1), Some(true));
        assert_eq!(ms.streak, 1);
        assert!(ms.offer(2).is_none(), "one mission at a time");
        assert!(ms.put_away());
        ms.offer(3);
        ms.active.as_mut().unwrap().phase = Phase::Recall { tries: 1 };
        let wrong: String = ms.active.as_ref().unwrap().pin.chars().map(|c| if c == '0' { '1' } else { '0' }).collect();
        assert_eq!(ms.submit(&wrong, 4), Some(false));
        assert!(matches!(ms.active.as_ref().unwrap().phase, Phase::Done { solved: false }));
        assert_eq!(ms.streak, 0);
    }

    #[test]
    fn comes_back_from_saved() {
        let mut done = recalling("12");
        done.id = 3;
        done.submit("12", 5);
        let mut older = recalling("34");
        older.id = 2;
        older.submit("34", 4);
        let mut missed = recalling("5");
        missed.id = 1;
        missed.phase = Phase::Recall { tries: 1 };
        missed.submit("6", 3);
        let ms = Missions::from_saved(vec![done.clone(), older, missed]);
        assert_eq!(ms.streak, 2);
        assert!(ms.active.is_none(), "a finished mission isn't brought back");

        let mut showing = Mission::new(4, 1, 0);
        showing.accept(0, 0);
        let row = showing.row();
        let saved = Saved {
            id: row.id,
            pin: row.pin.into(),
            phase: row.phase.into(),
            wait_turns: row.wait_turns,
            turns_left: row.turns_left,
            tries: row.tries,
            guess: None,
            solved: row.solved,
            created_at: row.created_at,
            ended_at: row.ended_at,
        };
        let back = Mission::from_saved(saved).unwrap();
        assert_eq!(back.phase, Phase::Waiting { turns_left: showing.wait_turns }, "comes back hidden");
        let ms = Missions::from_saved(vec![back, done]);
        assert_eq!(ms.active.as_ref().map(|m| m.id), Some(4));
        assert_eq!(ms.streak, 1);
    }

    #[test]
    fn offers_now_and_then_when_the_mailbox_is_empty() {
        let mut ms = Missions::default();
        let offered = (0..200).filter(|t| {
            let got = ms.turn_ended(*t * 7919);
            if got {
                ms.active = None;
            }
            got
        });
        let n = offered.count();
        assert!(n > 10 && n < 100, "{n}");
    }
}
