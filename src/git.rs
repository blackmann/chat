//! Local Git operations. Paths come from NUL-delimited status, never shell text.
//! Workflow reference: Zed's git_ui/{git_panel,staged_diff,unstaged_diff}.rs.
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub original: Option<PathBuf>,
    pub index: u8,
    pub worktree: u8,
    /// Lines added and removed in the index (HEAD → index) and in the working
    /// tree (index → working tree); `None` for binaries or no change on that side.
    pub staged_stat: Option<(u32, u32)>,
    pub unstaged_stat: Option<(u32, u32)>,
}
impl Entry {
    pub fn conflict(&self) -> bool {
        self.index == b'U'
            || self.worktree == b'U'
            || matches!((self.index, self.worktree), (b'A', b'A') | (b'D', b'D'))
    }
    pub fn untracked(&self) -> bool {
        self.index == b'?'
    }
    /// The file's change on one side: the index's letter, or the working tree's.
    pub fn status(&self, staged: bool) -> u8 {
        if self.conflict() {
            b'U'
        } else if self.untracked() {
            b'?'
        } else if staged {
            self.index
        } else {
            self.worktree
        }
    }
    pub fn stat(&self, staged: bool) -> Option<(u32, u32)> {
        if staged { self.staged_stat } else { self.unstaged_stat }
    }
    pub fn staged(&self) -> bool {
        !self.conflict() && !matches!(self.index, b' ' | b'?')
    }
    pub fn unstaged(&self) -> bool {
        self.conflict() || self.worktree != b' '
    }
    /// A rename already recorded in the index has no old path left to add.
    pub fn stage_paths(&self) -> Vec<PathBuf> {
        if matches!(self.worktree, b'R' | b'C') {
            self.paths()
        } else {
            vec![self.path.clone()]
        }
    }
    pub fn paths(&self) -> Vec<PathBuf> {
        self.original
            .iter()
            .cloned()
            .chain(std::iter::once(self.path.clone()))
            .collect()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub root: PathBuf,
    /// Where the repository keeps its index and refs: `.git`, or elsewhere for a worktree.
    pub git_dir: PathBuf,
    pub branch: String,
    pub entries: Vec<Entry>,
    /// The branch's upstream, if it tracks one, and how many commits it is ahead of it.
    pub upstream: Option<String>,
    pub ahead: usize,
    /// The commit `HEAD` points at; `None` before the first commit.
    pub last_commit: Option<LastCommit>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastCommit {
    /// The whole message, to put back in the commit box when the commit is undone.
    pub message: String,
    /// Whether it has a parent; undoing a root commit empties the branch instead.
    pub has_parent: bool,
}
impl LastCommit {
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }
}
impl Snapshot {
    /// Whether the last commit is already on the upstream, where undoing it
    /// would rewrite history others may have.
    pub fn pushed(&self) -> bool {
        self.upstream.is_some() && self.ahead == 0
    }
    /// Whether there is anything to push: commits ahead of the upstream, or a
    /// branch with commits that has no upstream yet.
    pub fn can_push(&self) -> bool {
        self.last_commit.is_some() && (self.upstream.is_none() || self.ahead > 0)
    }
}
#[derive(Clone, Debug)]
pub enum Diff {
    /// Both sides in full, for the view to diff and colour: an absent side is empty.
    Text { old: String, new: String },
    Binary,
    /// Too large to show.
    Large,
}
#[derive(Clone)]
pub enum Operation {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    Commit(String),
    /// Push the branch, setting its upstream on the first remote if it has none.
    Push,
    /// Undo the last commit, keeping its changes staged.
    UndoCommit,
}

fn command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("--literal-pathspecs")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}
fn checked(output: std::io::Result<Output>) -> Result<Vec<u8>, String> {
    let output = output.map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}
fn run(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    checked(command(root).args(args).output())
}
#[cfg(unix)]
fn path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}
#[cfg(not(unix))]
fn path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
}
/// `status --porcelain=v2 --branch -z`: the branch headers, then one record per
/// changed file (a rename's record is followed by its old path).
struct Status {
    branch: String,
    entries: Vec<Entry>,
    upstream: Option<String>,
    ahead: usize,
}
fn parse_status(bytes: &[u8]) -> Status {
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    let (mut head, mut oid, mut upstream, mut ahead) = (None, None, None, 0);
    let mut entries = Vec::new();
    while let Some(record) = records.next() {
        // Fields before the path: ordinary 8, renamed or copied 9, unmerged 10.
        let fields = match record[0] {
            b'#' => {
                let text = String::from_utf8_lossy(record);
                if let Some(name) = text.strip_prefix("# branch.head ") {
                    head = Some(name.to_owned());
                } else if let Some(sha) = text.strip_prefix("# branch.oid ") {
                    oid = Some(sha.to_owned());
                } else if let Some(name) = text.strip_prefix("# branch.upstream ") {
                    upstream = Some(name.to_owned());
                } else if let Some(ab) = text.strip_prefix("# branch.ab +") {
                    ahead = ab.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
                }
                continue;
            }
            b'?' => {
                entries.push(Entry {
                    path: path(&record[2.min(record.len())..]),
                    original: None,
                    index: b'?',
                    worktree: b'?',
                    staged_stat: None,
                    unstaged_stat: None,
                });
                continue;
            }
            b'1' => 8,
            b'2' => 9,
            b'u' => 10,
            _ => continue,
        };
        let mut parts = record.splitn(fields + 1, |b| *b == b' ');
        let xy = parts.nth(1).unwrap_or_default();
        let Some(name) = parts.nth(fields - 2) else { continue };
        if xy.len() != 2 {
            continue;
        }
        // v2 marks an unchanged side with '.', v1 (and the rest of the app) with ' '.
        let letter = |b: u8| if b == b'.' { b' ' } else { b };
        entries.push(Entry {
            path: path(name),
            original: if record[0] == b'2' { records.next().map(path) } else { None },
            index: letter(xy[0]),
            worktree: letter(xy[1]),
            staged_stat: None,
            unstaged_stat: None,
        });
    }
    // Detached: the commit, shortened as `rev-parse --short` would.
    let branch = match head.as_deref() {
        Some("(detached)") | None => oid.map(|o| o.chars().take(7).collect()).unwrap_or_default(),
        Some(name) => name.to_owned(),
    };
    Status { branch, entries, upstream, ahead }
}
pub fn snapshot(folder: &Path) -> Result<Snapshot, String> {
    let dirs = run(folder, &["rev-parse", "--show-toplevel", "--absolute-git-dir"])?;
    let mut dirs = dirs.split(|b| *b == b'\n');
    let (Some(root), Some(git_dir)) = (dirs.next().map(path), dirs.next().map(path)) else {
        return Err("Not a working tree.".into());
    };
    let Status { branch, mut entries, upstream, ahead } = parse_status(&run(
        &root,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
    )?);
    let numstat = |cached: bool| {
        let mut args = vec!["diff", "--numstat", "-z", "--no-ext-diff", "--no-textconv"];
        if cached {
            args.push("--cached");
        }
        run(&root, &args).map(|out| parse_numstat(&out)).unwrap_or_default()
    };
    let (staged, unstaged) = (numstat(true), numstat(false));
    let find = |stats: &[(PathBuf, Option<(u32, u32)>)], path: &Path| {
        stats.iter().find(|(p, _)| p == path).and_then(|(_, stat)| *stat)
    };
    for entry in &mut entries {
        if entry.untracked() {
            entry.unstaged_stat = line_count(&root.join(&entry.path)).map(|n| (n, 0));
        } else {
            entry.staged_stat = find(&staged, &entry.path);
            entry.unstaged_stat = find(&unstaged, &entry.path);
        }
    }
    // Parents, then the message; fails before the first commit.
    let last_commit = run(&root, &["log", "-1", "--format=%P%x00%B"]).ok().and_then(|out| {
        let out = String::from_utf8_lossy(&out);
        let (parents, message) = out.split_once('\0')?;
        Some(LastCommit { message: message.trim_end().to_owned(), has_parent: !parents.trim().is_empty() })
    });
    Ok(Snapshot { root, git_dir, branch, entries, upstream, ahead, last_commit })
}
/// `--numstat -z` records: counts and a path, or counts, nothing, and two paths for a rename.
fn parse_numstat(bytes: &[u8]) -> Vec<(PathBuf, Option<(u32, u32)>)> {
    let mut fields = bytes.split(|b| *b == 0);
    let mut out = Vec::new();
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, |b| *b == b'\t');
        let (Some(adds), Some(dels), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let number = |b: &[u8]| std::str::from_utf8(b).ok()?.parse::<u32>().ok();
        let stat = number(adds).zip(number(dels));
        let name = if name.is_empty() {
            // A rename: the old path, then the new one.
            let _ = fields.next();
            match fields.next() {
                Some(new) => new,
                None => break,
            }
        } else {
            name
        };
        out.push((path(name), stat));
    }
    out
}

/// Lines in an untracked file, when it is small enough and text.
fn line_count(file: &Path) -> Option<u32> {
    if std::fs::metadata(file).ok()?.len() > 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(file).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let lines = bytes.iter().filter(|b| **b == b'\n').count() + usize::from(bytes.last().is_some_and(|b| *b != b'\n'));
    Some(lines as u32)
}

/// One side of a file's change: HEAD → index when `staged`, index → working tree otherwise.
/// Files past this size aren't diffed in the view.
const MAX_DIFF: usize = 2 * 1024 * 1024;

/// A blob from the object store, as `git show` prints it: `HEAD:path` or `:path`.
fn blob(root: &Path, spec: &Path, staged_side: &str) -> Option<Vec<u8>> {
    let mut arg = OsString::from(staged_side);
    arg.push(spec.as_os_str());
    command(root).arg("show").arg(arg).output().ok().filter(|o| o.status.success()).map(|o| o.stdout)
}

/// One side of a file's change: HEAD → index when `staged`, index → working tree
/// otherwise, as the two texts.
pub fn diff(root: &Path, entry: &Entry, staged: bool) -> Result<Diff, String> {
    // A rename's old side lives at its old path.
    let before_path = entry.original.as_deref().unwrap_or(&entry.path);
    let (old, new) = if staged {
        (blob(root, before_path, "HEAD:"), blob(root, &entry.path, ":"))
    } else if entry.untracked() {
        (None, std::fs::read(root.join(&entry.path)).ok())
    } else {
        let index = if matches!(entry.index, b'R' | b'C') { &entry.path } else { before_path };
        (blob(root, index, ":"), std::fs::read(root.join(&entry.path)).ok())
    };
    let (old, new) = (old.unwrap_or_default(), new.unwrap_or_default());
    if old.len() > MAX_DIFF || new.len() > MAX_DIFF {
        return Ok(Diff::Large);
    }
    let text = |bytes: Vec<u8>| (!bytes.contains(&0)).then(|| String::from_utf8(bytes).ok()).flatten();
    Ok(match (text(old), text(new)) {
        (Some(old), Some(new)) => Diff::Text { old, new },
        _ => Diff::Binary,
    })
}
pub fn operate(root: &Path, operation: Operation) -> Result<(), String> {
    match operation {
        Operation::Stage(paths) | Operation::Unstage(paths) if paths.is_empty() => return Ok(()),
        Operation::Stage(paths) => {
            checked(command(root).args(["add", "-A", "--"]).args(paths).output())?;
        }
        Operation::Unstage(paths) => {
            let has_head = run(root, &["rev-parse", "--verify", "HEAD"]).is_ok();
            let mut cmd = command(root);
            if has_head {
                cmd.args(["reset", "-q", "HEAD", "--"]);
            } else {
                cmd.args(["rm", "--cached", "-f", "-q", "--"]);
            }
            checked(cmd.args(paths).output())?;
        }
        Operation::Commit(message) if message.trim().is_empty() => {
            return Err("Enter a commit message.".into());
        }
        Operation::Commit(message) => {
            checked(command(root).arg("commit").arg("-m").arg(OsString::from(message)).output())?;
        }
        Operation::Push => {
            // Never wait on a credential prompt nobody can see.
            let push = || {
                let mut cmd = command(root);
                cmd.env("GIT_TERMINAL_PROMPT", "0").arg("push");
                cmd
            };
            if run(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok() {
                checked(push().output())?;
            } else {
                let remotes = run(root, &["remote"])?;
                let remotes = String::from_utf8_lossy(&remotes);
                let remote = if remotes.lines().any(|r| r == "origin") { "origin" } else { remotes.lines().next().ok_or("No remote to push to.")? };
                checked(push().args(["-u", remote, "HEAD"]).output())?;
            }
        }
        Operation::UndoCommit => {
            if run(root, &["rev-parse", "--verify", "-q", "HEAD~1"]).is_ok() {
                run(root, &["reset", "--soft", "-q", "HEAD~1"])?;
            } else {
                // The first commit: the branch goes back to having none, the index stays.
                run(root, &["update-ref", "-d", "HEAD"])?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            // The clock is too coarse to tell tests started together apart.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "chat-git-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            run(&root, &["init", "-q"]).unwrap();
            run(&root, &["config", "user.name", "Test"]).unwrap();
            run(&root, &["config", "user.email", "test@example.com"]).unwrap();
            run(&root, &["config", "commit.gpgsign", "false"]).unwrap();
            Self(root)
        }
        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.0.join(name), text).unwrap();
        }
        fn stage(&self, name: &str) {
            operate(&self.0, Operation::Stage(vec![name.into()])).unwrap();
        }
        fn commit(&self) {
            operate(&self.0, Operation::Commit("test".into())).unwrap();
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn partial_staging_and_commit_preserve_worktree() {
        let r = Repo::new();
        r.write("a", "base\n");
        r.stage("a");
        r.commit();
        r.write("a", "index\n");
        r.stage("a");
        r.write("a", "worktree\n");
        let s = snapshot(&r.0).unwrap();
        let e = &s.entries[0];
        assert!(e.staged() && e.unstaged());
        assert_eq!((e.staged_stat, e.unstaged_stat), (Some((1, 1)), Some((1, 1))));
        let sides = |staged| match diff(&r.0, e, staged).unwrap() {
            Diff::Text { old, new } => (old, new),
            _ => panic!("expected text"),
        };
        assert_eq!(sides(true), ("base\n".into(), "index\n".into()));
        assert_eq!(sides(false), ("index\n".into(), "worktree\n".into()));
        r.commit();
        assert_eq!(
            std::fs::read_to_string(r.0.join("a")).unwrap(),
            "worktree\n"
        );
        assert!(!snapshot(&r.0).unwrap().entries[0].staged());
    }
    #[test]
    fn unborn_unstage_literal_paths_and_rename() {
        let r = Repo::new();
        let name = ":(glob)* space\nfile";
        r.write(name, "hello\n");
        r.write("other", "untouched\n");
        r.stage(name);
        let s = snapshot(&r.0).unwrap();
        assert_eq!(s.entries.iter().filter(|e| e.staged()).count(), 1);
        r.write(name, "edited after staging\n");
        operate(&r.0, Operation::Unstage(vec![name.into()])).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.0.join(name)).unwrap(),
            "edited after staging\n"
        );
        assert!(snapshot(&r.0).unwrap().entries.iter().all(|e| !e.staged()));
        r.stage(name);
        r.commit();
        std::fs::rename(r.0.join(name), r.0.join("renamed")).unwrap();
        operate(&r.0, Operation::Stage(vec![name.into(), "renamed".into()])).unwrap();
        let s = snapshot(&r.0).unwrap();
        let e = s
            .entries
            .iter()
            .find(|e| e.path == Path::new("renamed"))
            .unwrap();
        assert_eq!(e.original.as_deref(), Some(Path::new(name)));
        r.write("renamed", "further edits\n");
        operate(&r.0, Operation::Stage(e.stage_paths())).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.0.join("renamed")).unwrap(),
            "further edits\n"
        );
        operate(&r.0, Operation::Unstage(e.paths())).unwrap();
        assert!(snapshot(&r.0).unwrap().entries.iter().all(|e| !e.staged()));
    }
    #[test]
    fn deleted_and_binary_diffs() {
        let r = Repo::new();
        r.write("a", "base\n");
        r.stage("a");
        r.commit();
        std::fs::remove_file(r.0.join("a")).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert!(matches!(diff(&r.0, &s.entries[0], false).unwrap(), Diff::Text { old, new } if old == "base\n" && new.is_empty()));
        std::fs::write(r.0.join("binary"), [0, 1, 2]).unwrap();
        let s = snapshot(&r.0).unwrap();
        let e = s
            .entries
            .iter()
            .find(|e| e.path == Path::new("binary"))
            .unwrap();
        assert!(matches!(diff(&r.0, e, false).unwrap(), Diff::Binary));
        assert_eq!(e.unstaged_stat, None);
    }
    #[test]
    fn stats_before_the_first_commit_and_for_untracked_files() {
        let r = Repo::new();
        r.write("staged", "one\ntwo\n");
        r.stage("staged");
        r.write("loose", "a\nb\nc");
        let s = snapshot(&r.0).unwrap();
        let entry = |name: &str| s.entries.iter().find(|e| e.path == Path::new(name)).unwrap();
        assert_eq!(entry("staged").staged_stat, Some((2, 0)));
        assert_eq!(entry("loose").unstaged_stat, Some((3, 0)));
    }
    #[test]
    fn branch_unborn_named_and_detached() {
        let r = Repo::new();
        run(&r.0, &["checkout", "-q", "-b", "topic"]).unwrap();
        assert_eq!(snapshot(&r.0).unwrap().branch, "topic");
        r.write("a", "base\n");
        r.stage("a");
        r.commit();
        assert_eq!(snapshot(&r.0).unwrap().branch, "topic");
        run(&r.0, &["checkout", "-q", "--detach"]).unwrap();
        let short = run(&r.0, &["rev-parse", "--short=7", "HEAD"]).unwrap();
        assert_eq!(snapshot(&r.0).unwrap().branch.as_bytes(), short.trim_ascii());
        let s = snapshot(&r.0).unwrap();
        assert!(s.git_dir.ends_with(".git"));
    }
    #[test]
    fn undo_commit_keeps_changes_staged() {
        let r = Repo::new();
        r.write("a", "one\n");
        r.stage("a");
        r.commit();
        assert_eq!(snapshot(&r.0).unwrap().last_commit.unwrap().has_parent, false);
        r.write("a", "two\n");
        r.stage("a");
        operate(&r.0, Operation::Commit("second\n\nbody".into())).unwrap();
        let last = snapshot(&r.0).unwrap().last_commit.unwrap();
        assert_eq!((last.subject(), last.has_parent), ("second", true));
        assert!(last.message.ends_with("body"));

        operate(&r.0, Operation::UndoCommit).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert_eq!(s.last_commit.unwrap().subject(), "test");
        assert!(s.entries[0].staged());

        // Undoing the first commit leaves the branch empty and its file staged.
        operate(&r.0, Operation::UndoCommit).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert!(s.last_commit.is_none());
        assert!(s.entries[0].staged());
    }
    #[test]
    fn status_reads_upstream_and_ahead() {
        let status = parse_status(b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -0\0");
        assert_eq!((status.upstream.as_deref(), status.ahead), (Some("origin/main"), 2));
    }
}
