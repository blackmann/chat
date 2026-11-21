//! The database's schema, built up by the SQL files in `migrations/` at the
//! top of the repository, as in Django or Prisma.
//!
//! - To change the schema, add a file named for when it was made (UTC) and
//!   what it does, such as `20261121143005_pin_threads.sql`;
//!   `scripts/new-migration.sh pin_threads` makes one. `build.rs` puts every
//!   file into the binary, oldest first.
//! - A file already released is never edited; a fix goes in a new file. Each
//!   applied file's checksum is kept, and a changed one is warned about.
//! - At startup the files not yet applied run in order, all in one
//!   transaction, so a database is either fully upgraded or left as it was.
//! - A database with a migration this build doesn't have was written by a
//!   newer build. It isn't touched, rather than being written over by an
//!   older idea of the schema.
//! - Work SQL can't do, such as reading in old files, goes in the `after` hook
//!   `run` takes, keyed by the migration's name, like Django's `RunPython`.
use rusqlite::{Connection, Transaction, params};

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

const TABLE: &str = "CREATE TABLE IF NOT EXISTS _migrations (
    name TEXT PRIMARY KEY,
    checksum TEXT NOT NULL,
    applied_at INTEGER NOT NULL
)";

/// A stable checksum of a migration's text (FNV-1a), the same across builds.
fn checksum(sql: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sql.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn fail(message: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(message.into())
}

/// Bring the database up to date with `migrations`, calling `after` with each
/// one's name once its SQL has run, in the same transaction. Says which ran.
pub fn run(
    conn: &mut Connection,
    migrations: &[(&'static str, &str)],
    mut after: impl FnMut(&str, &Transaction) -> rusqlite::Result<()>,
) -> rusqlite::Result<Vec<&'static str>> {
    let tx = conn.transaction()?;
    tx.execute_batch(TABLE)?;
    baseline(&tx, migrations)?;

    let applied: Vec<(String, String)> = {
        let mut query = tx.prepare("SELECT name, checksum FROM _migrations ORDER BY name")?;
        query.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    if let Some((name, _)) = applied.iter().find(|(name, _)| !migrations.iter().any(|(m, _)| m == name)) {
        return Err(fail(format!("it has migration {name}, which this build doesn't know: it was written by a newer build")));
    }

    let now = crate::model::now_ms();
    let mut ran = Vec::new();
    for &(name, sql) in migrations {
        match applied.iter().find(|(a, _)| a == name) {
            Some((_, sum)) if *sum != checksum(sql) => {
                eprintln!("chat: migration {name} has changed since it was applied; it isn't run again");
            }
            Some(_) => {}
            None => {
                tx.execute_batch(sql).map_err(|e| fail(format!("migration {name} failed: {e}")))?;
                after(name, &tx)?;
                tx.execute("INSERT INTO _migrations (name, checksum, applied_at) VALUES (?1, ?2, ?3)", params![name, checksum(sql), now])?;
                ran.push(name);
            }
        }
    }
    tx.commit()?;
    Ok(ran)
}

/// A database made before migrations were tracked already has the first
/// one's tables; it's recorded as applied rather than run again.
fn baseline(tx: &Transaction, migrations: &[(&str, &str)]) -> rusqlite::Result<()> {
    let tracked: i64 = tx.query_row("SELECT count(*) FROM _migrations", [], |r| r.get(0))?;
    let made: i64 = tx.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'threads'", [], |r| r.get(0))?;
    if let (0, 1, Some((name, sql))) = (tracked, made, migrations.first()) {
        tx.execute("INSERT INTO _migrations (name, checksum, applied_at) VALUES (?1, ?2, 0)", params![name, checksum(sql)])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: (&str, &str) = ("20260101000000_one", "CREATE TABLE threads (id INTEGER PRIMARY KEY);");
    const TWO: (&str, &str) = ("20260102000000_two", "ALTER TABLE threads ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;");

    fn columns(conn: &Connection) -> Vec<String> {
        let mut q = conn.prepare("SELECT name FROM pragma_table_info('threads')").unwrap();
        q.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn runs_what_is_new_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(run(&mut conn, &[ONE], |_, _| Ok(())).unwrap(), ["20260101000000_one"]);
        let mut seen = Vec::new();
        let ran = run(&mut conn, &[ONE, TWO], |name, _| Ok(seen.push(name.to_string()))).unwrap();
        assert_eq!((ran, seen), (vec!["20260102000000_two"], vec!["20260102000000_two".to_string()]));
        assert_eq!(columns(&conn), ["id", "pinned"]);
        assert!(run(&mut conn, &[ONE, TWO], |_, _| Ok(())).unwrap().is_empty());
    }

    #[test]
    fn a_failure_leaves_the_database_as_it_was() {
        let mut conn = Connection::open_in_memory().unwrap();
        let bad = ("20260102000000_bad", "ALTER TABLE threads ADD COLUMN a INTEGER; NOT SQL;");
        assert!(run(&mut conn, &[ONE, bad], |_, _| Ok(())).is_err());
        let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name = 'threads'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 0, "not even the first one stays");
    }

    #[test]
    fn a_newer_database_is_left_alone() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn, &[ONE, TWO], |_, _| Ok(())).unwrap();
        let err = run(&mut conn, &[ONE], |_, _| Ok(())).unwrap_err().to_string();
        assert!(err.contains("20260102000000_two"), "{err}");
    }

    #[test]
    fn an_untracked_database_counts_as_having_the_first() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(ONE.1).unwrap();
        assert_eq!(run(&mut conn, &[ONE, TWO], |_, _| Ok(())).unwrap(), ["20260102000000_two"]);
    }
}
