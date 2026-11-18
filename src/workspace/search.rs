//! Searching what threads say, not just what they're called. The text is
//! copied out of the threads and matched on a background thread; hits come
//! back grouped by thread, each paragraph that holds the query listed once.
use crate::model::{Block, Project};
use std::{collections::HashSet, ops::Range};

/// Paragraphs listed under one thread; the rest are counted.
pub const SHOWN: usize = 4;
/// Characters of a long paragraph kept around its first match.
const EXCERPT: usize = 180;
const LEAD: usize = 50;

/// One thread's searchable text: its id and its messages, in order.
pub struct Doc {
    pub id: usize,
    texts: Vec<String>,
}

/// A paragraph that holds the query, cut down to fit, with where it matched.
pub struct Snippet {
    pub text: String,
    pub marks: Vec<Range<usize>>,
}

pub struct Hit {
    pub thread: usize,
    pub snippets: Vec<Snippet>,
    /// Matching paragraphs past the first `SHOWN`.
    pub more: usize,
}

/// What a finished search found, and for which project and query.
pub struct Results {
    pub project: usize,
    pub query: String,
    pub hits: Vec<Hit>,
}

impl Results {
    pub fn get(&self, thread: usize) -> Option<&Hit> {
        self.hits.iter().find(|h| h.thread == thread)
    }
}

/// The project's threads as text to search: what was said each way, leaving
/// out tool output and the agent's private reasoning.
pub fn docs(project: &Project) -> Vec<Doc> {
    project
        .threads
        .iter()
        .map(|t| Doc {
            id: t.id,
            texts: t
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::User(m) => Some(m.text.to_string()),
                    Block::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect(),
        })
        .collect()
}

/// Every thread with a paragraph that holds `query`, ignoring case.
pub fn run(docs: &[Doc], query: &str) -> Vec<Hit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    docs.iter()
        .filter_map(|doc| {
            let mut seen = HashSet::new();
            let mut snippets = Vec::new();
            let mut count = 0;
            for paragraph in doc.texts.iter().flat_map(|t| paragraphs(t)) {
                let text = collapse(paragraph);
                let marks = find(&text, &needle);
                if marks.is_empty() || !seen.insert(text.clone()) {
                    continue;
                }
                count += 1;
                if snippets.len() < SHOWN {
                    snippets.push(excerpt(text, marks));
                }
            }
            (count > 0).then(|| Hit { thread: doc.id, more: count - snippets.len(), snippets })
        })
        .collect()
}

/// Where `needle` (already lowercase) appears in `hay`, as byte ranges of `hay`.
pub fn find(hay: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    // Lowercasing can change a character's length, so keep where each byte
    // of the lowered text came from.
    let mut lowered = String::with_capacity(hay.len());
    let mut origin = Vec::with_capacity(hay.len() + 1);
    for (at, c) in hay.char_indices() {
        for l in c.to_lowercase() {
            let before = lowered.len();
            lowered.push(l);
            origin.extend(std::iter::repeat_n(at, lowered.len() - before));
        }
    }
    origin.push(hay.len());
    let end_of = |i: usize| {
        // The match ends with the character that produced its last byte.
        let start = origin[i - 1];
        start + hay[start..].chars().next().map_or(0, char::len_utf8)
    };
    let mut marks: Vec<Range<usize>> = Vec::new();
    let mut from = 0;
    while let Some(found) = lowered[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        let range = origin[start]..end_of(end);
        match marks.last_mut() {
            Some(last) if last.end >= range.start => last.end = last.end.max(range.end),
            _ => marks.push(range),
        }
        from = end;
    }
    marks
}

/// Text split at blank lines.
fn paragraphs(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || loop {
        if rest.is_empty() {
            return None;
        }
        let (para, next) = match blank_line(rest) {
            Some(r) => (&rest[..r.start], &rest[r.end..]),
            None => (rest, ""),
        };
        rest = next;
        if !para.trim().is_empty() {
            return Some(para);
        }
    })
}

/// The first run of a newline, optional spaces, then another newline.
fn blank_line(text: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(n) = text[i..].find('\n') {
        let start = i + n;
        let mut j = start + 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'\n' {
            let mut end = j + 1;
            while end < bytes.len() && matches!(bytes[end], b' ' | b'\t' | b'\r' | b'\n') {
                end += 1;
            }
            return Some(start..end);
        }
        i = start + 1;
    }
    None
}

/// One line of single spaces.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A long paragraph cut to the stretch around its first match.
fn excerpt(text: String, marks: Vec<Range<usize>>) -> Snippet {
    if text.chars().count() <= EXCERPT {
        return Snippet { text, marks };
    }
    let first = marks[0].start;
    let lead = text[..first].chars().count();
    let skip = lead.saturating_sub(LEAD);
    let start = text.char_indices().nth(skip).map_or(0, |(i, _)| i);
    let end = text[start..].char_indices().nth(EXCERPT).map_or(text.len(), |(i, _)| start + i);
    let (pre, post) = (if start > 0 { "…" } else { "" }, if end < text.len() { "…" } else { "" });
    let shift = pre.len();
    let marks = marks
        .into_iter()
        .filter(|m| m.start >= start && m.end <= end)
        .map(|m| m.start - start + shift..m.end - start + shift)
        .collect();
    Snippet { text: format!("{pre}{}{post}", &text[start..end]), marks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: usize, texts: &[&str]) -> Doc {
        Doc { id, texts: texts.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn groups_paragraphs_by_thread_once_each() {
        let docs = [
            doc(1, &["Fix the Parser.\n\nIt crashes on empty input.", "fix the  parser."]),
            doc(2, &["nothing here"]),
            doc(3, &["the parser\n  \nand the lexer"]),
        ];
        let hits = run(&docs, "PARSER");
        assert_eq!(hits.iter().map(|h| h.thread).collect::<Vec<_>>(), [1, 3]);
        // Same words, different case: still two paragraphs as written.
        assert_eq!(hits[0].snippets.len(), 2);
        assert_eq!(hits[0].snippets[0].text, "Fix the Parser.");
        assert_eq!(hits[0].snippets[0].marks, [8..14]);
        assert_eq!(hits[1].snippets[0].text, "the parser");
    }

    #[test]
    fn repeated_paragraphs_are_listed_once() {
        let docs = [doc(1, &["run the tests", "run the tests", "run   the\ntests"])];
        let hits = run(&docs, "tests");
        assert_eq!(hits[0].snippets.len(), 1);
        assert_eq!(hits[0].more, 0);
    }

    #[test]
    fn counts_what_isnt_shown() {
        let texts: Vec<String> = (0..7).map(|n| format!("match {n}")).collect();
        let docs = [Doc { id: 1, texts }];
        let hits = run(&docs, "match");
        assert_eq!((hits[0].snippets.len(), hits[0].more), (SHOWN, 7 - SHOWN));
    }

    #[test]
    fn matches_map_back_through_case_changes() {
        // 'İ' lowercases to two characters; the mark must still land on "x".
        assert_eq!(find("İx", "x"), [2..3]);
        assert_eq!(find("Straße STRASSE", "straße"), [0..7]);
        assert_eq!(find("aaaa", "aa"), [0..4]);
    }

    #[test]
    fn long_paragraphs_are_cut_around_the_match() {
        let text = format!("{} needle {}", "a ".repeat(200), "b ".repeat(200));
        let hits = run(&[doc(1, &[&text])], "needle");
        let s = &hits[0].snippets[0];
        assert!(s.text.starts_with('…') && s.text.ends_with('…'));
        assert_eq!(&s.text[s.marks[0].clone()], "needle");
    }
}
