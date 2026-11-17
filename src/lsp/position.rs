//! Turning places in what a pane shows into places a server understands, and
//! back. Panes show lines with tabs expanded to spaces; servers count columns
//! in the file's own text, in UTF-8 bytes or UTF-16 code units.
use super::Encoding;
use lsp_types::Position;

/// The raw line's byte offset for byte `shown` of the same line with tabs
/// expanded to `tab` columns. An offset inside a tab's spaces lands on the tab.
pub fn raw_offset(raw: &str, shown: usize, tab: usize) -> usize {
    let (mut column, mut at) = (0, 0);
    for (i, c) in raw.char_indices() {
        let width = if c == '\t' { tab - column % tab } else { c.len_utf8() };
        if shown < at + width {
            return i;
        }
        at += width;
        column += if c == '\t' { width } else { 1 };
    }
    raw.len()
}

/// The inverse of `raw_offset`: where byte `raw_byte` of a raw line is shown.
pub fn shown_offset(raw: &str, raw_byte: usize, tab: usize) -> usize {
    let (mut column, mut at) = (0, 0);
    for (i, c) in raw.char_indices() {
        if i >= raw_byte {
            return at;
        }
        let width = if c == '\t' { tab - column % tab } else { c.len_utf8() };
        at += width;
        column += if c == '\t' { width } else { 1 };
    }
    at
}

/// A byte offset in a line as a server's column.
pub fn to_column(line: &str, byte: usize, encoding: Encoding) -> u32 {
    let byte = byte.min(line.len());
    match encoding {
        Encoding::Utf8 => byte as u32,
        Encoding::Utf16 => line[..floor_char(line, byte)].encode_utf16().count() as u32,
    }
}

/// A server's column in a line as a byte offset, clamped to the line.
pub fn from_column(line: &str, column: u32, encoding: Encoding) -> usize {
    match encoding {
        Encoding::Utf8 => floor_char(line, column as usize),
        Encoding::Utf16 => {
            let mut units = 0;
            for (i, c) in line.char_indices() {
                if units >= column as usize {
                    return i;
                }
                units += c.len_utf16();
            }
            line.len()
        }
    }
}

fn floor_char(line: &str, byte: usize) -> usize {
    let mut byte = byte.min(line.len());
    while !line.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// The lines of a text, as a server counts them (`\r\n` is one break).
pub fn line(text: &str, n: usize) -> Option<&str> {
    text.split('\n').nth(n).map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// Where a place shown in a pane is in the server's copy of the file: `row`
/// is the line in `raw`, `shown` the byte in its tab-expanded form.
pub fn position(raw: &str, row: usize, shown: usize, tab: usize, encoding: Encoding) -> Option<Position> {
    let text = line(raw, row)?;
    let byte = raw_offset(text, shown, tab);
    Some(Position::new(row as u32, to_column(text, byte, encoding)))
}

/// The byte a server's position is shown at, in the tab-expanded line.
pub fn shown(raw: &str, at: Position, tab: usize, encoding: Encoding) -> Option<usize> {
    let text = line(raw, at.line as usize)?;
    Some(shown_offset(text, from_column(text, at.character, encoding), tab))
}

/// For each line of `from`, the line of `to` it is unchanged as, if any.
pub fn line_map(from: &str, to: &str) -> Vec<Option<usize>> {
    let diff = similar::TextDiff::from_lines(from, to);
    let mut map = vec![None; from.lines().count().max(diff.old_slices().len())];
    for op in diff.ops() {
        if let similar::DiffOp::Equal { old_index, new_index, len } = *op {
            for k in 0..len {
                if let Some(slot) = map.get_mut(old_index + k) {
                    *slot = Some(new_index + k);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_through_tabs() {
        // "\tx = 1" shows as "    x = 1" at width 4.
        let raw = "\tx = 1";
        assert_eq!(raw_offset(raw, 0, 4), 0);
        assert_eq!(raw_offset(raw, 3, 4), 0);
        assert_eq!(raw_offset(raw, 4, 4), 1);
        assert_eq!(raw_offset(raw, 8, 4), 5);
        assert_eq!(shown_offset(raw, 1, 4), 4);
        assert_eq!(shown_offset(raw, 5, 4), 8);
        // A tab after text only pads to the next stop.
        assert_eq!(shown_offset("ab\tc", 3, 4), 4);
        assert_eq!(raw_offset("ab\tc", 4, 4), 3);
    }

    #[test]
    fn counts_utf16_units() {
        let l = "let é = \"😀\"; x";
        let x = l.find(" x").unwrap() + 1;
        assert_eq!(to_column(l, x, Encoding::Utf8), x as u32);
        // é is one unit, the emoji two: 13 characters before `x`, 14 units.
        assert_eq!(to_column(l, x, Encoding::Utf16), 14);
        assert_eq!(from_column(l, 14, Encoding::Utf16), x);
        assert_eq!(from_column(l, 999, Encoding::Utf16), l.len());
    }

    #[test]
    fn positions_round_trip() {
        let raw = "fn a() {}\r\n\tlet café = b();\n";
        let p = position(raw, 1, 4 + "let café = ".len(), 4, Encoding::Utf16).unwrap();
        // The tab and 11 characters before `b`.
        assert_eq!(p, Position::new(1, 12));
        assert_eq!(shown(raw, p, 4, Encoding::Utf16), Some(4 + "let café = ".len()));
        assert_eq!(line(raw, 0), Some("fn a() {}"));
    }

    #[test]
    fn maps_unchanged_lines() {
        let from = "a\nb\nc\nd\n";
        let to = "a\nx\nb\nd\ne\n";
        assert_eq!(line_map(from, to), vec![Some(0), Some(2), None, Some(3)]);
    }
}
