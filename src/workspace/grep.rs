//! Finding text in a project's files. The walk keeps to what Git would (it
//! reads `.gitignore`s), skips binary and very large files, and stops once
//! enough lines have matched or a newer search has started.
use super::search::find;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Matching lines kept before the search stops.
pub const LIMIT: usize = 500;
/// Files past this size aren't searched.
const MAX_FILE: u64 = 2 * 1024 * 1024;
/// Characters of a long line kept around its first match.
const EXCERPT: usize = 160;
const LEAD: usize = 40;

/// A matching line: its number (from 1), its text cut to fit, and where it matched.
pub struct Line {
    pub number: usize,
    pub text: String,
    pub marks: Vec<Range<usize>>,
}

pub struct FileHit {
    pub path: PathBuf,
    pub lines: Vec<Line>,
}

/// What a finished search found, and for which project and query.
pub struct Results {
    pub project: usize,
    pub query: String,
    pub files: Vec<FileHit>,
    /// More lines matched than were kept.
    pub truncated: bool,
}

impl Results {
    pub fn lines(&self) -> usize {
        self.files.iter().map(|f| f.lines.len()).sum()
    }
}

/// Every line under `roots` holding `query`, ignoring case. Gives up early
/// (returning what it has) once `generation` moves past `mine`.
pub fn run(roots: &[PathBuf], query: &str, skipped: &[&str], generation: &AtomicUsize, mine: usize) -> (Vec<FileHit>, bool) {
    let needle = query.to_lowercase();
    let mut files = Vec::new();
    let mut count = 0;
    if needle.trim().is_empty() {
        return (files, false);
    }
    for root in roots {
        let skipped: Vec<String> = skipped.iter().map(|s| s.to_string()).collect();
        let walk = ignore::WalkBuilder::new(root)
            .hidden(false)
            .filter_entry(move |e| !skipped.iter().any(|s| e.file_name().to_string_lossy() == s.as_str()))
            .sort_by_file_path(|a, b| a.cmp(b))
            .build();
        for entry in walk.flatten() {
            if generation.load(Ordering::Relaxed) != mine {
                return (files, false);
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Some(lines) = search_file(entry.path(), &needle) else { continue };
            if lines.is_empty() {
                continue;
            }
            let room = LIMIT - count;
            let truncated = lines.len() > room;
            let lines: Vec<Line> = lines.into_iter().take(room).collect();
            count += lines.len();
            files.push(FileHit { path: entry.into_path(), lines });
            if truncated || count >= LIMIT {
                return (files, true);
            }
        }
    }
    (files, false)
}

/// The lines of one file holding `needle` (lowercase), or `None` for a file
/// that isn't text or is too large.
fn search_file(path: &Path, needle: &str) -> Option<Vec<Line>> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    Some(
        text.lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let marks = find(line, needle);
                (!marks.is_empty()).then(|| clip(i + 1, line, marks))
            })
            .collect(),
    )
}

/// A line without its indent, cut to the stretch around its first match when long.
fn clip(number: usize, line: &str, marks: Vec<Range<usize>>) -> Line {
    let indent = line.len() - line.trim_start().len();
    let line = line.trim();
    let marks: Vec<Range<usize>> =
        marks.into_iter().filter_map(|m| (m.start >= indent && m.end - indent <= line.len()).then(|| m.start - indent..m.end - indent)).collect();
    let Some(first) = marks.first().map(|m| m.start) else {
        return Line { number, text: line.to_string(), marks };
    };
    if line.chars().count() <= EXCERPT {
        return Line { number, text: line.to_string(), marks };
    }
    let skip = line[..first].chars().count().saturating_sub(LEAD);
    let start = line.char_indices().nth(skip).map_or(0, |(i, _)| i);
    let end = line[start..].char_indices().nth(EXCERPT).map_or(line.len(), |(i, _)| start + i);
    let pre = if start > 0 { "…" } else { "" };
    let post = if end < line.len() { "…" } else { "" };
    let marks = marks
        .into_iter()
        .filter(|m| m.start >= start && m.end <= end)
        .map(|m| m.start - start + pre.len()..m.end - start + pre.len())
        .collect();
    Line { number, text: format!("{pre}{}{post}", &line[start..end]), marks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("chat-grep-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn main() {\n    let Needle = 1;\n}\n").unwrap();
        std::fs::write(root.join("b.txt"), "no match\nneedle and NEEDLE\n").unwrap();
        std::fs::write(root.join("bin.dat"), b"needle\0binary").unwrap();
        std::fs::write(root.join("node_modules/x.js"), "needle").unwrap();
        root
    }

    #[test]
    fn finds_lines_in_text_files_only() {
        let root = tree();
        let generation = AtomicUsize::new(0);
        let (files, truncated) = run(&[root.clone()], "needle", &["node_modules"], &generation, 0);
        let found: Vec<_> = files
            .iter()
            .map(|f| (f.path.strip_prefix(&root).unwrap().to_string_lossy().into_owned(), f.lines.iter().map(|l| (l.number, l.text.clone())).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            found,
            [
                ("b.txt".to_string(), vec![(2, "needle and NEEDLE".to_string())]),
                ("src/a.rs".to_string(), vec![(2, "let Needle = 1;".to_string())]),
            ]
        );
        assert_eq!(files[0].lines[0].marks, [0..6, 11..17]);
        assert_eq!(files[1].lines[0].marks, [4..10]);
        assert!(!truncated);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stops_when_a_newer_search_starts() {
        let root = tree();
        let generation = AtomicUsize::new(1);
        let (files, _) = run(&[root.clone()], "needle", &[], &generation, 0);
        assert!(files.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let clipped = clip(1, &line, find(&line, "needle"));
        assert!(clipped.text.starts_with('…') && clipped.text.ends_with('…'));
        assert_eq!(&clipped.text[clipped.marks[0].clone()], "needle");
    }
}
