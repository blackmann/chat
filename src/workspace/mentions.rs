//! Files and symbols in the composer. A mention sits in the text as a token —
//! invisible markers around padding and a name — which is drawn as a chip with
//! a border and the file's icon, edits as one piece, and is replaced by its
//! path when the message is sent. Typing `@` offers the project's files and
//! symbols to mention.
use super::{ui::*, *};
use crate::icons::Glyph;
use gpui_component::{
    IconName, IconNamed,
    input::{Backspace, Delete, Enter, Escape, IndentInline, InputEvent, MoveDown, MoveLeft, MoveRight, MoveUp},
};
use std::ops::Range;

/// Word joiner: zero-width, but survives editing and copying, so it can bind a
/// visible name to the path behind it.
pub(super) const MARKER: &str = "\u{2060}";
/// Room for the icon: figure spaces don't break lines and are wide enough.
pub(super) const LEAD: &str = "\u{2007}\u{2007}\u{2007}";
pub(super) const TRAIL: &str = "\u{a0}\u{a0}";
/// Suggestions shown at once.
const SHOWN: usize = 50;
/// Files the index reads symbols from, and how large each may be.
const SYMBOL_FILES: usize = 4000;
const SYMBOL_FILE_SIZE: u64 = 256 * 1024;
/// How long an index is trusted before it is built again.
const STALE: Duration = Duration::from_secs(30);

/// The icon for a path, by what kind of file it is.
pub(super) fn icon_path(path: &Path, dir: bool) -> SharedString {
    if dir {
        return IconName::Folder.path();
    }
    super::file_kind::file_kind(path).0
}

/// A mention of `label`, sending `sent` in its place.
pub(super) fn mention(label: &str, sent: String, icon: SharedString) -> FileMention {
    let shown = label.replace(' ', "\u{a0}");
    FileMention { token: format!("{MARKER}{LEAD}{shown}{TRAIL}{MARKER}"), label: label.to_string(), reference: sent.clone(), sent, icon }
}

/// Widen a mention's token so the room the input gives it holds the name as
/// the chip draws it: in the mono face, which runs wider than the input's.
pub(super) fn fit(mut m: FileMention, window: &Window) -> FileMention {
    let shown = m.label.replace(' ', "\u{a0}");
    // The composer's text is `text_sm` against a rem of the theme's base size.
    let input = (crate::theme::ui_size() + 2.) * 0.875;
    let drawn = text_width_in(window, &shown, crate::theme::mono_size(), crate::theme::mono());
    let room = text_width_in(window, &shown, input, crate::theme::ui_font());
    let space = text_width_in(window, "\u{a0}", input, crate::theme::ui_font()).max(px(1.));
    let extra = ((drawn - room) / space).ceil().max(0.) as usize;
    if extra > 0 {
        let end = m.token.len() - MARKER.len();
        m.token.insert_str(end, &"\u{a0}".repeat(extra));
    }
    m
}

/// How a path is sent to the agent: relative to the project where it can be.
pub(super) fn sent_path(path: &Path, root: Option<&Path>) -> String {
    let shown = root
        .and_then(|root| path.strip_prefix(root).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(path);
    format!("@{}", shown.to_string_lossy())
}

pub(super) fn path_mention(path: &Path, root: Option<&Path>) -> FileMention {
    let label = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    mention(&label, sent_path(path, root), icon_path(path, path.is_dir()))
}

/// A mention standing for a snippet: drawn as `label`, kept in the transcript as
/// `reference`, and sending `body` to the agent.
pub(super) fn snippet(label: &str, reference: String, body: String, icon: SharedString) -> FileMention {
    FileMention { reference, sent: body, ..mention(label, String::new(), icon) }
}

/// The same snippet under another label, so two snippets never share a token.
pub(super) fn relabel(m: &FileMention, label: &str) -> FileMention {
    snippet(label, m.reference.clone(), m.sent.clone(), m.icon.clone())
}

/// `text` fenced as a code block, with a fence longer than any backtick run inside.
pub(super) fn code_block(lang: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{lang}\n{}\n{fence}", text.trim_end_matches('\n'))
}

/// `name:12-30`, or `name:12` for one line.
pub(super) fn lines_label(name: &str, first: usize, last: usize) -> String {
    if first == last { format!("{name}:{first}") } else { format!("{name}:{first}-{last}") }
}

/// Text selected in the transcript, quoted. The chip shows how it starts.
pub(super) fn quote_mention(text: &str) -> FileMention {
    const SHOWN: usize = 28;
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut label: String = first.chars().take(SHOWN).collect();
    if first.chars().count() > SHOWN || text.trim().lines().count() > 1 {
        label = format!("{}…", label.trim_end());
    }
    let quoted = text.trim_matches('\n').lines().map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") }).collect::<Vec<_>>().join("\n");
    snippet(&format!("“{label}”"), "quote".into(), format!("\n\n{quoted}\n\n"), Glyph::Message.path())
}

/// Lines of a file, by their 1-based numbers.
pub(super) fn file_lines_mention(path: &Path, root: Option<&Path>, first: usize, last: usize, text: &str) -> FileMention {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    let at = sent_path(path, root);
    let reference = lines_label(&at, first, last);
    let lang = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let body = format!("\n\n{reference}\n{}\n\n", code_block(lang, text));
    snippet(&lines_label(&name, first, last), reference, body, icon_path(path, false))
}

/// Rows of a diff, already marked `+`, `-` or ` `. `lines` are the numbers the
/// rows cover, new-side where there are any.
pub(super) fn diff_lines_mention(path: &Path, root: Option<&Path>, lines: Option<(usize, usize)>, diff: &str) -> FileMention {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    let at = sent_path(path, root);
    let (label, reference) = match lines {
        Some((first, last)) => (lines_label(&name, first, last), lines_label(&at, first, last)),
        None => (name.to_string(), at),
    };
    let body = format!("\n\n{reference} (diff)\n{}\n\n", code_block("diff", diff));
    snippet(&label, reference, body, Glyph::FileDiff.path())
}

fn utf16(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].encode_utf16().count()
}
fn byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// Where each mention's token is in `text`, in bytes, in order.
fn tokens(text: &str, mentions: &[FileMention]) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = Vec::new();
    for m in mentions {
        for (start, _) in text.match_indices(&m.token) {
            let range = start..start + m.token.len();
            if !found.contains(&range) {
                found.push(range);
            }
        }
    }
    found.sort_by_key(|r| r.start);
    found
}

/// The `@query` being typed before the cursor: where its `@` is, and the query.
fn query_at(text: &str, cursor: usize) -> Option<(usize, &str)> {
    let before = text.get(..cursor)?;
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    // An `@` inside a word, as in an email address, isn't a mention.
    let prev = before[..at].chars().next_back();
    prev.is_none_or(|c| c.is_whitespace() || c == '(' || MARKER.starts_with(c)).then_some((at, query))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Dir,
    Symbol(&'static str),
}

/// Something that can be mentioned.
pub(super) struct Candidate {
    pub label: String,
    pub detail: String,
    pub sent: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// Lowercased text matched against, and where its last component starts.
    search: String,
    base: usize,
    depth: usize,
}

pub(super) struct Index {
    /// The project's main folder (`None` for chats): indexes move as projects are reordered.
    pub project: Option<PathBuf>,
    pub built: Instant,
    pub items: Vec<Candidate>,
}

/// Files under `root`: what Git tracks or would, or a plain walk outside a repository.
fn files_in(root: &Path) -> Vec<PathBuf> {
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()
        .filter(|o| o.status.success());
    if let Some(out) = listed {
        return out
            .stdout
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| root.join(String::from_utf8_lossy(b).as_ref()))
            .collect();
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            if SKIPPED.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(_) => out.push(entry.path()),
                Err(_) => {}
            }
            if out.len() > 20_000 {
                return out;
            }
        }
    }
    out
}

/// Declarations worth mentioning, found line by line: name, kind, line number.
fn symbols(text: &str) -> Vec<(String, &'static str, usize)> {
    const PREFIXES: &[&str] = &[
        "pub(crate) ", "pub(super) ", "pub ", "export default ", "export ", "async ", "static ", "public ", "private ",
        "protected ", "abstract ", "unsafe ", "final ", "open ", "internal ",
    ];
    const KEYWORDS: &[(&str, &str)] = &[
        ("fn ", "fn"),
        ("struct ", "struct"),
        ("enum ", "enum"),
        ("trait ", "trait"),
        ("type ", "type"),
        ("class ", "class"),
        ("interface ", "interface"),
        ("def ", "def"),
        ("function ", "function"),
        ("func ", "func"),
        ("protocol ", "protocol"),
    ];
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut rest = line.trim_start();
        // Deeply nested lines are rarely declarations anyone names.
        if line.len() - rest.len() > 8 {
            continue;
        }
        while let Some(stripped) = PREFIXES.iter().find_map(|p| rest.strip_prefix(p)) {
            rest = stripped;
        }
        for (keyword, kind) in KEYWORDS {
            if let Some(after) = rest.strip_prefix(keyword) {
                let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
                if !name.is_empty() {
                    out.push((name, *kind, n + 1));
                }
                break;
            }
        }
    }
    out
}

/// Everything in the project's folders that can be mentioned.
fn build(roots: &[PathBuf], primary: Option<&Path>) -> Vec<Candidate> {
    let several = roots.len() > 1;
    let mut items = Vec::new();
    let mut dirs = std::collections::BTreeSet::new();
    let mut code_files = 0;
    for root in roots {
        let prefix = if several { Some(crate::model::folder_name(root)) } else { None };
        let shown = |path: &Path| {
            let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
            match &prefix {
                Some(p) => format!("{p}/{rel}"),
                None => rel,
            }
        };
        for file in files_in(root) {
            let rel = shown(&file);
            for dir in file.parent().into_iter().flat_map(Path::ancestors) {
                if dir.starts_with(root) && dir != root.as_path() {
                    dirs.insert((dir.to_path_buf(), shown(dir)));
                } else {
                    break;
                }
            }
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if code_files < SYMBOL_FILES
                && syntax::language(&file.to_string_lossy()).is_some()
                && std::fs::metadata(&file).is_ok_and(|m| m.len() <= SYMBOL_FILE_SIZE)
            {
                code_files += 1;
                if let Ok(text) = std::fs::read_to_string(&file) {
                    for (symbol, kind, line) in symbols(&text) {
                        items.push(Candidate {
                            search: symbol.to_lowercase(),
                            base: 0,
                            depth: 0,
                            sent: format!("`{symbol}` ({}:{line})", sent_path(&file, primary)),
                            detail: format!("{rel}:{line}"),
                            label: symbol,
                            path: file.clone(),
                            kind: Kind::Symbol(kind),
                        });
                    }
                }
            }
            items.push(Candidate {
                base: rel.len() - name.len(),
                depth: rel.matches('/').count(),
                search: rel.to_lowercase(),
                detail: rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default(),
                sent: sent_path(&file, primary),
                label: name,
                path: file,
                kind: Kind::File,
            });
        }
    }
    for (dir, rel) in dirs {
        let name = crate::model::folder_name(&dir);
        items.push(Candidate {
            base: rel.len() - name.len(),
            depth: rel.matches('/').count(),
            search: rel.to_lowercase(),
            detail: rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default(),
            sent: sent_path(&dir, primary),
            label: name,
            path: dir,
            kind: Kind::Dir,
        });
    }
    items
}

/// How well `query` matches: its characters in order, scored up for runs and
/// for landing on the file name. `None` when they aren't all there.
fn score(query: &[char], item: &Candidate) -> Option<i32> {
    let mut score = 0;
    let mut last: Option<usize> = None;
    let mut chars = item.search.char_indices();
    for q in query {
        let (i, _) = chars.by_ref().find(|(_, c)| c == q)?;
        if last.is_some_and(|l| i == l + 1) {
            score += 8;
        }
        if i >= item.base {
            score += 4;
        }
        if i == item.base {
            score += 12;
        }
        last = Some(i);
    }
    if item.search[item.base..].starts_with(&query.iter().collect::<String>()) {
        score += 30;
    }
    // Symbols are many; files and folders come first on a tie.
    if matches!(item.kind, Kind::Symbol(_)) {
        score -= 6;
    }
    Some(score - item.search.len() as i32 / 8 - item.depth as i32)
}

impl Workspace {
    pub(super) fn observe_mention_draft(&mut self, cx: &mut Context<Self>) {
        cx.subscribe(&self.composer, |s, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change | InputEvent::Blur) {
                s.mention_selected = 0;
                s.mention_scroll.set_offset(point(px(0.), px(0.)));
                s.mention_dismissed = false;
                // Forget files a deleted chip pointed at.
                let input = s.composer.read(cx);
                let text = input.value().to_string();
                let typing = query_at(&text, input.cursor()).is_some();
                s.file_mentions.borrow_mut().retain(|m| text.contains(&m.token));
                if typing {
                    s.ensure_mention_index(cx);
                }
                cx.notify();
            }
        })
        .detach();
    }

    /// The `@query` before the cursor, when one is being typed: its `@` (bytes) and the query.
    fn mention_query(&self, window: &Window, cx: &App) -> Option<(usize, String)> {
        let input = self.composer.read(cx);
        if self.mention_dismissed || self.popup.is_some() || !input.focus_handle(cx).is_focused(window) {
            return None;
        }
        let text = input.value();
        query_at(&text, input.cursor()).map(|(at, q)| (at, q.to_string()))
    }

    /// Build the project's index in the background, unless a fresh one is there.
    pub(super) fn ensure_mention_index(&mut self, cx: &mut Context<Self>) {
        let fresh = self.mention_index.as_ref().is_some_and(|i| i.project == self.current_project().path && i.built.elapsed() < STALE);
        if fresh || self.mention_indexing {
            return;
        }
        let roots = self.current_project().roots();
        let primary = self.current_project().path.clone();
        let project = primary.clone();
        self.mention_indexing = true;
        cx.spawn(async move |this, cx| {
            let items = cx.background_executor().spawn(async move { build(&roots, primary.as_deref()) }).await;
            let _ = this.update(cx, |s, cx| {
                s.mention_indexing = false;
                s.mention_index = Some(Rc::new(Index { project, built: Instant::now(), items }));
                cx.notify();
            });
        })
        .detach();
    }

    /// What matches the query, best first. With no query, the files nearest the top.
    fn mention_matches(&self, query: &str) -> Vec<&Candidate> {
        let Some(index) = self.mention_index.as_ref().filter(|i| i.project == self.current_project().path) else {
            return Vec::new();
        };
        let query: Vec<char> = query.to_lowercase().chars().collect();
        if query.is_empty() {
            let mut files: Vec<&Candidate> = index.items.iter().filter(|c| !matches!(c.kind, Kind::Symbol(_))).collect();
            files.sort_by(|a, b| (a.depth, a.kind == Kind::File, &a.search).cmp(&(b.depth, b.kind == Kind::File, &b.search)));
            files.truncate(SHOWN);
            return files;
        }
        let mut ranked: Vec<(i32, &Candidate)> = index.items.iter().filter_map(|c| score(&query, c).map(|s| (s, c))).collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.search.len().cmp(&b.1.search.len())));
        ranked.into_iter().take(SHOWN).map(|(_, c)| c).collect()
    }

    /// Files (not folders or symbols) whose path matches `query`, best first.
    pub(super) fn file_matches(&self, query: &str, limit: usize) -> Vec<&Candidate> {
        let Some(index) = self.mention_index.as_ref().filter(|i| i.project == self.current_project().path) else {
            return Vec::new();
        };
        let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        let mut ranked: Vec<(i32, &Candidate)> =
            index.items.iter().filter(|c| c.kind == Kind::File).filter_map(|c| score(&query, c).map(|s| (s, c))).collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.search.len().cmp(&b.1.search.len())));
        ranked.into_iter().take(limit).map(|(_, c)| c).collect()
    }

    fn accept_mention(&mut self, pick: Option<usize>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((at, query)) = self.mention_query(window, cx) else { return false };
        let matches = self.mention_matches(&query);
        let Some(item) = matches.get(pick.unwrap_or(self.mention_selected).min(matches.len().saturating_sub(1))) else {
            return false;
        };
        let added = mention(&item.label, item.sent.clone(), match item.kind {
            Kind::Symbol(_) => Glyph::Braces.path(),
            Kind::Dir => icon_path(&item.path, true),
            Kind::File => icon_path(&item.path, false),
        });
        let added = fit(added, window);
        let token = format!("{} ", added.token);
        self.file_mentions.borrow_mut().push(added);
        self.composer.update(cx, |input, cx| {
            let text = input.value().to_string();
            let range = utf16(&text, at)..utf16(&text, input.cursor());
            input.replace_text_in_range(Some(range), &token, window, cx);
            input.focus(window, cx);
        });
        self.mention_dismissed = true;
        cx.notify();
        true
    }

    /// Backspace or Delete beside a chip takes the whole chip.
    fn delete_chip(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mentions = self.file_mentions.borrow().clone();
        self.composer.update(cx, |input, cx| {
            let Some(selection) = input.selected_text_range(false, window, cx) else { return false };
            if !selection.range.is_empty() {
                return false;
            }
            let text = input.value().to_string();
            let cursor = byte(&text, selection.range.start);
            let hit = tokens(&text, &mentions).into_iter().find(|r| {
                if back { cursor > r.start && cursor <= r.end } else { cursor >= r.start && cursor < r.end }
            });
            let Some(r) = hit else { return false };
            input.replace_text_in_range(Some(utf16(&text, r.start)..utf16(&text, r.end)), "", window, cx);
            true
        })
    }

    /// Arrow keys step over a chip in one go rather than through its padding.
    fn step_over_chip(&mut self, right: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mentions = self.file_mentions.borrow().clone();
        self.composer.update(cx, |input, cx| {
            let Some(selection) = input.selected_text_range(false, window, cx) else { return false };
            if !selection.range.is_empty() {
                return false;
            }
            let text = input.value().to_string();
            let cursor = byte(&text, selection.range.start);
            let hit = tokens(&text, &mentions).into_iter().find(|r| {
                if right { cursor >= r.start && cursor < r.end } else { cursor > r.start && cursor <= r.end }
            });
            let Some(r) = hit else { return false };
            let to = if right { r.end } else { r.start };
            input.set_cursor_position(super::chat::lsp_position(&text, to), window, cx);
            true
        })
    }

    /// Draws each mention as a chip: a border and the file's icon around the
    /// name the input draws. The input's text starts at its own top-left, so
    /// `bounds_for_range` places the chips exactly.
    pub(super) fn chip_overlay(&self, colors: Palette) -> impl IntoElement {
        let input = self.composer.clone();
        let mentions = self.file_mentions.clone();
        let border = Hsla::from(rgb(colors.border));
        let ink = Hsla::from(rgb(colors.muted));
        let fill = Hsla::from(rgb(colors.hover));
        let text_ink = Hsla::from(rgb(colors.text));
        let chip_font = font(mono());
        let chip_size = px(crate::theme::mono_size());
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let mentions = mentions.borrow().clone();
                if mentions.is_empty() {
                    return;
                }
                let text = input.read(cx).value().to_string();
                let origin = Bounds { origin: bounds.origin, size: Size::default() };
                let mut chips = Vec::new();
                input.update(cx, |state, cx| {
                    // Once the text overflows and scrolls, positions are no longer known.
                    let end = utf16(&text, text.len());
                    let overflowing = state
                        .bounds_for_range(end..end, origin, window, cx)
                        .is_none_or(|b| b.bottom() > bounds.bottom() + px(1.));
                    if overflowing {
                        return;
                    }
                    for range in tokens(&text, &mentions) {
                        let inner = range.start + MARKER.len()..range.end - MARKER.len();
                        let lead = inner.start..inner.start + LEAD.len();
                        // The token's markers and spaces are break points to the
                        // wrapper, so it can split across rows. A range's bounds
                        // only hold for one row, so a split chip is left as text.
                        let last = inner.end - text[..inner.end].chars().next_back().map_or(0, char::len_utf8);
                        let row_of = |ix: usize, state: &mut InputState, window: &mut Window, cx: &mut Context<InputState>| {
                            let at = utf16(&text, ix);
                            state.bounds_for_range(at..at, origin, window, cx).map(|b| b.top())
                        };
                        let first_row = row_of(inner.start, state, window, cx);
                        if first_row.is_none() || first_row != row_of(last, state, window, cx) {
                            continue;
                        }
                        let whole = state.bounds_for_range(utf16(&text, inner.start)..utf16(&text, inner.end), origin, window, cx);
                        let pad = state.bounds_for_range(utf16(&text, lead.start)..utf16(&text, lead.end), origin, window, cx);
                        let mention = mentions.iter().find(|m| text[range.clone()] == m.token);
                        if let (Some(whole), Some(pad), Some(mention)) = (whole, pad, mention) {
                            chips.push((whole, pad, mention.icon.clone(), mention.label.replace(' ', "\u{a0}")));
                        }
                    }
                });
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    for (whole, pad, icon, label) in chips {
                        let run = TextRun {
                            len: label.len(),
                            font: chip_font.clone(),
                            color: text_ink,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let line = window.text_system().shape_line(label.into(), chip_size, &[run], None);
                        // The token is padded to hold the mono name; should fonts have
                        // changed since, the chip still wraps the whole name.
                        let right = whole.right().max(pad.right() + line.width + px(6.));
                        let chip = Bounds::from_corners(whole.origin + point(px(0.), px(1.)), point(right, whole.bottom() - px(1.)));
                        // Paint over the input's proportional label, then redraw it in
                        // the configured mono face so composer and transcript chips match.
                        window.paint_quad(quad(chip, px(4.), fill, px(1.), border, BorderStyle::Solid));
                        let size = px(12.).min(pad.size.width - px(4.)).max(px(8.));
                        let at = point(pad.left() + (pad.size.width - size) / 2., chip.center().y - size / 2.);
                        let _ = window.paint_svg(Bounds { origin: at, size: Size { width: size, height: size } }, icon, TransformationMatrix::unit(), ink, cx);
                        let _ = line.paint(point(pad.right(), chip.top()), chip.size.height, window, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// The composer with chip editing and, while `@` is being typed, the list of
    /// files and symbols to mention above it.
    pub(super) fn with_mention_menu(&self, composer: Div, colors: Palette, window: &Window, cx: &mut Context<Self>) -> Div {
        let composer = composer
            .capture_action(cx.listener(|s, _: &Backspace, w, cx| {
                if s.delete_chip(true, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|s, _: &Delete, w, cx| {
                if s.delete_chip(false, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|s, _: &MoveLeft, w, cx| {
                if s.step_over_chip(false, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|s, _: &MoveRight, w, cx| {
                if s.step_over_chip(true, w, cx) {
                    cx.stop_propagation();
                }
            }));
        let Some((_, query)) = self.mention_query(window, cx) else {
            return composer;
        };
        let matches = self.mention_matches(&query);
        let active = self.mention_selected.min(matches.len().saturating_sub(1));
        let mut rows = col().id("mention-rows").max_h(px(260.)).overflow_y_scroll().track_scroll(&self.mention_scroll);
        if matches.is_empty() {
            rows = rows.child(
                row().h(px(28.)).px_2().gap_2().text_size(px(crate::theme::scaled_ui(12.))).text_color(rgb(colors.muted)).map(|s| {
                    if self.mention_index.as_ref().is_none_or(|i| i.project != self.current_project().path) {
                        s.child(spinner(colors)).child("Indexing files…")
                    } else {
                        s.child("No matching files or symbols")
                    }
                }),
            );
        }
        for (i, item) in matches.iter().enumerate() {
            let icon_el = match item.kind {
                Kind::Symbol(_) => icon(Glyph::Braces),
                Kind::Dir => icon(IconName::Folder),
                Kind::File => icon(gpui_component::Icon::empty().path(icon_path(&item.path, false))),
            };
            rows = rows.child(
                row()
                    .id(("mention", i))
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(i == active, |r| r.bg(rgb(colors.selected)))
                    .hover(|r| r.bg(rgb(colors.popover_hover)))
                    .child(icon_el.size(px(13.)).text_color(rgb(if matches!(item.kind, Kind::Symbol(_)) { colors.blue } else { colors.muted })))
                    .child(
                        line_with_detail(
                            item.label.clone(),
                            (!item.detail.is_empty()).then(|| (SharedString::from(item.detail.clone()), Hsla::from(rgb(colors.quiet)))),
                            18.,
                        )
                        .flex_1()
                        .min_w_0()
                        .when(matches!(item.kind, Kind::Symbol(_)), |s| s.font_family(mono()).text_size(px(crate::theme::mono_size()))),
                    )
                    .when_some(if let Kind::Symbol(kind) = item.kind { Some(kind) } else { None }, |s, kind| {
                        s.child(label(colors, kind).text_size(px(crate::theme::scaled_ui(11.))))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, _, window, cx| {
                            cx.stop_propagation();
                            s.accept_mention(Some(i), window, cx);
                        }),
                    ),
            );
        }
        let count = matches.len();
        let menu = col()
            .id("mentions")
            .occlude()
            .p_1()
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .text_color(rgb(colors.text))
            .child(rows);
        composer
            .relative()
            .capture_action(cx.listener(move |s, _: &MoveDown, _, cx| {
                if count > 0 {
                    s.mention_selected = (s.mention_selected.min(count - 1) + 1) % count;
                    s.mention_scroll.scroll_to_item(s.mention_selected);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .capture_action(cx.listener(move |s, _: &MoveUp, _, cx| {
                if count > 0 {
                    s.mention_selected = (s.mention_selected.min(count - 1) + count - 1) % count;
                    s.mention_scroll.scroll_to_item(s.mention_selected);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .capture_action(cx.listener(|s, action: &Enter, w, cx| {
                if !action.secondary && s.accept_mention(None, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|s, _: &IndentInline, w, cx| {
                if s.accept_mention(None, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|s, _: &Escape, _, cx| {
                s.mention_dismissed = true;
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                deferred(div().absolute().bottom_full().left_0().ml_2().mb_2().w_full().max_w(px(520.)).child(menu)).with_priority(1),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{byte, code_block, file_lines_mention, query_at, quote_mention, symbols, utf16};
    use std::path::Path;

    #[test]
    fn fences_outlast_backticks_inside() {
        assert_eq!(code_block("rs", "let a = 1;\n"), "```rs\nlet a = 1;\n```");
        assert_eq!(code_block("md", "```\nx\n```"), "````md\n```\nx\n```\n````");
    }

    #[test]
    fn file_lines_send_a_reference_and_the_code() {
        let m = file_lines_mention(Path::new("/p/src/a.rs"), Some(Path::new("/p")), 3, 4, "x\ny");
        assert_eq!(m.label, "a.rs:3-4");
        assert_eq!(m.reference, "@src/a.rs:3-4");
        assert_eq!(m.sent, "\n\n@src/a.rs:3-4\n```rs\nx\ny\n```\n\n");
    }

    #[test]
    fn quotes_keep_their_lines() {
        let m = quote_mention("first line\n\nsecond");
        assert_eq!(m.label, "“first line…”");
        assert_eq!(m.sent, "\n\n> first line\n>\n> second\n\n");
    }

    #[test]
    fn finds_the_query_being_typed() {
        assert_eq!(query_at("look at @src/ma", 15), Some((8, "src/ma")));
        assert_eq!(query_at("@", 1), Some((0, "")));
        assert_eq!(query_at("mail me@host", 12), None);
        assert_eq!(query_at("@done and more", 14), None);
    }

    #[test]
    fn finds_declarations() {
        let found = symbols("pub struct Workspace {\n    pub fn new() {}\nexport default function App() {}\nclass Foo:\n");
        let names: Vec<_> = found.iter().map(|(n, k, l)| (n.as_str(), *k, *l)).collect();
        assert_eq!(names, [("Workspace", "struct", 1), ("new", "fn", 2), ("App", "function", 3), ("Foo", "class", 4)]);
    }

    #[test]
    fn converts_offsets() {
        let text = "a😀b";
        assert_eq!(utf16(text, 5), 3);
        assert_eq!(byte(text, 3), 5);
    }
}
