//! Tree-sitter syntax highlighting (GPUI Component's grammars, as in Zed), in
//! Atom's One Light and One Dark. Carried over from Gitflow. Styles are split
//! per line so virtualized views can colour only what they draw, and both
//! themes are computed, so switching appearance is free.
use gpui::{HighlightStyle, SharedString};
use gpui_component::{
    Rope,
    highlighter::{HighlightTheme, SyntaxHighlighter},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    rc::Rc,
    sync::LazyLock,
};

/// Larger files show as plain text; highlighting them would stall the view.
const MAX_BYTES: usize = 1_500_000;
const MAX_LINES: usize = 40_000;

pub type LineStyles = Vec<(Range<usize>, HighlightStyle)>;
pub struct Highlights {
    pub light: Vec<LineStyles>,
    pub dark: Vec<LineStyles>,
}
impl Highlights {
    pub fn line(&self, dark: bool, n: usize) -> Option<&LineStyles> {
        if dark { self.dark.get(n) } else { self.light.get(n) }
    }
}

/// The grammar for a file name or a fenced code block's tag, if one is bundled.
pub fn language(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    match file {
        "makefile" | "gnumakefile" => return Some("make"),
        "cmakelists.txt" => return Some("cmake"),
        _ => {}
    }
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or(file);
    Some(match ext {
        "rs" | "rust" => "rust",
        "ts" | "mts" | "cts" | "typescript" => "typescript",
        // The bundled TSX grammar carries only TypeScript's own highlight query (no
        // strings, keywords or JSX); JavaScript's reads TSX far better.
        "tsx" => "javascript",
        "js" | "mjs" | "cjs" | "jsx" | "javascript" => "javascript",
        "py" | "pyi" | "python" => "python",
        "go" | "golang" => "go",
        "rb" | "ruby" | "rake" | "gemspec" => "ruby",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "c++" => "cpp",
        "cs" | "csharp" => "csharp",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" | "mdx" => "markdown",
        "sh" | "bash" | "zsh" | "shell" | "console" => "bash",
        "sql" => "sql",
        "swift" => "swift",
        "zig" => "zig",
        "ex" | "exs" | "elixir" => "elixir",
        "graphql" | "gql" => "graphql",
        "proto" | "protobuf" => "proto",
        "scala" | "sc" => "scala",
        "diff" | "patch" => "diff",
        "erb" => "erb",
        "ejs" => "ejs",
        "make" | "mk" => "make",
        "cmake" => "cmake",
        _ => return None,
    })
}

/// Highlight `lines` (already tab-expanded) as `lang`, for both themes.
pub fn highlight(lang: &str, lines: &[SharedString]) -> Option<Highlights> {
    if lines.len() > MAX_LINES {
        return None;
    }
    let mut text = String::new();
    let mut starts = Vec::with_capacity(lines.len());
    for l in lines {
        starts.push(text.len());
        text.push_str(l);
        text.push('\n');
    }
    if text.len() > MAX_BYTES {
        return None;
    }
    let mut highlighter = SyntaxHighlighter::new(lang);
    highlighter.update(None, &Rope::from_str(&text));
    let whole = 0..text.len();
    let split = |theme: &HighlightTheme| per_line(&highlighter.styles(&whole, theme), &starts, lines);
    Some(Highlights {
        light: split(&ONE_LIGHT),
        dark: split(&ONE_DARK),
    })
}

/// A text as the lines views draw: no line endings, tabs as four spaces (a tab
/// has no width of its own in a text run).
pub fn lines(text: &str) -> Vec<SharedString> {
    text.lines().map(|l| l.replace('\t', "    ").into()).collect()
}

/// How many highlighted texts to keep before starting over.
const CACHE: usize = 256;

/// Highlight a whole text by its `lines`, remembering the result. Views are
/// rebuilt on every frame, and a code block or a diff side must not be parsed
/// again each time. `None` when no grammar fits or the text is too large.
pub fn cached(name: &str, text: &str) -> Option<Rc<Highlights>> {
    thread_local! {
        static SEEN: RefCell<HashMap<u64, Option<Rc<Highlights>>>> = RefCell::default();
    }
    let lang = language(name)?;
    let mut hasher = DefaultHasher::new();
    (lang, text).hash(&mut hasher);
    let key = hasher.finish();
    SEEN.with_borrow_mut(|seen| {
        if let Some(hit) = seen.get(&key) {
            return hit.clone();
        }
        // A streaming code block leaves one entry per chunk behind.
        if seen.len() >= CACHE {
            seen.clear();
        }
        let made = highlight(lang, &lines(text)).map(Rc::new);
        seen.insert(key, made.clone());
        made
    })
}

/// Code colours: Atom's One Dark and One Light, in plain weights.
static ONE_DARK: LazyLock<HighlightTheme> = LazyLock::new(|| {
    one(
        "dark",
        Ui {
            fg: "#abb2bf",
            comment: "#5c6370",
            red: "#e06c75",
            orange: "#d19a66",
            yellow: "#e5c07b",
            green: "#98c379",
            cyan: "#56b6c2",
            blue: "#61afef",
            purple: "#c678dd",
        },
    )
});
static ONE_LIGHT: LazyLock<HighlightTheme> = LazyLock::new(|| {
    one(
        "light",
        Ui {
            fg: "#383a42",
            comment: "#a0a1a7",
            red: "#e45649",
            orange: "#986801",
            yellow: "#c18401",
            green: "#50a14f",
            cyan: "#0184bc",
            blue: "#4078f2",
            purple: "#a626a4",
        },
    )
});

struct Ui {
    fg: &'static str,
    comment: &'static str,
    red: &'static str,
    orange: &'static str,
    yellow: &'static str,
    green: &'static str,
    cyan: &'static str,
    blue: &'static str,
    purple: &'static str,
}
fn one(appearance: &str, c: Ui) -> HighlightTheme {
    let color = |hex: &str| serde_json::json!({ "color": hex });
    let syntax = serde_json::json!({
        "attribute": color(c.orange),
        "boolean": color(c.orange),
        "comment": color(c.comment),
        "comment.doc": color(c.comment),
        "constant": color(c.orange),
        "constructor": color(c.yellow),
        "embedded": color(c.fg),
        "enum": color(c.yellow),
        "function": color(c.blue),
        "keyword": color(c.purple),
        "label": color(c.blue),
        "link_text": color(c.blue),
        "link_uri": color(c.cyan),
        "number": color(c.orange),
        "operator": color(c.cyan),
        "preproc": color(c.purple),
        "property": color(c.red),
        "punctuation": color(c.fg),
        "punctuation.bracket": color(c.fg),
        "punctuation.delimiter": color(c.fg),
        "punctuation.list_marker": color(c.red),
        "punctuation.special": color(c.purple),
        "string": color(c.green),
        "string.escape": color(c.cyan),
        "string.regex": color(c.cyan),
        "string.special": color(c.cyan),
        "string.special.symbol": color(c.cyan),
        "tag": color(c.red),
        "tag.doctype": color(c.purple),
        "text.literal": color(c.green),
        "title": color(c.red),
        "type": color(c.yellow),
        "variable": color(c.fg),
        "variable.special": color(c.red),
        "variant": color(c.blue),
    });
    serde_json::from_value(serde_json::json!({
        "name": format!("One {appearance}"),
        "appearance": appearance,
        "style": { "editor.foreground": c.fg, "syntax": syntax },
    }))
    .expect("One theme")
}

/// Clip whole-text styles to each line, as line-relative ranges.
fn per_line(styles: &[(Range<usize>, HighlightStyle)], starts: &[usize], lines: &[SharedString]) -> Vec<LineStyles> {
    let mut out = vec![LineStyles::new(); lines.len()];
    for (range, style) in styles {
        if *style == HighlightStyle::default() || range.is_empty() {
            continue;
        }
        let mut line = starts.partition_point(|s| *s <= range.start).saturating_sub(1);
        while line < lines.len() {
            let (start, len) = (starts[line], lines[line].len());
            if start >= range.end {
                break;
            }
            let from = range.start.max(start) - start;
            let to = range.end.min(start + len) - start;
            if from < to {
                out[line].push((from..to, *style));
            }
            line += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{cached, highlight, language, lines};
    use gpui::SharedString;
    #[test]
    fn picks_languages_by_name() {
        assert_eq!(language("src/main.rs"), Some("rust"));
        assert_eq!(language("App.TSX"), Some("javascript"));
        assert_eq!(language("Makefile"), Some("make"));
        assert_eq!(language("py"), Some("python"));
        assert_eq!(language("notes.txt"), None);
    }
    #[test]
    fn styles_stay_within_their_lines() {
        let lines = ["fn main() {", "    let x = \"hi\";", "}"].map(SharedString::from);
        let h = highlight("rust", &lines).expect("rust grammar");
        assert_eq!(h.dark.len(), 3);
        assert!(h.dark[0].iter().any(|(r, _)| &lines[0][r.clone()] == "fn"));
        for (n, styles) in h.light.iter().enumerate() {
            assert!(styles.iter().all(|(r, _)| r.end <= lines[n].len()));
        }
    }
    #[test]
    fn caches_by_language_and_text() {
        let text = "fn main() {\n\tlet x = 1;\n}\n";
        let first = cached("rust", text).expect("rust grammar");
        assert!(std::rc::Rc::ptr_eq(&first, &cached("a/b.rs", text).unwrap()));
        assert_eq!(first.dark.len(), lines(text).len());
        assert_eq!(lines(text)[1], "    let x = 1;");
        assert!(cached("txt", text).is_none());
    }
}
