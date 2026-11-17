//! Picking a symbol in a code view by clicking it, and marking where else it
//! appears. Until a language server answers (and wherever it can't, such as a
//! diff's removed lines), the same word is matched as plain text, so this works
//! in any language and both sides of a diff.
use super::panes::LineSource;
use gpui::{HighlightStyle, Hsla, Pixels, Point, SharedString, UnderlineStyle, combine_highlights, px};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

/// Where a server says the picked symbol is used, by the pane's rows: byte
/// ranges in each row's shown text, and whether each one writes to it. Rows in
/// `unmapped` are ones the server can't see, which fall back to the text match.
#[derive(Clone, Default)]
pub(super) struct ServerMarks {
    pub rows: HashMap<usize, Vec<(Range<usize>, bool)>>,
    pub unmapped: HashSet<usize>,
}

/// The symbol picked in a code view: the word, where it was clicked (row,
/// shown byte and window point), and the server's ranges once they're in.
#[derive(Clone)]
pub(super) struct SymbolMark {
    pub source: LineSource,
    pub word: SharedString,
    pub row: usize,
    pub at: usize,
    pub point: Point<Pixels>,
    pub server: Option<ServerMarks>,
    /// Which pick this is, so a late reply for an earlier one is dropped.
    pub id: u64,
}

impl SymbolMark {
    /// What to mark in one row of `source`.
    pub fn row(&self, source: &LineSource, row: usize) -> RowMarks<'_> {
        if &self.source != source {
            return RowMarks::default();
        }
        match &self.server {
            Some(server) if !server.unmapped.contains(&row) => {
                RowMarks { word: None, server: Some(server.rows.get(&row).map_or(&[][..], Vec::as_slice)), link: None }
            }
            _ => RowMarks { word: Some(&self.word), server: None, link: None },
        }
    }
}

/// What a row draws over its syntax colours.
#[derive(Default)]
pub(super) struct RowMarks<'a> {
    /// Tint whole-word matches of this.
    pub word: Option<&'a str>,
    /// Or tint these, from a server.
    pub server: Option<&'a [(Range<usize>, bool)]>,
    /// Underline this, as a link to its definition (⌘ held).
    pub link: Option<Range<usize>>,
}

/// The colours marks are drawn in.
#[derive(Clone, Copy)]
pub(super) struct Tints {
    pub read: Hsla,
    pub write: Hsla,
    pub link: Hsla,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The word around byte `ix` of a line, when the character there is part of
/// one. Numbers don't count: marking every `0` says nothing.
pub(super) fn word_at(line: &str, ix: usize) -> Option<Range<usize>> {
    if ix >= line.len() || !line.is_char_boundary(ix) || !line[ix..].chars().next().is_some_and(is_word) {
        return None;
    }
    let start = line[..ix].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(ix, |(i, _)| i);
    let end = line[ix..].char_indices().find(|(_, c)| !is_word(*c)).map_or(line.len(), |(i, _)| ix + i);
    let word = &line[start..end];
    (!word.starts_with(|c: char| c.is_ascii_digit())).then_some(start..end)
}

/// Where `word` appears in a line as a whole word.
pub(super) fn occurrences(line: &str, word: &str) -> Vec<Range<usize>> {
    if word.is_empty() {
        return Vec::new();
    }
    line.match_indices(word)
        .map(|(i, _)| i..i + word.len())
        .filter(|r| {
            let before = line[..r.start].chars().next_back();
            let after = line[r.end..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .collect()
}

/// A line's syntax colours with the row's marks laid over them.
pub(super) fn overlay(marks: Vec<(Range<usize>, HighlightStyle)>, line: &str, row: RowMarks, tints: Tints) -> Vec<(Range<usize>, HighlightStyle)> {
    let tint = |write: bool| HighlightStyle { background_color: Some(if write { tints.write } else { tints.read }), ..Default::default() };
    let valid = |r: &Range<usize>| r.start < r.end && r.end <= line.len() && line.is_char_boundary(r.start) && line.is_char_boundary(r.end);
    let mut found: Vec<(Range<usize>, HighlightStyle)> = match (row.server, row.word) {
        (Some(ranges), _) => ranges.iter().filter(|(r, _)| valid(r)).map(|(r, w)| (r.clone(), tint(*w))).collect(),
        (None, Some(word)) => occurrences(line, word).into_iter().map(|r| (r, tint(false))).collect(),
        (None, None) => Vec::new(),
    };
    found.sort_by_key(|(r, _)| r.start);
    // Overlapping ranges would break the runs; keep the first of any pair.
    found.dedup_by(|b, a| b.0.start < a.0.end);
    let mut out = if found.is_empty() { marks } else { combine_highlights(marks, found).collect() };
    if let Some(link) = row.link.filter(valid) {
        let style = HighlightStyle {
            color: Some(tints.link),
            underline: Some(UnderlineStyle { thickness: px(1.), color: Some(tints.link), wavy: false }),
            ..Default::default()
        };
        out = combine_highlights(out, [(link, style)]).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tints() -> Tints {
        Tints { read: Hsla::red(), write: Hsla::green(), link: Hsla::blue() }
    }

    #[test]
    fn picks_the_word_under_the_pointer() {
        let line = "let total = sum_of(items);";
        assert_eq!(word_at(line, 6), Some(4..9));
        assert_eq!(word_at(line, 4), Some(4..9));
        assert_eq!(word_at(line, 14), Some(12..18));
        // Spaces, punctuation and the end of the line pick nothing.
        assert_eq!(word_at(line, 3), None);
        assert_eq!(word_at(line, 18), None);
        assert_eq!(word_at(line, line.len()), None);
    }

    #[test]
    fn skips_numbers_and_takes_unicode_and_dollar() {
        assert_eq!(word_at("x = 42;", 4), None);
        assert_eq!(word_at("const $el = café;", 7), Some(6..9));
        assert_eq!(word_at("const $el = café;", 13), Some(12..17));
    }

    #[test]
    fn matches_whole_words_only() {
        let line = "item items item_count (item)";
        assert_eq!(occurrences(line, "item"), vec![0..4, 23..27]);
        assert!(occurrences(line, "").is_empty());
    }

    #[test]
    fn tints_occurrences_over_syntax_colours() {
        let colour = HighlightStyle { color: Some(Hsla::default()), ..Default::default() };
        let row = RowMarks { word: Some("foo"), ..Default::default() };
        let out = overlay(vec![(0..3, colour)], "foo(foo)", row, tints());
        let tinted: Vec<_> = out.iter().filter(|(_, s)| s.background_color.is_some()).map(|(r, _)| r.clone()).collect();
        assert_eq!(tinted, vec![0..3, 4..7]);
        assert!(out.iter().any(|(r, s)| *r == (0..3) && s.color.is_some()));
    }

    #[test]
    fn server_ranges_replace_the_text_match_and_tell_writes_apart() {
        let ranges = [(0..1, true), (7..8, false), (20..30, false)];
        let row = RowMarks { word: Some("x"), server: Some(&ranges), link: None };
        let out = overlay(Vec::new(), "x = 1; x + x", row, tints());
        let tinted: Vec<_> = out.iter().filter_map(|(r, s)| Some((r.clone(), s.background_color?))).collect();
        // The last `x` isn't the server's, and the out-of-range one is dropped.
        assert_eq!(tinted, vec![(0..1, Hsla::green()), (7..8, Hsla::red())]);
    }

    #[test]
    fn underlines_the_link() {
        let row = RowMarks { link: Some(4..7), ..Default::default() };
        let out = overlay(Vec::new(), "let foo = 1;", row, tints());
        assert!(out.iter().any(|(r, s)| *r == (4..7) && s.underline.is_some()));
    }

    #[test]
    fn rows_the_server_cant_see_fall_back_to_text() {
        let mut server = ServerMarks::default();
        server.rows.insert(1, vec![(0..3, false)]);
        server.unmapped.insert(2);
        let source = LineSource::File("/a.rs".into());
        let mark = SymbolMark { source: source.clone(), word: "foo".into(), row: 1, at: 0, point: Point::default(), server: Some(server), id: 0 };
        assert!(mark.row(&source, 1).server.is_some_and(|r| r.len() == 1));
        assert!(mark.row(&source, 0).server.is_some_and(|r| r.is_empty()));
        assert_eq!(mark.row(&source, 2).word, Some("foo"));
        assert!(mark.row(&LineSource::File("/b.rs".into()), 1).word.is_none());
    }
}
