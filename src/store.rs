//! Threads on disk, one JSON file per project under the settings folder, so
//! they come back after a restart. The live connection isn't saved; a thread
//! keeps its agent's session id and reopens that session when next shown.
use crate::{
    agent::Step,
    model::{Block, Project, Thread, ThreadState},
    settings::{self, AgentSpec},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct SavedProject {
    path: Option<PathBuf>,
    /// When this was written, in ms since the epoch.
    #[serde(default)]
    saved_at: Option<i64>,
    threads: Vec<SavedThread>,
}

#[derive(Serialize, Deserialize)]
struct SavedThread {
    title: String,
    /// By name, since the configured list can change between runs.
    agent: String,
    state: ThreadState,
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    picks: Vec<(String, String)>,
    #[serde(default)]
    worktrees: std::collections::BTreeMap<PathBuf, PathBuf>,
    blocks: Vec<Block>,
    #[serde(default)]
    plan: Vec<(Step, String)>,
    #[serde(default)]
    usage: Option<(u64, u64)>,
    #[serde(default)]
    cost: Option<(f64, String)>,
    /// When a turn still running at the save began.
    #[serde(default)]
    turn_began: Option<i64>,
}

/// FNV-1a: a file name that stays the same for a path across runs and builds,
/// which std's hasher doesn't promise.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn file(path: Option<&Path>) -> Option<PathBuf> {
    let name = match path {
        Some(p) => format!("{:016x}.json", fnv(&p.to_string_lossy())),
        None => "chats.json".into(),
    };
    Some(settings::dir()?.join("threads").join(name))
}

/// The project's saved threads, newest first, numbered from `next_id`.
pub fn load(project: &Project, agents: &[AgentSpec], default_agent: usize, next_id: &mut usize) -> Vec<Thread> {
    let saved = file(project.path.as_deref())
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|bytes| serde_json::from_slice::<SavedProject>(&bytes).ok())
        // A name collision would hand over another folder's threads.
        .filter(|s| s.path == project.path);
    let Some(saved) = saved else {
        return Vec::new();
    };
    let saved_at = saved.saved_at;
    saved
        .threads
        .into_iter()
        .map(|s| {
            *next_id += 1;
            let agent = agents.iter().position(|a| a.name == s.agent).unwrap_or(default_agent);
            let mut thread = Thread::new(*next_id, agent);
            thread.title = s.title.into();
            thread.state = s.state;
            thread.unread = s.unread;
            thread.session_id = s.session;
            thread.picks = s.picks;
            thread.worktrees = s.worktrees;
            thread.blocks = s.blocks;
            thread.plan = s.plan;
            thread.usage = s.usage;
            thread.cost = s.cost;
            thread.turn_began = s.turn_began;
            thread.settle(saved_at);
            thread
        })
        .collect()
}

/// The same shapes, borrowing from the live threads so saving copies nothing.
#[derive(Serialize)]
struct ProjectOut<'a> {
    path: &'a Option<PathBuf>,
    saved_at: i64,
    threads: Vec<ThreadOut<'a>>,
}
#[derive(Serialize)]
struct ThreadOut<'a> {
    title: &'a str,
    agent: &'a str,
    state: ThreadState,
    unread: bool,
    session: &'a Option<String>,
    picks: Vec<(String, String)>,
    worktrees: &'a std::collections::BTreeMap<PathBuf, PathBuf>,
    blocks: &'a [Block],
    plan: &'a [(Step, String)],
    usage: Option<(u64, u64)>,
    cost: &'a Option<(f64, String)>,
    turn_began: Option<i64>,
}

/// The project as it should be written. Threads with nothing in them are left out.
pub fn snapshot(project: &Project, agents: &[AgentSpec]) -> Option<(PathBuf, Vec<u8>)> {
    let threads = project
        .threads
        .iter()
        .filter(|t| !t.blocks.is_empty())
        .map(|t| ThreadOut {
            title: &t.title,
            agent: agents.get(t.agent).map_or("", |a| a.name.as_str()),
            state: t.state,
            unread: t.unread,
            session: &t.session_id,
            picks: t.current_picks(),
            worktrees: &t.worktrees,
            blocks: &t.blocks,
            plan: &t.plan,
            usage: t.usage,
            cost: &t.cost,
            turn_began: t.turn_began,
        })
        .collect();
    let bytes = serde_json::to_vec(&ProjectOut { path: &project.path, saved_at: crate::model::now_ms(), threads }).ok()?;
    Some((file(project.path.as_deref())?, bytes))
}

/// Write a snapshot, replacing the old file only once the new one is whole.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Event;

    #[test]
    fn threads_survive_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("chat-store-{}", std::process::id()));
        let path = dir.join("p.json");
        let agents = AgentSpec::defaults();
        let mut project = Project::chats();
        let mut thread = Thread::new(1, 1);
        thread.title = "Add a greeting".into();
        thread.blocks.push(Block::User(crate::model::Message::now("add a greeting")));
        thread.apply(Event::Text("Done.".into()));
        thread.state = ThreadState::Running;
        thread.turn_began = Some(crate::model::now_ms() - 5_000);
        thread.session_id = Some("s-1".into());
        thread.picks = vec![("model".into(), "small".into())];
        project.threads.push(thread);
        project.threads.push(Thread::new(2, 0));

        let (_, bytes) = snapshot(&project, &agents).unwrap();
        write(&path, &bytes).unwrap();
        let saved: SavedProject = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.threads.len(), 1, "empty threads aren't saved");
        assert_eq!(saved.threads[0].agent, "Codex");

        let mut restored = Thread::new(9, 0);
        restored.blocks = saved.threads[0].blocks.iter().map(|b| serde_json::from_value(serde_json::to_value(b).unwrap()).unwrap()).collect();
        restored.state = saved.threads[0].state;
        restored.turn_began = saved.threads[0].turn_began;
        restored.settle(saved.saved_at);
        assert!(restored.state == ThreadState::Idle, "a turn cut off by quitting comes back stopped");
        assert!(matches!(&restored.blocks[0], Block::User(m) if m.at.is_some()), "a message keeps its time");
        assert!(
            matches!(restored.blocks.last(), Some(Block::TurnEnded { worked_ms, .. }) if (5_000..60_000).contains(worked_ms)),
            "a cut-off turn says how long it ran, up to the last save"
        );
        assert!(matches!(&restored.blocks[1], Block::Text(t) if t == "Done."));
        assert_eq!(saved.threads[0].picks, [("model".to_string(), "small".to_string())]);
        assert_eq!(saved.threads[0].session.as_deref(), Some("s-1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_names_are_stable() {
        assert_eq!(fnv("/Users/me/code/chat"), fnv("/Users/me/code/chat"));
        assert_ne!(fnv("/a"), fnv("/b"));
        assert_eq!(fnv(""), 0xcbf29ce484222325);
    }
}
