//! Project avatars from DiceBear (https://www.dicebear.com), fetched once and kept
//! beside the settings so the rail draws them offline. Both styles are CC0.
//! Each project's seed is its parent folder and folder name; nothing else of
//! the path is sent.
use crate::settings::Avatar;
use std::path::{Path, PathBuf};

const API: &str = "https://api.dicebear.com/10.x";

/// What DiceBear is given for a project: its parent folder and folder name, as
/// in `code/app`, so projects get avatars (and colours) of their own.
pub fn seed(path: &Path) -> String {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    match (path.parent().and_then(name), name(path)) {
        (Some(parent), Some(dir)) => format!("{parent}/{dir}"),
        (None, Some(dir)) => dir,
        _ => path.display().to_string(),
    }
}

/// A stable number for a project, from its seed: FNV-1a rather than
/// `DefaultHasher`, which may change between Rust releases.
pub fn hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed(path).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// `seed` made safe to put in a query string.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Where a project's avatar in `avatar`'s style is kept; `None` for initials.
pub fn file(avatar: Avatar, path: &Path) -> Option<PathBuf> {
    let style = avatar.style()?;
    Some(crate::settings::dir()?.join("avatars").join(format!("{style}-{:016x}.svg", hash(path))))
}

/// Download the avatar to `dest`. Blocking: run it off the main thread. The file
/// appears whole or not at all, so a half-written one is never drawn.
pub fn fetch(avatar: Avatar, path: &Path, dest: &Path) -> anyhow::Result<()> {
    let style = avatar.style().ok_or_else(|| anyhow::anyhow!("initials aren't fetched"))?;
    let url = format!("{API}/{style}/svg?seed={}", encode(&seed(path)));
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = dest.with_extension("part");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "15", "-o"])
        .arg(&partial)
        .arg(&url)
        .status()?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        anyhow::bail!("curl exited with {status}");
    }
    let bytes = std::fs::read(&partial)?;
    if !bytes.starts_with(b"<svg") {
        let _ = std::fs::remove_file(&partial);
        anyhow::bail!("not an SVG");
    }
    std::fs::rename(&partial, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_the_parent_and_folder_name() {
        assert_eq!(seed(Path::new("/Users/someone/code/app")), "code/app");
        assert_eq!(seed(Path::new("/app")), "app");
        assert_ne!(hash(Path::new("/x/code/app")), hash(Path::new("/x/code/other")));
        assert_eq!(hash(Path::new("/x/code/app")), hash(Path::new("/y/code/app")));
    }

    #[test]
    fn seeds_are_encoded_for_the_url() {
        assert_eq!(encode("code/my app"), "code%2Fmy%20app");
    }

    #[test]
    fn initials_have_no_file() {
        assert!(file(Avatar::Initials, Path::new("/a")).is_none());
    }
}
