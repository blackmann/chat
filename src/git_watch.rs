//! Tells the Changes page when `git status` may have changed: a file edited in
//! a project folder, or the index, HEAD or refs moved by staging, committing or
//! checking out from anywhere. Events in ignored folders (`target/`,
//! `node_modules/`…) and in git's object store are dropped before they reach the UI.
use futures::channel::mpsc;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    ignore: Gitignore,
}
impl Repo {
    fn new(root: &Path, git_dir: &Path) -> Self {
        let (root, git_dir) = (real(root), real(git_dir));
        let mut repo = Self { ignore: Gitignore::empty(), root, git_dir };
        repo.reload();
        repo
    }
    /// The top-level `.gitignore` and `info/exclude`. Nested and global ignore
    /// files aren't read: what they hide only costs a refresh that finds nothing.
    fn reload(&mut self) {
        let mut builder = GitignoreBuilder::new(&self.root);
        builder.add(self.root.join(".gitignore"));
        builder.add(self.git_dir.join("info/exclude"));
        self.ignore = builder.build().unwrap_or_else(|_| Gitignore::empty());
    }
}
/// FSEvents reports resolved paths (`/private/var`, not `/var`).
fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a change at `path` can show in `git status`.
fn relevant(repos: &mut [Repo], path: &Path) -> bool {
    if let Some(repo) = repos.iter_mut().find(|r| path.starts_with(&r.git_dir)) {
        let inner = path.strip_prefix(&repo.git_dir).unwrap_or(path);
        if inner == Path::new("info/exclude") {
            repo.reload();
            return true;
        }
        // Objects are written before the index or a ref moves, and the move
        // itself is a rename of a `.lock` file onto the real one.
        let first = inner.components().next().and_then(|c| c.as_os_str().to_str());
        return !matches!(first, Some("objects" | "logs" | "hooks" | "lfs" | "fsmonitor--daemon"))
            && path.extension().is_none_or(|e| e != "lock");
    }
    // The innermost repository holding the path: a nested one shadows its parent.
    let Some(repo) = repos.iter_mut().filter(|r| path.starts_with(&r.root)).max_by_key(|r| r.root.as_os_str().len()) else {
        return false;
    };
    if path.file_name().is_some_and(|n| n == ".gitignore") {
        if path.parent() == Some(repo.root.as_path()) {
            repo.reload();
        }
        return true;
    }
    !repo.ignore.matched_path_or_any_parents(path, path.is_dir()).is_ignore()
}

pub struct Watcher {
    watcher: notify::RecommendedWatcher,
    watched: Vec<PathBuf>,
    /// What `set` was last given, so an unchanged project costs nothing.
    wanted: (Vec<PathBuf>, Vec<(PathBuf, PathBuf)>),
    repos: Arc<Mutex<Vec<Repo>>>,
}
impl Watcher {
    /// The receiver gets a message for every batch of events that matters;
    /// many may queue up during a build or checkout.
    pub fn new() -> notify::Result<(Self, mpsc::UnboundedReceiver<()>)> {
        let (changes, received) = mpsc::unbounded();
        let repos: Arc<Mutex<Vec<Repo>>> = Arc::default();
        let shared = repos.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let matters = match event {
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
                Ok(event) => {
                    let mut repos = shared.lock().unwrap_or_else(|e| e.into_inner());
                    event.paths.iter().any(|p| relevant(&mut repos, p))
                }
                // Dropped events: look again rather than miss something.
                Err(_) => true,
            };
            if matters {
                let _ = changes.unbounded_send(());
            }
        })?;
        Ok((Self { watcher, watched: Vec::new(), wanted: Default::default(), repos }, received))
    }
    /// Watch these folders, and each repository's git dir where it lies outside
    /// them (a folder inside a larger repository, or a worktree). `repos` pairs
    /// each repository's root with its git dir.
    pub fn set(&mut self, folders: Vec<PathBuf>, repos: Vec<(PathBuf, PathBuf)>) {
        if self.wanted == (folders.clone(), repos.clone()) {
            return;
        }
        let mut wanted: Vec<PathBuf> = folders.iter().map(|f| real(f)).collect();
        wanted.extend(repos.iter().map(|(_, git_dir)| real(git_dir)));
        // A folder inside another is already watched with it.
        let all = wanted.clone();
        wanted.retain(|w| !all.iter().any(|o| o != w && w.starts_with(o)));
        wanted.sort();
        wanted.dedup();
        for old in self.watched.iter().filter(|w| !wanted.contains(w)) {
            let _ = self.watcher.unwatch(old);
        }
        let mut watched = Vec::new();
        for path in wanted {
            if self.watched.contains(&path) || self.watcher.watch(&path, RecursiveMode::Recursive).is_ok() {
                watched.push(path);
            }
        }
        self.watched = watched;
        *self.repos.lock().unwrap_or_else(|e| e.into_inner()) = repos.iter().map(|(root, git_dir)| Repo::new(root, git_dir)).collect();
        self.wanted = (folders, repos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_ignored_and_internal_paths() {
        let root = std::env::temp_dir().join(format!("chat-watch-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        let mut repos = vec![Repo::new(&root, &root.join(".git"))];
        let root = real(&root);
        let at = |p: &str| root.join(p);
        assert!(relevant(&mut repos, &at("src/main.rs")));
        assert!(relevant(&mut repos, &at(".gitignore")));
        assert!(relevant(&mut repos, &at(".git/index")));
        assert!(relevant(&mut repos, &at(".git/HEAD")));
        assert!(relevant(&mut repos, &at(".git/refs/heads/main")));
        assert!(!relevant(&mut repos, &at(".git/index.lock")));
        assert!(!relevant(&mut repos, &at(".git/objects/ab/cdef")));
        assert!(!relevant(&mut repos, &at("target/debug/chat")));
        assert!(!relevant(&mut repos, &at("build.log")));
        assert!(!relevant(&mut repos, Path::new("/elsewhere/file")));
        let _ = std::fs::remove_dir_all(&root);
    }
    /// Waits for a message, or `false` after a few seconds.
    fn changed(rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
        for _ in 0..50 {
            if rx.try_recv().is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }
    #[test]
    fn reports_edits_but_not_ignored_files() {
        let root = real(&std::env::temp_dir()).join(format!("chat-watch-live-{}", std::process::id()));
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::process::Command::new("git").arg("init").arg("-q").arg(&root).status().unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        let (mut watcher, mut rx) = Watcher::new().unwrap();
        watcher.set(vec![root.clone()], vec![(root.clone(), root.join(".git"))]);
        std::thread::sleep(std::time::Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(root.join("target/out"), "x").unwrap();
        assert!(!changed(&mut rx));
        std::fs::write(root.join("file"), "x").unwrap();
        assert!(changed(&mut rx));
        let _ = std::fs::remove_dir_all(&root);
    }
}
