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
}
impl Entry {
    pub fn conflict(&self) -> bool {
        self.index == b'U'
            || self.worktree == b'U'
            || matches!((self.index, self.worktree), (b'A', b'A') | (b'D', b'D'))
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
    pub branch: String,
    pub entries: Vec<Entry>,
}
#[derive(Clone, Debug)]
pub struct Diff {
    pub patch: String,
}
#[derive(Clone)]
pub enum Operation {
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    Commit(String),
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
fn parse_status(bytes: &[u8]) -> Vec<Entry> {
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut entries = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let original = if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
            records.next().map(path)
        } else {
            None
        };
        entries.push(Entry {
            path: path(&record[3..]),
            original,
            index: record[0],
            worktree: record[1],
        });
    }
    entries
}
pub fn snapshot(folder: &Path) -> Result<Snapshot, String> {
    let root = run(folder, &["rev-parse", "--show-toplevel"])?;
    let root = path(root.strip_suffix(b"\n").unwrap_or(&root));
    let branch = run(&root, &["symbolic-ref", "--short", "HEAD"])
        .or_else(|_| run(&root, &["rev-parse", "--short", "HEAD"]))?;
    let entries = parse_status(&run(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?);
    Ok(Snapshot {
        root,
        branch: String::from_utf8_lossy(&branch).trim().into(),
        entries,
    })
}
pub fn diff(root: &Path, entry: &Entry, staged: bool) -> Result<Diff, String> {
    let mut cmd = command(root);
    if entry.index == b'?' {
        // --no-index handles binary files and symlinks, and returns 1 for a diff.
        cmd.args([
            "diff",
            "--no-index",
            "--no-color",
            "--no-ext-diff",
            "--",
            "/dev/null",
        ])
        .arg(root.join(&entry.path));
        let out = cmd.output().map_err(|e| e.to_string())?;
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(String::from_utf8_lossy(&out.stderr).into());
        }
        return Ok(Diff {
            patch: String::from_utf8_lossy(&out.stdout).into(),
        });
    }
    cmd.args(["diff", "--no-color", "--no-ext-diff", "--no-textconv"]);
    if staged {
        cmd.arg("--cached");
    }
    cmd.arg("--").args(entry.paths());
    Ok(Diff {
        patch: String::from_utf8_lossy(&checked(cmd.output())?).into(),
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
        Operation::Commit(message) => {
            if message.trim().is_empty() {
                return Err("Enter a commit message.".into());
            }
            checked(
                command(root)
                    .arg("commit")
                    .arg("-m")
                    .arg(OsString::from(message))
                    .output(),
            )?;
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
            let root = std::env::temp_dir().join(format!(
                "chat-git-{}-{}",
                std::process::id(),
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
        assert!(diff(&r.0, e, true).unwrap().patch.contains("+index"));
        assert!(diff(&r.0, e, false).unwrap().patch.contains("+worktree"));
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
        assert!(
            diff(&r.0, &s.entries[0], false)
                .unwrap()
                .patch
                .contains("-base")
        );
        std::fs::write(r.0.join("binary"), [0, 1, 2]).unwrap();
        let s = snapshot(&r.0).unwrap();
        let e = s
            .entries
            .iter()
            .find(|e| e.path == Path::new("binary"))
            .unwrap();
        assert!(diff(&r.0, e, false).unwrap().patch.contains("Binary files"));
    }
}
