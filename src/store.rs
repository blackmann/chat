//! Threads on disk, in one SQLite database under the settings folder, so they
//! come back after a restart. The live connection isn't saved; a thread keeps
//! its agent's session id and reopens that session when next shown.
//!
//! At startup only what the thread list needs is read: titles, states, times.
//! A thread's blocks are read when it's opened, and dropped again once it has
//! been put away for a while. Saving writes each thread's details and only the
//! blocks that changed since the last save, in one transaction, off the main
//! thread. What was said each way is also kept in a full-text index, for
//! searching threads by what's in them.
use crate::{
    agent::Step,
    model::{Block, Project, Thread, ThreadState},
    settings::{self, AgentSpec},
};
use rusqlite::{Connection, params};

mod migrations;
use serde::{Deserialize, Serialize};
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// A block's row in the text index: its thread's id, shifted, plus its place.
/// Rows are found and replaced by this, without scanning the index.
const SEQ_BITS: i64 = 24;
fn text_row(thread: i64, seq: i64) -> i64 {
    (thread << SEQ_BITS) | seq
}

/// The database, open for the main thread's reads, with a second connection
/// that saves run on.
pub struct Store {
    path: PathBuf,
    read: Connection,
    writer: Arc<Mutex<Connection>>,
    /// The id the next saved thread gets.
    next_id: i64,
}

fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

/// Where a project's threads are filed.
fn project_key(project: &Project) -> String {
    project.path.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn hash(body: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    h.finish()
}

/// The text a block adds to the index: what was said each way, leaving out
/// tool output and the agent's private reasoning.
fn said(block: &Block) -> Option<&str> {
    match block {
        Block::User(m) => Some(&m.text),
        Block::Text(t) => Some(t),
        _ => None,
    }
}

impl Store {
    /// The database in the settings folder, made and filled from the old
    /// thread files the first time.
    pub fn open() -> Option<Store> {
        let dir = settings::dir()?;
        match Store::open_in(&dir) {
            Ok(store) => Some(store),
            Err(e) => {
                eprintln!("chat: can't open the thread database in {}: {e}", dir.display());
                None
            }
        }
    }

    fn open_in(dir: &Path) -> rusqlite::Result<Store> {
        std::fs::create_dir_all(dir).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let path = dir.join("chat.db");
        let mut writer = connect(&path)?;
        let mut imported = Vec::new();
        migrations::run(&mut writer, migrations::MIGRATIONS, |name, tx| {
            // The first database is filled from the old thread files.
            if name == migrations::MIGRATIONS[0].0 {
                imported = import_json(tx, &dir.join("threads"))?;
            }
            Ok(())
        })?;
        // Kept, renamed, in case anything went wrong; not read again.
        for file in imported {
            let _ = std::fs::rename(&file, file.with_extension("json.old"));
        }
        let next_id = writer.query_row("SELECT coalesce(max(id), 0) + 1 FROM threads", [], |r| r.get(0))?;
        Ok(Store { read: connect(&path)?, writer: Arc::new(Mutex::new(writer)), path, next_id })
    }

    /// The project's saved threads, numbered from `next_id`, without their
    /// blocks. A thread cut off by quitting mid-turn is read whole and put
    /// to rest, so it doesn't show as still working.
    pub fn load(&self, project: &Project, agents: &[AgentSpec], default_agent: usize, next_id: &mut usize) -> Vec<Thread> {
        let rows = self.read_threads(&project_key(project));
        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("chat: can't read threads: {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .map(|row| {
                *next_id += 1;
                let mut thread = Thread::new(*next_id, agents.iter().position(|a| a.name == row.agent).unwrap_or(default_agent));
                thread.stored = Some(row.id);
                thread.unloaded = true;
                thread.title = row.title.into();
                thread.state = row.state;
                thread.unread = row.unread;
                thread.session_id = row.session;
                thread.picks = row.picks;
                thread.worktrees = row.worktrees;
                thread.plan = row.plan;
                thread.usage = row.usage;
                thread.cost = row.cost;
                thread.turn_began = row.turn_began;
                thread.created_at = row.created_at;
                thread.active_at = row.active_at;
                if thread.state.busy() {
                    self.load_blocks(&mut thread);
                    thread.settle(Some(row.saved_at));
                }
                thread
            })
            .collect()
    }

    fn read_threads(&self, project: &str) -> rusqlite::Result<Vec<ThreadRow>> {
        let mut query = self.read.prepare_cached(
            "SELECT t.id, title, agent, state, unread, session, picks, worktrees, plan, usage, cost,
                    turn_began, created_at, active_at, saved_at
             FROM threads t JOIN projects p ON p.id = t.project
             WHERE p.path = ?1 ORDER BY t.position",
        )?;
        let rows = query.query_map([project], |r| {
            let json = |i: usize| r.get::<_, Option<String>>(i);
            Ok(ThreadRow {
                id: r.get(0)?,
                title: r.get(1)?,
                agent: r.get(2)?,
                state: parse(json(3)?).unwrap_or(ThreadState::Idle),
                unread: r.get(4)?,
                session: r.get(5)?,
                picks: parse(json(6)?).unwrap_or_default(),
                worktrees: parse(json(7)?).unwrap_or_default(),
                plan: parse(json(8)?).unwrap_or_default(),
                usage: parse(json(9)?),
                cost: parse(json(10)?),
                turn_began: r.get(11)?,
                created_at: r.get(12)?,
                active_at: r.get(13)?,
                saved_at: r.get(14)?,
            })
        })?;
        rows.collect()
    }

    /// Read a saved thread's blocks back in, if they aren't already.
    pub fn load_blocks(&self, thread: &mut Thread) {
        let Some(id) = thread.stored.filter(|_| thread.unloaded) else { return };
        let read = || -> rusqlite::Result<Vec<String>> {
            let mut query = self.read.prepare_cached("SELECT body FROM blocks WHERE thread = ?1 ORDER BY seq")?;
            let rows = query.query_map([id], |r| r.get::<_, String>(0))?;
            rows.collect()
        };
        let bodies = match read() {
            Ok(bodies) => bodies,
            Err(e) => {
                // Left unloaded: a save won't write over what couldn't be read.
                eprintln!("chat: can't read thread {id}: {e}");
                return;
            }
        };
        thread.saved = bodies.iter().map(|b| hash(b)).collect();
        thread.blocks = bodies
            .iter()
            // A block this build can't read takes its place as a notice; it's
            // written back as that, the rest of the thread still readable.
            .map(|b| serde_json::from_str(b).unwrap_or_else(|_| Block::Notice("Part of this thread couldn't be read.".into())))
            .collect();
        thread.unloaded = false;
    }

    /// What a save has to write for these projects, worked out on the main
    /// thread: every thread's details, and the blocks that changed since the
    /// last save. The threads are marked saved as of now.
    pub fn plan(&mut self, projects: &mut [Project], agents: &[AgentSpec]) -> Plan {
        let now = crate::model::now_ms();
        let projects = projects
            .iter_mut()
            .map(|project| {
                let key = project_key(project);
                let threads = project
                    .threads
                    .iter_mut()
                    .filter(|t| !t.is_blank())
                    .enumerate()
                    .map(|(position, t)| {
                        let id = *t.stored.get_or_insert_with(|| {
                            self.next_id += 1;
                            self.next_id - 1
                        });
                        let blocks = (!t.unloaded).then(|| changed_blocks(t));
                        ThreadPlan {
                            id,
                            position: position as i64,
                            title: t.title.to_string(),
                            agent: agents.get(t.agent).map_or("", |a| a.name.as_str()).to_string(),
                            state: json(&t.state),
                            unread: t.unread,
                            session: t.session_id.clone(),
                            picks: json(&t.current_picks()),
                            worktrees: json(&t.worktrees),
                            plan: json(&t.plan),
                            usage: t.usage.map(|u| json(&u)),
                            cost: t.cost.as_ref().map(json),
                            turn_began: t.turn_began,
                            created_at: t.created_at,
                            active_at: t.active_at,
                            blocks,
                        }
                    })
                    .collect();
                ProjectPlan { key, threads }
            })
            .collect();
        Plan { saved_at: now, projects }
    }

    /// The connection saves are written on, to hand to a background thread.
    pub fn writer(&self) -> Arc<Mutex<Connection>> {
        self.writer.clone()
    }

    /// Where the database is, for a search to open its own connection.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The blocks of `thread` that differ from what was last saved, and how many
/// there are now; the saved hashes move up to match.
fn changed_blocks(thread: &mut Thread) -> BlockChanges {
    let mut writes = Vec::new();
    let mut saved = Vec::with_capacity(thread.blocks.len());
    for (seq, block) in thread.blocks.iter().enumerate() {
        let Ok(body) = serde_json::to_string(block) else { continue };
        let h = hash(&body);
        if thread.saved.get(seq) != Some(&h) {
            writes.push(BlockWrite { seq: seq as i64, text: said(block).map(str::to_string), body });
        }
        saved.push(h);
    }
    thread.saved = saved;
    BlockChanges { len: thread.blocks.len() as i64, writes }
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn parse<T: for<'de> Deserialize<'de>>(text: Option<String>) -> Option<T> {
    serde_json::from_str(&text?).ok()
}

struct ThreadRow {
    id: i64,
    title: String,
    agent: String,
    state: ThreadState,
    unread: bool,
    session: Option<String>,
    picks: Vec<(String, String)>,
    worktrees: std::collections::BTreeMap<PathBuf, PathBuf>,
    plan: Vec<(Step, String)>,
    usage: Option<(u64, u64)>,
    cost: Option<(f64, String)>,
    turn_began: Option<i64>,
    created_at: Option<i64>,
    active_at: Option<i64>,
    saved_at: i64,
}

/// Everything one save writes. Plain data, so it can cross to the thread that
/// writes it.
pub struct Plan {
    saved_at: i64,
    projects: Vec<ProjectPlan>,
}
impl Plan {
    /// The threads this save writes no blocks for: what they hold is already
    /// saved, so they can be let go of without waiting on the write.
    pub fn untouched(&self) -> std::collections::HashSet<i64> {
        self.projects
            .iter()
            .flat_map(|p| &p.threads)
            .filter(|t| t.blocks.as_ref().is_some_and(|b| b.writes.is_empty()))
            .map(|t| t.id)
            .collect()
    }
}
struct ProjectPlan {
    key: String,
    threads: Vec<ThreadPlan>,
}
struct ThreadPlan {
    id: i64,
    position: i64,
    title: String,
    agent: String,
    state: String,
    unread: bool,
    session: Option<String>,
    picks: String,
    worktrees: String,
    plan: String,
    usage: Option<String>,
    cost: Option<String>,
    turn_began: Option<i64>,
    created_at: Option<i64>,
    active_at: Option<i64>,
    /// `None` for a thread whose blocks aren't read in: they stay as they are.
    blocks: Option<BlockChanges>,
}
struct BlockChanges {
    /// How many blocks the thread has; saved ones past this go.
    len: i64,
    writes: Vec<BlockWrite>,
}
struct BlockWrite {
    seq: i64,
    body: String,
    /// What it adds to the text index, if anything.
    text: Option<String>,
}

/// Write a save's plan, all of it or none. Threads of these projects that
/// aren't in it any more go, with their blocks.
pub fn write(conn: &Mutex<Connection>, plan: &Plan) -> rusqlite::Result<()> {
    let mut conn = conn.lock().unwrap_or_else(|e| e.into_inner());
    let tx = conn.transaction()?;
    for project in &plan.projects {
        tx.execute("INSERT INTO projects (path) VALUES (?1) ON CONFLICT(path) DO NOTHING", [&project.key])?;
        let project_id: i64 = tx.query_row("SELECT id FROM projects WHERE path = ?1", [&project.key], |r| r.get(0))?;
        // Gone from the list: forget them.
        let keep: std::collections::HashSet<i64> = project.threads.iter().map(|t| t.id).collect();
        let stored: Vec<i64> = {
            let mut query = tx.prepare_cached("SELECT id FROM threads WHERE project = ?1")?;
            let rows = query.query_map([project_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for id in stored.into_iter().filter(|id| !keep.contains(id)) {
            tx.execute("DELETE FROM block_text WHERE rowid BETWEEN ?1 AND ?2", [text_row(id, 0), text_row(id + 1, 0) - 1])?;
            tx.execute("DELETE FROM threads WHERE id = ?1", [id])?;
        }
        for t in &project.threads {
            write_thread(&tx, project_id, t, plan.saved_at)?;
        }
    }
    tx.commit()
}

fn write_thread(tx: &rusqlite::Transaction, project: i64, t: &ThreadPlan, saved_at: i64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO threads (id, project, position, title, agent, state, unread, session, picks, worktrees,
                              plan, usage, cost, turn_began, created_at, active_at, saved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(id) DO UPDATE SET
            project = excluded.project, position = excluded.position, title = excluded.title,
            agent = excluded.agent, state = excluded.state, unread = excluded.unread,
            session = excluded.session, picks = excluded.picks, worktrees = excluded.worktrees,
            plan = excluded.plan, usage = excluded.usage, cost = excluded.cost,
            turn_began = excluded.turn_began, created_at = excluded.created_at,
            active_at = excluded.active_at, saved_at = excluded.saved_at",
    )?
    .execute(params![
        t.id, project, t.position, t.title, t.agent, t.state, t.unread, t.session, t.picks, t.worktrees,
        t.plan, t.usage, t.cost, t.turn_began, t.created_at, t.active_at, saved_at
    ])?;
    let Some(blocks) = &t.blocks else { return Ok(()) };
    for block in &blocks.writes {
        tx.prepare_cached("INSERT OR REPLACE INTO blocks (thread, seq, body) VALUES (?1, ?2, ?3)")?
            .execute(params![t.id, block.seq, block.body])?;
        let row = text_row(t.id, block.seq);
        tx.prepare_cached("DELETE FROM block_text WHERE rowid = ?1")?.execute([row])?;
        if let Some(text) = &block.text {
            tx.prepare_cached("INSERT INTO block_text (rowid, text) VALUES (?1, ?2)")?.execute(params![row, text])?;
        }
    }
    tx.prepare_cached("DELETE FROM blocks WHERE thread = ?1 AND seq >= ?2")?.execute([t.id, blocks.len])?;
    tx.prepare_cached("DELETE FROM block_text WHERE rowid BETWEEN ?1 AND ?2")?
        .execute([text_row(t.id, blocks.len), text_row(t.id + 1, 0) - 1])?;
    Ok(())
}

/// The saved text of `project`'s threads that holds `needle`, ignoring case,
/// as (thread id, block, text), in order. Opens its own connection, so it can
/// run off the main thread while saves go on.
pub fn search(db: &Path, project: &str, needle: &str) -> rusqlite::Result<Vec<(i64, usize, String)>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Three characters or more are looked up by trigram; shorter ones can't
    // be, so LIKE reads through the index's text instead.
    let (filter, arg) = if needle.chars().count() >= 3 {
        ("block_text MATCH ?2", format!("\"{}\"", needle.replace('"', "\"\"")))
    } else {
        let escaped = needle.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        ("text LIKE ?2 ESCAPE '\\'", format!("%{escaped}%"))
    };
    let sql = format!(
        "SELECT b.rowid, b.text FROM block_text b
         JOIN threads t ON t.id = (b.rowid >> {SEQ_BITS})
         JOIN projects p ON p.id = t.project
         WHERE p.path = ?1 AND {filter}
         ORDER BY b.rowid"
    );
    let mut query = conn.prepare(&sql)?;
    let rows = query.query_map(params![project, arg], |r| {
        let row: i64 = r.get(0)?;
        Ok((row >> SEQ_BITS, (row & ((1 << SEQ_BITS) - 1)) as usize, r.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// The search key for a project, as `search` takes it.
pub fn search_key(project: &Project) -> String {
    project_key(project)
}

// ── The old thread files ────────────────────────────────────────────────────
//
// Before the database, each project's threads were one JSON file under
// `threads/`. They're read in once, when the database is made.

#[derive(Deserialize)]
struct SavedProject {
    path: Option<PathBuf>,
    #[serde(default)]
    saved_at: Option<i64>,
    threads: Vec<SavedThread>,
}

#[derive(Deserialize)]
struct SavedThread {
    title: String,
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
    #[serde(default)]
    turn_began: Option<i64>,
    #[serde(default)]
    created_at: Option<i64>,
    #[serde(default)]
    active_at: Option<i64>,
}

/// Read every old thread file in `dir` into the database. Says which files
/// went in; one that can't be read is left where it is.
fn import_json(tx: &rusqlite::Transaction, dir: &Path) -> rusqlite::Result<Vec<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(Vec::new()) };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    let mut imported = Vec::new();
    let mut next_id: i64 = 1;
    for file in files {
        let Some(saved) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<SavedProject>(&b).ok()) else {
            eprintln!("chat: left {} as it is: it couldn't be read", file.display());
            continue;
        };
        let key = saved.path.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        tx.execute("INSERT INTO projects (path) VALUES (?1) ON CONFLICT(path) DO NOTHING", [&key])?;
        let project: i64 = tx.query_row("SELECT id FROM projects WHERE path = ?1", [&key], |r| r.get(0))?;
        let saved_at = saved.saved_at.unwrap_or(0);
        for (position, s) in saved.threads.into_iter().enumerate() {
            let id = next_id;
            next_id += 1;
            let writes = s
                .blocks
                .iter()
                .enumerate()
                .map(|(seq, b)| BlockWrite { seq: seq as i64, body: json(b), text: said(b).map(str::to_string) })
                .collect();
            let plan = ThreadPlan {
                id,
                position: position as i64,
                title: s.title,
                agent: s.agent,
                state: json(&s.state),
                unread: s.unread,
                session: s.session,
                picks: json(&s.picks),
                worktrees: json(&s.worktrees),
                plan: json(&s.plan),
                usage: s.usage.map(|u| json(&u)),
                cost: s.cost.as_ref().map(json),
                turn_began: s.turn_began,
                created_at: s.created_at,
                active_at: s.active_at,
                blocks: Some(BlockChanges { len: s.blocks.len() as i64, writes }),
            };
            write_thread(tx, project, &plan, saved_at)?;
        }
        imported.push(file);
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agent::Event, model::Message};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chat-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save(store: &mut Store, projects: &mut [Project], agents: &[AgentSpec]) {
        let plan = store.plan(projects, agents);
        write(&store.writer(), &plan).unwrap();
    }

    #[test]
    fn threads_survive_a_round_trip_and_load_when_opened() {
        let dir = temp("round-trip");
        let agents = AgentSpec::defaults();
        let mut store = Store::open_in(&dir).unwrap();
        let mut project = Project::chats();
        let mut thread = Thread::new(1, 1);
        thread.title = "Add a greeting".into();
        thread.blocks.push(Block::User(Message::now("add a greeting")));
        thread.apply(Event::Text("Done.".into()));
        thread.session_id = Some("s-1".into());
        thread.picks = vec![("model".into(), "small".into())];
        thread.mark_active(1_000);
        project.threads.push(thread);
        project.threads.push(Thread::new(2, 0));
        let mut projects = [project];
        save(&mut store, &mut projects, &agents);

        let store = Store::open_in(&dir).unwrap();
        let mut next = 0;
        let mut threads = store.load(&projects[0], &agents, 0, &mut next);
        assert_eq!(threads.len(), 1, "empty threads aren't saved");
        let t = &mut threads[0];
        assert!(t.unloaded && t.blocks.is_empty(), "blocks wait until it's opened");
        assert!(!t.is_blank(), "and it isn't taken for a new thread meanwhile");
        assert_eq!((t.title.as_ref(), t.agent, t.created_at), ("Add a greeting", 1, Some(1_000)));
        assert_eq!(t.session_id.as_deref(), Some("s-1"));
        assert_eq!(t.picks, [("model".to_string(), "small".to_string())]);
        store.load_blocks(t);
        assert!(!t.unloaded);
        assert!(matches!(&t.blocks[0], Block::User(m) if m.at.is_some()), "a message keeps its time");
        assert!(matches!(&t.blocks[1], Block::Text(x) if x == "Done."));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_turn_cut_off_by_quitting_comes_back_stopped() {
        let dir = temp("cut-off");
        let agents = AgentSpec::defaults();
        let mut store = Store::open_in(&dir).unwrap();
        let mut thread = Thread::new(1, 0);
        thread.blocks.push(Block::User(Message::now("go")));
        thread.state = ThreadState::Running;
        thread.turn_began = Some(crate::model::now_ms() - 5_000);
        let mut project = Project::chats();
        project.threads.push(thread);
        let mut projects = [project];
        save(&mut store, &mut projects, &agents);

        let mut next = 0;
        let threads = Store::open_in(&dir).unwrap().load(&projects[0], &agents, 0, &mut next);
        let t = &threads[0];
        assert!(t.state == ThreadState::Idle && !t.unloaded, "read whole to be put to rest");
        assert!(
            matches!(t.blocks.last(), Some(Block::TurnEnded { worked_ms, .. }) if (5_000..60_000).contains(worked_ms)),
            "a cut-off turn says how long it ran, up to the last save"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_write_only_what_changed() {
        let dir = temp("changes");
        let agents = AgentSpec::defaults();
        let mut store = Store::open_in(&dir).unwrap();
        let mut thread = Thread::new(1, 0);
        thread.blocks.push(Block::User(Message::now("one")));
        thread.blocks.push(Block::Text("two".into()));
        let mut project = Project::chats();
        project.threads.push(thread);
        let mut projects = [project];
        let writes = |plan: &Plan| plan.projects[0].threads[0].blocks.as_ref().map(|b| b.writes.iter().map(|w| w.seq).collect::<Vec<_>>());
        let plan = store.plan(&mut projects, &agents);
        assert_eq!(writes(&plan), Some(vec![0, 1]));
        write(&store.writer(), &plan).unwrap();
        // A chunk streams into the last block.
        projects[0].threads[0].apply(Event::Text(" and more".into()));
        let plan = store.plan(&mut projects, &agents);
        assert_eq!(writes(&plan), Some(vec![1]), "only the block that grew");
        write(&store.writer(), &plan).unwrap();
        assert_eq!(writes(&store.plan(&mut projects, &agents)), Some(vec![]), "nothing new, nothing written");

        let mut next = 0;
        let mut threads = store.load(&projects[0], &agents, 0, &mut next);
        store.load_blocks(&mut threads[0]);
        assert!(matches!(&threads[0].blocks[1], Block::Text(t) if t == "two and more"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn searches_what_was_said() {
        let dir = temp("search");
        let agents = AgentSpec::defaults();
        let mut store = Store::open_in(&dir).unwrap();
        let mut a = Thread::new(1, 0);
        a.blocks.push(Block::User(Message::now("Where is the Lighthouse?")));
        a.blocks.push(Block::Thought("lighthouse, privately".into()));
        a.blocks.push(Block::Text("Past the lighthouse.".into()));
        let mut b = Thread::new(2, 0);
        b.blocks.push(Block::Text("nothing here".into()));
        let mut project = Project::chats();
        project.threads.extend([a, b]);
        let mut projects = [project];
        save(&mut store, &mut projects, &agents);
        let id = projects[0].threads[0].stored.unwrap();
        let db = store.path().to_path_buf();
        let found = |q: &str| search(&db, "", q).unwrap().into_iter().map(|(t, seq, _)| (t, seq)).collect::<Vec<_>>();
        assert_eq!(found("LIGHTHOUSE"), [(id, 0), (id, 2)], "ignoring case, leaving out thoughts");
        assert_eq!(found("ou"), [(id, 0), (id, 2)], "shorter than a trigram still finds");
        assert!(found("\"quoted\" 100%_").is_empty());
        assert!(search(store.path(), "/elsewhere", "lighthouse").unwrap().is_empty(), "only the project's threads");

        // A thread gone from the list goes from the index too.
        projects[0].threads.remove(0);
        save(&mut store, &mut projects, &agents);
        assert!(found("lighthouse").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_thread_files_are_read_in_once() {
        let dir = temp("import");
        let agents = AgentSpec::defaults();
        std::fs::create_dir_all(dir.join("threads")).unwrap();
        let old = serde_json::json!({
            "path": null, "saved_at": 50_000,
            "threads": [{
                "title": "Old", "agent": "Codex", "state": "Idle", "created_at": 1_000, "active_at": 7_000,
                "blocks": [
                    {"User": {"text": "hi there", "at": 1_000}},
                    {"TurnEnded": {"at": 7_000, "worked_ms": 6_000}},
                ],
            }],
        });
        std::fs::write(dir.join("threads/chats.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        std::fs::write(dir.join("threads/broken.json"), b"{ not json").unwrap();

        let store = Store::open_in(&dir).unwrap();
        let mut next = 0;
        let mut threads = store.load(&Project::chats(), &agents, 0, &mut next);
        assert_eq!(threads.len(), 1);
        assert_eq!((threads[0].title.as_ref(), threads[0].agent, threads[0].active_at), ("Old", 1, Some(7_000)));
        store.load_blocks(&mut threads[0]);
        assert_eq!(threads[0].blocks.len(), 2);
        assert_eq!(search(store.path(), "", "there").unwrap().len(), 1, "imported text is searchable");
        assert!(dir.join("threads/chats.json.old").exists() && !dir.join("threads/chats.json").exists());
        assert!(dir.join("threads/broken.json").exists(), "a file that can't be read is left alone");
        drop(store);

        // Opening again doesn't read anything in twice.
        let store = Store::open_in(&dir).unwrap();
        assert_eq!(store.load(&Project::chats(), &agents, 0, &mut next).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
