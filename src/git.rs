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
    /// Every checkout of the repository, the main one first.
    pub worktrees: Vec<Worktree>,
    /// Changes put aside with `git stash`, newest first.
    pub stashes: Vec<Stash>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// Its place in the stash list: `stash@{index}`.
    pub index: usize,
    /// What it was saved as: the message given, or the commit it was made on.
    pub message: String,
    /// The branch it was made on, when Git recorded one.
    pub branch: Option<String>,
    /// How long ago, as Git says it: "2 hours ago".
    pub when: String,
}
/// `stash list` records: ref, reflog subject and age, separated by unit separators.
fn parse_stashes(bytes: &[u8]) -> Vec<Stash> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\x1f');
            let (spec, subject, when) = (parts.next()?, parts.next()?, parts.next().unwrap_or(""));
            let index = spec.strip_prefix("stash@{")?.strip_suffix('}')?.parse().ok()?;
            // "WIP on main: 1a2b3c4 subject" for a plain stash, "On main: message" for a named one.
            let (head, rest) = subject.split_once(": ").unwrap_or(("", subject));
            let branch = head.strip_prefix("WIP on ").or_else(|| head.strip_prefix("On ")).map(str::to_owned);
            let message = if head.starts_with("WIP on ") {
                // Drop the abbreviated hash in front of the commit's subject.
                rest.split_once(' ').map_or(rest, |(_, s)| s).to_owned()
            } else {
                rest.to_owned()
            };
            Some(Stash { index, message, branch, when: when.to_owned() })
        })
        .collect()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The branch checked out, or `None` when detached.
    pub branch: Option<String>,
}
/// `worktree list --porcelain`: a record per checkout, separated by blank lines.
fn parse_worktrees(bytes: &[u8]) -> Vec<Worktree> {
    let text = String::from_utf8_lossy(bytes);
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(Worktree { path: PathBuf::from(p), branch: None });
        } else if let (Some(b), Some(last)) = (line.strip_prefix("branch "), out.last_mut()) {
            last.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_owned());
        } else if line == "bare" {
            // A bare repository has nothing checked out to show.
            out.pop();
        }
    }
    out
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
    /// A picture GPUI can draw, each side as it was: `None` where the file
    /// didn't exist.
    Image { old: Option<std::sync::Arc<gpui::Image>>, new: Option<std::sync::Arc<gpui::Image>> },
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
    /// Put changes aside: everything, untracked files included, or only what is staged.
    Stash { staged_only: bool },
    /// Bring a stash's changes back and remove it from the list.
    StashPop(usize),
    /// Bring a stash's changes back and keep it.
    StashApply(usize),
    /// Throw a stash away.
    StashDrop(usize),
}

fn command(root: &Path) -> Command {
    let mut cmd = stash_command(root);
    cmd.arg("--literal-pathspecs");
    cmd
}
/// `git stash` takes no paths here, and with `--literal-pathspecs` it leaves
/// untracked files behind, so it runs without.
fn stash_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    // Hooks (husky's pre-commit, pre-push) run tools like pnpm that live on the
    // shell's PATH, not the bare one an app from the Dock starts with.
    if let Some(path) = crate::agent::shell_path() {
        cmd.env("PATH", path);
    }
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
    let worktrees = run(&root, &["worktree", "list", "--porcelain"]).map(|out| parse_worktrees(&out)).unwrap_or_default();
    let stashes = run(&root, &["stash", "list", "--format=%gd%x1f%gs%x1f%cr"]).map(|out| parse_stashes(&out)).unwrap_or_default();
    Ok(Snapshot { root, git_dir, branch, entries, upstream, ahead, last_commit, worktrees, stashes })
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

/// Images past this size aren't shown side by side.
const MAX_IMAGE: usize = 32 * 1024 * 1024;

/// The format of an image file GPUI can draw, by its extension. SVGs stay
/// text, so their change reads as a diff of the markup.
fn image_format(path: &Path) -> Option<gpui::ImageFormat> {
    use gpui::ImageFormat::*;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => Png,
        "jpg" | "jpeg" => Jpeg,
        "gif" => Gif,
        "webp" => Webp,
        "bmp" => Bmp,
        "tif" | "tiff" => Tiff,
        _ => return None,
    })
}

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
    if let Some(format) = image_format(&entry.path) {
        let side = |bytes: Option<Vec<u8>>| bytes.filter(|b| !b.is_empty()).map(|b| std::sync::Arc::new(gpui::Image::from_bytes(format, b)));
        let (old, new) = (side(old), side(new));
        if old.iter().chain(&new).any(|i| i.bytes.len() > MAX_IMAGE) {
            return Ok(Diff::Large);
        }
        return Ok(Diff::Image { old, new });
    }
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
/// How a line of the working file differs from HEAD, for the file view's gutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were removed just above this one.
    RemovedAbove,
    /// Lines were removed just below this one (the file's end).
    RemovedBelow,
}

/// The commit a run of lines last changed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Empty for lines not committed yet.
    pub sha: String,
    pub author: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub summary: String,
}
impl Commit {
    pub fn committed(&self) -> bool {
        !self.sha.is_empty()
    }
}

/// What Git knows about an open file: each line's change since HEAD and the
/// commit that last touched it, by line.
#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub marks: Vec<Option<Mark>>,
    pub commits: Vec<Commit>,
    /// Each line's commit, as an index into `commits`.
    pub blame: Vec<Option<usize>>,
}

/// Per-line marks for `new` against `old`, as an editor gutter shows them.
pub fn line_marks(old: &str, new: &str) -> Vec<Option<Mark>> {
    use similar::DiffOp;
    let count = new.lines().count();
    let mut marks = vec![None; count];
    for op in similar::TextDiff::from_lines(old, new).ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                marks[new_index..(new_index + new_len).min(count)].fill(Some(Mark::Added));
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                marks[new_index..(new_index + new_len).min(count)].fill(Some(Mark::Modified));
            }
            DiffOp::Delete { new_index, .. } => {
                if new_index < count {
                    marks[new_index].get_or_insert(Mark::RemovedAbove);
                } else if let Some(last) = marks.last_mut() {
                    last.get_or_insert(Mark::RemovedBelow);
                }
            }
        }
    }
    marks
}

/// `blame --porcelain`: a header per line, commit details the first time a
/// commit appears, then the line itself after a tab.
fn parse_blame(bytes: &[u8]) -> (Vec<Commit>, Vec<Option<usize>>) {
    let text = String::from_utf8_lossy(bytes);
    let mut commits: Vec<Commit> = Vec::new();
    let mut blame = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.lines() {
        if line.starts_with('\t') {
            blame.push(current);
            continue;
        }
        let mut words = line.splitn(2, ' ');
        let (key, value) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
        if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            let sha = if key.bytes().all(|b| b == b'0') { String::new() } else { key.to_owned() };
            current = Some(commits.iter().position(|c| c.sha == sha).unwrap_or_else(|| {
                commits.push(Commit { sha, author: String::new(), time: 0, summary: String::new() });
                commits.len() - 1
            }));
            continue;
        }
        let Some(commit) = current.map(|i| &mut commits[i]) else { continue };
        match key {
            "author" => commit.author = value.to_owned(),
            "author-time" => commit.time = value.parse().unwrap_or(0),
            "summary" => commit.summary = value.to_owned(),
            _ => {}
        }
    }
    (commits, blame)
}

/// Gutter marks and blame for a file in a working tree; `None` outside one or
/// for an ignored file.
pub fn file_info(file: &Path) -> Option<FileInfo> {
    // Git reports the top level with symlinks resolved.
    let file = &std::fs::canonicalize(file).ok()?;
    let folder = file.parent()?;
    let top = run(folder, &["rev-parse", "--show-toplevel"]).ok()?;
    let root = path(top.trim_ascii_end());
    let relative = file.strip_prefix(&root).ok()?;
    let text = std::fs::read(file).ok().filter(|b| b.len() <= MAX_DIFF && !b.contains(&0))?;
    let text = String::from_utf8(text).ok()?;
    let head = blob(&root, relative, "HEAD:").and_then(|b| String::from_utf8(b).ok());
    if head.is_none() && checked(command(&root).args(["check-ignore", "-q", "--"]).arg(relative).output()).is_ok() {
        return None;
    }
    let marks = line_marks(head.as_deref().unwrap_or(""), &text);
    let (commits, blame) = head
        .is_some()
        .then(|| checked(command(&root).args(["blame", "--porcelain", "--"]).arg(relative).output()).ok())
        .flatten()
        .map(|out| parse_blame(&out))
        .unwrap_or_default();
    Some(FileInfo { marks, commits, blame })
}

/// Blame for `text` as if it were `file`'s contents, which may differ from
/// what's on disk: a diff's old or new side. Lines HEAD doesn't have are
/// uncommitted. `None` outside a repository or for a file HEAD doesn't have.
pub fn blame_text(file: &Path, text: &str) -> Option<FileInfo> {
    use std::io::Write;
    // The file may be gone, as one a diff deletes, but its folder usually isn't.
    let folder = std::fs::canonicalize(file.parent()?).ok()?;
    let top = run(&folder, &["rev-parse", "--show-toplevel"]).ok()?;
    let root = path(top.trim_ascii_end());
    let relative = folder.join(file.file_name()?);
    let relative = relative.strip_prefix(&root).ok()?;
    let mut child = command(&root)
        .args(["blame", "--porcelain", "--contents", "-", "--"])
        .arg(relative)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let text = text.to_owned();
    // Written alongside the read, so a large file can't fill both pipes.
    let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let out = checked(child.wait_with_output()).ok()?;
    let _ = writer.join();
    let (commits, blame) = parse_blame(&out);
    Some(FileInfo { marks: Vec::new(), commits, blame })
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
        Operation::Stash { staged_only } => {
            let mode = if staged_only { "--staged" } else { "--include-untracked" };
            checked(stash_command(root).args(["stash", "push", "-q", mode]).output())?;
        }
        // `--index` puts back what was staged as staged, as it was when stashed.
        Operation::StashPop(index) => {
            checked(stash_command(root).args(["stash", "pop", "-q", "--index"]).arg(format!("stash@{{{index}}}")).output())?;
        }
        Operation::StashApply(index) => {
            checked(stash_command(root).args(["stash", "apply", "-q", "--index"]).arg(format!("stash@{{{index}}}")).output())?;
        }
        Operation::StashDrop(index) => {
            checked(stash_command(root).args(["stash", "drop", "-q"]).arg(format!("stash@{{{index}}}")).output())?;
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

// ── Links to the remote ─────────────────────────────────────────────────────

/// A repository's web home, from a remote URL in any of git's spellings:
/// `git@host:owner/repo.git`, `ssh://git@host:22/owner/repo`, `https://user@host/owner/repo.git`.
fn web_base(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (host, path) = if let Some((_, rest)) = remote.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        // An ssh port isn't the web one.
        let host = if remote.starts_with("http") { host } else { host.split(':').next()? };
        (host, path)
    } else {
        // scp-like: [user@]host:path
        let (authority, path) = remote.split_once(':')?;
        (authority.rsplit_once('@').map_or(authority, |(_, h)| h), path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    // GitHub's ssh-over-https host serves the same repositories.
    let host = if host == "ssh.github.com" { "github.com" } else { host };
    Some(format!("https://{host}/{path}"))
}

/// Percent-encodes what can't sit in a URL path as is, keeping `/`.
fn encode_path(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The page showing `path` at `reference`, its lines (1-based) marked as each
/// host spells them.
fn web_url(base: &str, reference: &str, commit: bool, path: &str, lines: Option<(usize, usize)>) -> String {
    let host = base.trim_start_matches("https://").split('/').next().unwrap_or_default();
    let (reference, path) = (encode_path(reference), encode_path(path));
    let range = |first: usize, last: usize, sep: &str, again: &str| {
        if first == last { format!("{first}") } else { format!("{first}{sep}{again}{last}") }
    };
    if host.contains("gitlab") {
        let hash = lines.map(|(a, b)| format!("#L{}", range(a, b, "-", ""))).unwrap_or_default();
        format!("{base}/-/blob/{reference}/{path}{hash}")
    } else if host.contains("bitbucket") {
        let hash = lines.map(|(a, b)| format!("#lines-{}", range(a, b, ":", ""))).unwrap_or_default();
        format!("{base}/src/{reference}/{path}{hash}")
    } else if host.contains("codeberg") || host.contains("gitea") || host.contains("forgejo") {
        let kind = if commit { "commit" } else { "branch" };
        let hash = lines.map(|(a, b)| format!("#L{}", range(a, b, "-", "L"))).unwrap_or_default();
        format!("{base}/src/{kind}/{reference}/{path}{hash}")
    } else {
        // GitHub, and the many hosts that copy its layout.
        let hash = lines.map(|(a, b)| format!("#L{}", range(a, b, "-", "L"))).unwrap_or_default();
        format!("{base}/blob/{reference}/{path}{hash}")
    }
}

/// A link to `file` (and `lines`, 1-based) on its repository's remote. It
/// names the branch the remote tracks for HEAD, or — as a `permalink`, or when
/// the branch isn't on the remote — the commit HEAD is at.
pub fn remote_link(file: &Path, lines: Option<(usize, usize)>, permalink: bool) -> Result<String, String> {
    let dir = if file.is_dir() { file } else { file.parent().ok_or("No folder.")? };
    let text = |root: &Path, args: &[&str]| run(root, args).map(|out| String::from_utf8_lossy(&out).trim().to_owned());
    let root = PathBuf::from(text(dir, &["rev-parse", "--show-toplevel"]).map_err(|_| "Not in a Git repository.")?);
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let relative = canonical(file)
        .strip_prefix(canonical(&root))
        .map_err(|_| "Outside the repository.")?
        .to_string_lossy()
        .into_owned();
    // `origin/feature/x`: the remote, then its name for the branch.
    let upstream = text(&root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).ok().filter(|u| !u.is_empty());
    let remotes = text(&root, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes.lines().collect();
    let tracked = upstream.as_deref().and_then(|u| {
        let remote = remotes.iter().filter(|r| u.starts_with(&format!("{r}/"))).max_by_key(|r| r.len())?;
        Some((remote.to_string(), u[remote.len() + 1..].to_string()))
    });
    let remote = match &tracked {
        Some((remote, _)) => remote.clone(),
        None if remotes.contains(&"origin") => "origin".into(),
        None => remotes.first().ok_or("This repository has no remote.")?.to_string(),
    };
    let url = text(&root, &["remote", "get-url", &remote])?;
    let base = web_base(&url).ok_or_else(|| format!("Can't make a web link from {url}"))?;
    let (reference, commit) = match tracked {
        Some((_, branch)) if !permalink => (branch, false),
        _ => (text(&root, &["rev-parse", "HEAD"]).map_err(|_| "Nothing has been committed yet.")?, true),
    };
    Ok(web_url(&base, &reference, commit, &relative, lines))
}

#[cfg(test)]
mod tests {
    #[test]
    fn web_links_from_remote_urls() {
        for url in [
            "git@github.com:blackmann/chat.git",
            "ssh://git@ssh.github.com:443/blackmann/chat.git",
            "https://token@github.com/blackmann/chat",
            "https://github.com/blackmann/chat.git/",
        ] {
            assert_eq!(web_base(url).as_deref(), Some("https://github.com/blackmann/chat"), "{url}");
        }
        assert_eq!(web_base("https://gitlab.example.com:8443/group/sub/repo.git").as_deref(), Some("https://gitlab.example.com:8443/group/sub/repo"));
        assert_eq!(web_base("/local/path"), None);
        let gh = "https://github.com/o/r";
        assert_eq!(web_url(gh, "main", false, "src/a b.rs", Some((3, 7))), "https://github.com/o/r/blob/main/src/a%20b.rs#L3-L7");
        assert_eq!(web_url(gh, "feat/x", false, "a.rs", Some((3, 3))), "https://github.com/o/r/blob/feat/x/a.rs#L3");
        assert_eq!(web_url("https://gitlab.com/o/r", "main", false, "a.rs", Some((3, 7))), "https://gitlab.com/o/r/-/blob/main/a.rs#L3-7");
        assert_eq!(web_url("https://bitbucket.org/o/r", "abc", true, "a.rs", Some((3, 7))), "https://bitbucket.org/o/r/src/abc/a.rs#lines-3:7");
        assert_eq!(web_url("https://codeberg.org/o/r", "abc", true, "a.rs", None), "https://codeberg.org/o/r/src/commit/abc/a.rs");
    }

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
    fn image_changes_keep_both_pictures() {
        let r = Repo::new();
        // The bytes only need to be told apart: the view decodes them later.
        std::fs::write(r.0.join("logo.png"), b"\x89PNG\0before").unwrap();
        r.stage("logo.png");
        r.commit();
        std::fs::write(r.0.join("logo.png"), b"\x89PNG\0after").unwrap();
        std::fs::write(r.0.join("new.webp"), b"RIFF\0new").unwrap();
        let s = snapshot(&r.0).unwrap();
        let sides = |name: &str| {
            let e = s.entries.iter().find(|e| e.path == Path::new(name)).unwrap();
            match diff(&r.0, e, false).unwrap() {
                Diff::Image { old, new } => (old.map(|i| i.bytes.clone()), new.map(|i| (i.format, i.bytes.clone()))),
                other => panic!("expected an image, got {other:?}"),
            }
        };
        assert_eq!(sides("logo.png"), (Some(b"\x89PNG\0before".to_vec()), Some((gpui::ImageFormat::Png, b"\x89PNG\0after".to_vec()))));
        assert_eq!(sides("new.webp"), (None, Some((gpui::ImageFormat::Webp, b"RIFF\0new".to_vec()))));
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
    fn lists_worktrees_main_first() {
        let list = parse_worktrees(b"worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /r-side\nHEAD def\ndetached\n\n");
        assert_eq!(list, [Worktree { path: "/r".into(), branch: Some("main".into()) }, Worktree { path: "/r-side".into(), branch: None }]);
        let r = Repo::new();
        r.write("a", "x\n");
        r.stage("a");
        r.commit();
        run(&r.0, &["worktree", "add", "-q", "-b", "side", "side-tree"]).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert_eq!(s.worktrees.len(), 2);
        assert_eq!(s.worktrees[1].branch.as_deref(), Some("side"));
    }
    #[test]
    fn marks_added_modified_and_removed_lines() {
        let marks = line_marks("a\nb\nc\nd\n", "a\nB\nc\nnew\n");
        assert_eq!(marks, [None, Some(Mark::Modified), None, Some(Mark::Modified)]);
        assert_eq!(line_marks("a\nb\nc\n", "a\nc\n"), [None, Some(Mark::RemovedAbove)]);
        assert_eq!(line_marks("a\nb\n", "a\n"), [Some(Mark::RemovedBelow)]);
        assert_eq!(line_marks("a\n", "a\nb\n"), [None, Some(Mark::Added)]);
    }
    #[test]
    fn file_info_blames_committed_and_uncommitted_lines() {
        let r = Repo::new();
        r.write("a", "one\ntwo\n");
        r.stage("a");
        r.commit();
        r.write("a", "one\nTWO\nthree\n");
        let info = file_info(&r.0.join("a")).unwrap();
        assert_eq!(info.marks, [None, Some(Mark::Modified), Some(Mark::Modified)]);
        let commit = |n: usize| &info.commits[info.blame[n].unwrap()];
        assert!(commit(0).committed());
        assert_eq!((commit(0).author.as_str(), commit(0).summary.as_str()), ("Test", "test"));
        assert!(!commit(1).committed() && !commit(2).committed());

        r.write("new", "x\n");
        let info = file_info(&r.0.join("new")).unwrap();
        assert_eq!(info.marks, [Some(Mark::Added)]);
        assert!(info.blame.is_empty());
    }
    #[test]
    fn blame_text_blames_given_contents() {
        let r = Repo::new();
        r.write("a", "one\ntwo\n");
        r.stage("a");
        r.commit();
        // Neither the file on disk nor HEAD: a diff's side, with a line of its own.
        r.write("a", "something else\n");
        let info = blame_text(&r.0.join("a"), "one\nnew\ntwo\n").unwrap();
        let commit = |n: usize| &info.commits[info.blame[n].unwrap()];
        assert_eq!(info.blame.len(), 3);
        assert!(commit(0).committed() && commit(2).committed());
        assert!(!commit(1).committed());
        // A deleted file's old side still blames, from its folder.
        std::fs::remove_file(r.0.join("a")).unwrap();
        assert!(blame_text(&r.0.join("a"), "one\ntwo\n").unwrap().commits[0].committed());
    }
    #[test]
    fn status_reads_upstream_and_ahead() {
        let status = parse_status(b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -0\0");
        assert_eq!((status.upstream.as_deref(), status.ahead), (Some("origin/main"), 2));
    }
    #[test]
    fn parses_stash_list() {
        let out = b"stash@{0}\x1fOn main: half done\x1f2 minutes ago\nstash@{1}\x1fWIP on feature/x: 1a2b3c4 Fix it\x1f3 days ago\n";
        let stashes = parse_stashes(out);
        assert_eq!(stashes.len(), 2);
        assert_eq!((stashes[0].index, stashes[0].message.as_str(), stashes[0].branch.as_deref()), (0, "half done", Some("main")));
        assert_eq!((stashes[1].index, stashes[1].message.as_str(), stashes[1].branch.as_deref()), (1, "Fix it", Some("feature/x")));
        assert_eq!(stashes[1].when, "3 days ago");
    }
    #[test]
    fn stash_pop_apply_and_drop() {
        let r = Repo::new();
        r.write("a", "base\n");
        r.stage("a");
        r.commit();
        r.write("a", "staged\n");
        r.stage("a");
        r.write("new", "untracked\n");
        operate(&r.0, Operation::Stash { staged_only: false }).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert!(s.entries.is_empty(), "everything, untracked files too, is put aside");
        assert_eq!(s.stashes.len(), 1);
        operate(&r.0, Operation::StashApply(0)).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert_eq!(s.stashes.len(), 1, "apply keeps the stash");
        assert!(s.entries.iter().any(|e| e.path == Path::new("a") && e.staged()), "staged changes come back staged");
        assert!(s.entries.iter().any(|e| e.untracked()));
        // A different change, or Git, within the same second, makes the same stash commit and adds no entry.
        r.write("a", "second\n");
        operate(&r.0, Operation::Stash { staged_only: false }).unwrap();
        assert_eq!(snapshot(&r.0).unwrap().stashes.len(), 2);
        operate(&r.0, Operation::StashDrop(1)).unwrap();
        operate(&r.0, Operation::StashPop(0)).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert!(s.stashes.is_empty() && !s.entries.is_empty());
    }
    #[test]
    fn stash_staged_only_leaves_the_rest() {
        let r = Repo::new();
        r.write("a", "base\n");
        r.write("b", "base\n");
        r.stage("a");
        r.stage("b");
        r.commit();
        r.write("a", "staged\n");
        r.stage("a");
        r.write("b", "unstaged\n");
        operate(&r.0, Operation::Stash { staged_only: true }).unwrap();
        let s = snapshot(&r.0).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].path, Path::new("b"));
        assert_eq!(std::fs::read_to_string(r.0.join("a")).unwrap(), "base\n");
    }
}