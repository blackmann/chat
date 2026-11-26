//! Copying files dropped from Finder into a project's folders.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Copy `source` — a file, link or whole folder — into `dir`. A name already
/// taken there gets a number, as Finder does: "notes 2.md". Returns where it went.
pub fn copy_into(source: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = source.file_name().ok_or_else(|| io::Error::other("That has no name to copy under."))?;
    if dir.starts_with(source) {
        return Err(io::Error::other("A folder can't be copied into itself."));
    }
    let target = free_name(dir, Path::new(name));
    copy_tree(source, &target)?;
    Ok(target)
}

/// `name` in `dir`, or the first "name N.ext" there that isn't taken.
pub fn free_name(dir: &Path, name: &Path) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let stem = name.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .expect("some number is free")
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::set_permissions(to, meta.permissions())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chat-copy-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_a_file_and_numbers_a_taken_name() {
        let base = temp("file");
        let (from, into) = (base.join("from"), base.join("into"));
        fs::create_dir_all(&from).unwrap();
        fs::create_dir_all(&into).unwrap();
        fs::write(from.join("notes.md"), "hi").unwrap();
        assert_eq!(copy_into(&from.join("notes.md"), &into).unwrap(), into.join("notes.md"));
        assert_eq!(copy_into(&from.join("notes.md"), &into).unwrap(), into.join("notes 2.md"));
        assert_eq!(fs::read_to_string(into.join("notes 2.md")).unwrap(), "hi");
        // Dropped back where it came from: a copy beside it.
        assert_eq!(copy_into(&into.join("notes.md"), &into).unwrap(), into.join("notes 3.md"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copies_a_folder_with_what_is_inside() {
        let base = temp("dir");
        fs::create_dir_all(base.join("pics/raw")).unwrap();
        fs::write(base.join("pics/raw/a.png"), "a").unwrap();
        fs::create_dir_all(base.join("into")).unwrap();
        let to = copy_into(&base.join("pics"), &base.join("into")).unwrap();
        assert_eq!(fs::read_to_string(to.join("raw/a.png")).unwrap(), "a");
        assert!(copy_into(&base.join("pics"), &base.join("pics/raw")).is_err(), "not into itself");
        let _ = fs::remove_dir_all(&base);
    }
}
