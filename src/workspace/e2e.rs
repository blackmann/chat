//! The workspace driven as a person would drive it, against a real language
//! server: clicks, ⌘-hover, ⌘-click, keys and diffs, on a small Rust project
//! with tabs, a non-ASCII name and an uncommitted change. Runs only where
//! rust-analyzer is installed.
// Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
use super::panes::{LineKind, LineSource, diff_lines};
use super::{TabKind, Workspace, lsp};
use gpui::{AppContext as _, Context, Entity, Modifiers, MouseButton, MouseDownEvent, Pixels, Point, TestAppContext, VisualTestContext, point, px};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const COMMITTED_MAIN: &str = "mod util;\n\nfn main() {\n\tlet total = util::twice(21);\n\tlet unused = util::twice(1);\n\tlet café = total + 1;\n\tprintln!(\"{total} {café}\");\n}\n";
/// The working copy: `unused` removed, `again` added.
const MAIN: &str = "mod util;\n\nfn main() {\n\tlet total = util::twice(21);\n\tlet café = total + 1;\n\tlet again = util::twice(total);\n\tprintln!(\"{total} {café} {again}\");\n}\n";
const UTIL: &str = "/// Doubles a number.\npub fn twice(x: i32) -> i32 {\n    x * 2\n}\n";

fn sh(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new(args[0]).args(&args[1..]).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "{args:?} failed");
}

/// A Cargo project committed once, then edited, and a config folder that
/// opens it with the stand-in agent.
fn fixture() -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("chat-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("demo");
    std::fs::create_dir_all(project.join("src")).unwrap();
    // macOS reaches the temporary folder through a symlink; git and the
    // server report the real path, so the test uses it too.
    let project = std::fs::canonicalize(&project).unwrap();
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(project.join("src/main.rs"), COMMITTED_MAIN).unwrap();
    std::fs::write(project.join("src/util.rs"), UTIL).unwrap();
    std::fs::write(project.join("notes.go"), "package main\n\nfunc main() {}\n").unwrap();
    sh(&project, &["git", "init", "-q"]);
    sh(&project, &["git", "add", "."]);
    sh(&project, &["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    std::fs::write(project.join("src/main.rs"), MAIN).unwrap();

    let config = config_for(&base, &project);
    (project, config)
}

/// These tests share the settings folder's environment variable: one at a time.
static E2E: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Settings that open `project` with the stand-in agent.
fn config_for(base: &Path, project: &Path) -> PathBuf {
    let config = base.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let mock = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mock-agent.py");
    let settings = serde_json::json!({
        "projects": [project],
        "agents": [{ "name": "Mock", "command": "python3", "args": [mock] }],
    });
    std::fs::write(config.join("settings.json"), settings.to_string()).unwrap();
    config
}

/// The app, in a window, reading settings from `config`.
fn boot<'a>(cx: &'a mut TestAppContext, config: &Path) -> Driver<'a> {
    // SAFETY: tests that read settings from disk hold `E2E` while they do.
    unsafe { std::env::set_var("CHAT_CONFIG_DIR", config) };
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        cx.bind_keys(crate::key_bindings());
    });
    let slot: Rc<RefCell<Option<Entity<Workspace>>>> = Rc::default();
    let made = slot.clone();
    let (_root, vcx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| Workspace::new(window, cx));
        *made.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let ws = slot.borrow().clone().unwrap();
    Driver { cx: vcx, ws }
}

struct Driver<'a> {
    cx: &'a mut VisualTestContext,
    ws: Entity<Workspace>,
}

impl Driver<'_> {
    fn read<R>(&mut self, f: impl FnOnce(&Workspace) -> R) -> R {
        let ws = self.ws.clone();
        self.cx.update(|_, cx| f(ws.read(cx)))
    }

    fn act(&mut self, f: impl FnOnce(&mut Workspace, &mut Context<Workspace>)) {
        let ws = self.ws.clone();
        self.cx.update(|_, cx| ws.update(cx, f));
        self.settle();
    }

    /// Let queued work run and draw a fresh frame.
    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// Wait for a server or a thread to get somewhere.
    fn wait(&mut self, what: &str, secs: u64, mut done: impl FnMut(&mut Self) -> bool) {
        let start = Instant::now();
        loop {
            self.settle();
            if done(self) {
                return;
            }
            assert!(start.elapsed().as_secs() < secs, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn active(&mut self) -> TabKind {
        self.read(|s| s.tabs[s.active].kind.clone())
    }

    /// The middle of character `col` (counted in characters, as shown) of a
    /// pane's row.
    fn point(&mut self, row: usize, col: usize) -> Point<Pixels> {
        let key: &'static str = Box::leak(format!("code:{row}").into_boxed_str());
        let bounds = self.cx.debug_bounds(key).unwrap_or_else(|| panic!("row {row} isn't drawn"));
        let glyph = self.cx.update(|window, _| super::panes::glyph_width(window));
        point(bounds.left() + glyph * (col as f32 + 0.5), bounds.center().y)
    }

    fn click(&mut self, row: usize, col: usize, modifiers: Modifiers) {
        let at = self.point(row, col);
        self.cx.simulate_mouse_move(at, None, modifiers);
        self.cx.simulate_click(at, modifiers);
        self.settle();
    }

    fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.settle();
    }

    fn picked(&mut self) -> Option<(String, usize)> {
        self.read(|s| s.symbol.as_ref().map(|m| (m.word.to_string(), m.row)))
    }

    /// The server's ranges for the picked symbol, as the text each covers, by row.
    fn server_marks(&mut self) -> Option<Vec<(usize, String, bool)>> {
        self.read(|s| {
            let mark = s.symbol.as_ref()?;
            let server = mark.server.as_ref()?;
            let mut out = Vec::new();
            for (row, ranges) in &server.rows {
                let line = s.pane_line(&mark.source, *row)?;
                for (r, write) in ranges {
                    out.push((*row, line.get(r.clone()).unwrap_or("<out of range>").to_string(), *write));
                }
            }
            out.sort();
            Some(out)
        })
    }

    /// The row of a diff pane whose text contains `needle` and is of `kind`.
    fn diff_row(&mut self, source: &LineSource, kind: LineKind, needle: &str) -> usize {
        let source = source.clone();
        let needle = needle.to_string();
        self.read(move |s| {
            let (old, new) = s.diff_sides(&source).expect("diff loaded");
            diff_lines(&old, &new).iter().position(|l| l.kind == kind && l.text.contains(&needle)).expect("row in diff")
        })
    }
}

fn cmd() -> Modifiers {
    Modifiers::command()
}

#[gpui::test]
fn drives_code_review_with_rust_analyzer(cx: &mut TestAppContext) {
    let env = crate::agent::shell_path();
    let Some(ra) = crate::lsp::which("rust-analyzer", env.as_deref()).filter(|p| !crate::lsp::proxy_missing(p)) else {
        eprintln!("rust-analyzer isn't installed; skipping");
        return;
    };
    eprintln!("using {}", ra.display());
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    let main_rs = project.join("src/main.rs");
    let util_rs = project.join("src/util.rs");
    let main_src = LineSource::File(main_rs.clone());

    // ── Opening a file starts the server; the header follows it. ──────────────
    d.act(|s, cx| s.open(TabKind::File(main_rs.clone()), cx));
    let first = d.read(|s| s.lsp_badge(&main_rs).map(|b| b.text.to_string()));
    eprintln!("badge on open: {first:?}");
    assert!(first.as_deref().is_some_and(|b| b.starts_with("rust-analyzer")), "{first:?}");
    let mut seen = Vec::new();
    d.wait("rust-analyzer to be ready", 240, |d| {
        let badge = d.read(|s| s.lsp_badge(&main_rs).map(|b| (b.text.to_string(), b.warn)));
        if let Some((text, warn)) = &badge {
            assert!(!warn, "server trouble: {text}");
            if seen.last() != Some(text) {
                seen.push(text.clone());
            }
        }
        badge.is_some_and(|(t, _)| t == "rust-analyzer")
    });
    eprintln!("badge went through: {seen:?}");

    // ── Plain click picks a word; the text match shows at once. ───────────────
    // Row 3 is `\tlet total = util::twice(21);`, shown as four spaces.
    d.click(3, 8, Modifiers::none());
    assert_eq!(d.picked(), Some(("total".into(), 3)));
    // A click on a space lets it go.
    d.click(3, 2, Modifiers::none());
    assert_eq!(d.picked(), None);

    // The server's highlights replace the text match, once indexing allows.
    let mut marks = None;
    d.wait("highlights for `total`", 120, |d| {
        if d.picked().is_none() {
            d.click(3, 8, Modifiers::none());
        }
        marks = d.server_marks();
        if marks.is_none() {
            // Indexing may still be under way; pick again later.
            std::thread::sleep(Duration::from_millis(300));
            d.act(|s, _| s.symbol = None);
        }
        marks.is_some()
    });
    let marks = marks.unwrap();
    eprintln!("`total` highlights: {marks:?}");
    assert!(marks.iter().all(|(_, text, _)| text == "total"), "{marks:?}");
    let rows: Vec<usize> = marks.iter().map(|m| m.0).collect();
    for row in [3, 4, 5, 6] {
        assert!(rows.contains(&row), "no highlight on row {row}: {marks:?}");
    }

    // Tabs before it, two bytes in `é`: the ranges still land on `café`.
    d.click(4, 8, Modifiers::none());
    assert_eq!(d.picked(), Some(("café".into(), 4)));
    d.wait("highlights for `café`", 30, |d| d.server_marks().is_some());
    let marks = d.server_marks().unwrap();
    eprintln!("`café` highlights: {marks:?}");
    assert!(marks.iter().all(|(_, text, _)| text == "café"), "{marks:?}");
    assert!(marks.iter().any(|m| m.0 == 4) && marks.iter().any(|m| m.0 == 6), "{marks:?}");

    // Escape lets the pick go.
    d.keys("escape");
    assert_eq!(d.picked(), None);

    // ── ⌘ over a word makes it a link; letting go takes it away. ─────────────
    let twice = d.point(3, 22);
    d.cx.simulate_mouse_move(twice, None, cmd());
    d.settle();
    let link = d.read(|s| s.servers.link.clone());
    assert!(link.as_ref().is_some_and(|l| l.source == main_src && l.row == 3 && l.range == (22..27)), "link: {:?}", link.map(|l| l.range));
    d.cx.simulate_modifiers_change(Modifiers::none());
    d.settle();
    assert!(d.read(|s| s.servers.link.is_none()));
    // Pressing ⌘ while resting on a word links it without moving.
    d.cx.simulate_mouse_move(twice, None, Modifiers::none());
    d.settle();
    d.cx.simulate_modifiers_change(cmd());
    d.settle();
    assert!(d.read(|s| s.servers.link.is_some()), "⌘ pressed in place didn't link");
    d.cx.simulate_modifiers_change(Modifiers::none());
    d.settle();

    // ── ⌘-click goes to the definition, in another file. ─────────────────────
    d.click(3, 22, cmd());
    d.wait("the jump to util.rs", 30, |d| d.active() == TabKind::File(util_rs.clone()));
    assert_eq!(d.picked(), Some(("twice".into(), 1)), "the symbol landed on is picked");
    assert_eq!(d.read(|s| s.servers.back.len()), 1);

    // ── Back and forward. ────────────────────────────────────────────────────
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::File(main_rs.clone()), "⌃- goes back");
    d.keys("ctrl-shift--");
    assert_eq!(d.active(), TabKind::File(util_rs.clone()), "⌃⇧- goes forward");
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::File(main_rs.clone()));

    // ── A library's source: a long path outside the project. ────────────────
    // In a narrow window its header path gives way to the labels beside it.
    d.cx.simulate_resize(gpui::size(px(1000.), px(700.)));
    d.settle();
    // `\tprintln!(…)` on row 6: `println` at character 4.
    d.click(6, 4, cmd());
    d.wait("a jump into the standard library", 60, |d| matches!(d.active(), TabKind::File(p) if !p.starts_with(&project)));
    let shown = d.read(|s| match &s.tabs[s.active].kind {
        TabKind::File(p) => s.relative(p),
        _ => String::new(),
    });
    eprintln!("library header: {shown}");
    assert!(shown.starts_with("~/") || !shown.starts_with("/Users/"), "shown from home: {shown}");
    let (path, trailing) = (d.cx.debug_bounds("pane-path").unwrap(), d.cx.debug_bounds("pane-trailing").unwrap());
    eprintln!("path {path:?} / labels {trailing:?}");
    assert!(path.right() <= trailing.left(), "the path runs under the labels");
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::File(main_rs.clone()));
    d.cx.simulate_resize(gpui::size(px(1600.), px(1000.)));
    d.settle();

    // ── F12 on the picked symbol. ────────────────────────────────────────────
    d.click(5, 22, Modifiers::none());
    assert_eq!(d.picked(), Some(("twice".into(), 5)));
    d.keys("f12");
    d.wait("F12 to util.rs", 30, |d| d.active() == TabKind::File(util_rs.clone()));

    // ── Hover shows the doc comment. ─────────────────────────────────────────
    // util.rs row 1: `pub fn twice(x: i32) -> i32 {`
    let at = d.point(1, 8);
    d.cx.simulate_mouse_move(at, None, Modifiers::none());
    d.settle();
    assert!(d.read(|s| s.servers.hover.is_some()), "resting on a word starts a hover");
    d.cx.executor().advance_clock(lsp::HOVER_DELAY + Duration::from_millis(50));
    d.wait("the hover card", 30, |d| d.read(|s| s.servers.hover.as_ref().is_some_and(|h| h.text.is_some())));
    let text = d.read(|s| s.servers.hover.as_ref().and_then(|h| h.text.clone())).unwrap();
    eprintln!("hover: {text:?}");
    assert!(text.contains("Doubles a number") && text.contains("fn twice"), "{text}");
    // The pointer can go into the card, and it stays while it's there.
    let card = d.cx.debug_bounds("lsp-hover").expect("the card is drawn");
    d.cx.simulate_mouse_move(card.center(), None, Modifiers::none());
    d.settle();
    d.cx.executor().advance_clock(lsp::CARD_GRACE * 2);
    d.settle();
    assert!(d.read(|s| s.servers.in_card && s.servers.hover.is_some()), "the card stays with the pointer in it");
    // Moving off it, onto the space after `pub`, takes it away after a moment.
    let off = d.point(1, 3);
    d.cx.simulate_mouse_move(off, None, Modifiers::none());
    d.settle();
    assert!(d.read(|s| s.servers.hover.is_some()), "the card waits for the pointer to come back");
    d.cx.executor().advance_clock(lsp::CARD_GRACE + Duration::from_millis(50));
    d.settle();
    assert!(d.read(|s| s.servers.hover.is_none()));

    // ── ⌘⇧-click lists references; Escape closes the list. ──────────────────
    d.click(1, 8, Modifiers::command_shift());
    d.wait("the references list", 30, |d| d.read(|s| s.popup.is_some()));
    d.keys("escape");
    assert!(d.read(|s| s.popup.is_none()), "Escape closes the list");
    // Each entry, picked by keyboard, goes to a use of `twice` and can be
    // gone back from. The order is the server's.
    let mut landed = Vec::new();
    for k in 1..=3 {
        d.act(|s, cx| s.open(TabKind::File(util_rs.clone()), cx));
        d.click(1, 8, Modifiers::command_shift());
        d.wait("the references list again", 30, |d| d.read(|s| s.popup.is_some()));
        d.act(|s, _| s.servers.at = None);
        d.keys(&format!("{}enter", "down ".repeat(k)));
        assert!(d.read(|s| s.popup.is_none()), "Enter picks and closes");
        d.wait("a jump from the list", 30, |d| d.read(|s| s.servers.at.is_some()));
        let tab = d.active();
        let (word, row) = d.picked().expect("the reference is picked");
        let src = match &tab {
            TabKind::File(p) => LineSource::File(p.clone()),
            other => panic!("landed on {other:?}"),
        };
        let line = d.read(|s| s.pane_line(&src, row)).unwrap();
        assert_eq!(word, "twice");
        assert!(line.contains("twice"), "{line}");
        landed.push((tab, row));
    }
    eprintln!("references landed on: {landed:?}");
    let mut places = landed.clone();
    places.sort_by_key(|(t, r)| (format!("{t:?}"), *r));
    places.dedup();
    assert_eq!(places.len(), 3, "three different places: {landed:?}");
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::File(util_rs.clone()), "back from a reference");

    // ── A Git diff: added lines go through the server, removed lines by name. ─
    d.act(|s, cx| s.open(TabKind::GitDiff(main_rs.clone(), false), cx));
    let diff = LineSource::GitDiff(main_rs.clone(), false);
    d.wait("the Git diff", 30, |d| d.read(|s| s.diff_sides(&diff).is_some()));
    let added = d.diff_row(&diff, LineKind::Add, "again");
    let removed = d.diff_row(&diff, LineKind::Del, "unused");
    // `\tlet again = util::twice(total);`: `twice` at 22, `total` at 28.
    d.click(added, 28, Modifiers::none());
    assert_eq!(d.picked(), Some(("total".into(), added)));
    d.wait("highlights in the diff", 30, |d| d.server_marks().is_some());
    let marks = d.server_marks().unwrap();
    eprintln!("diff highlights: {marks:?}");
    assert!(marks.iter().all(|(_, text, _)| text == "total"), "{marks:?}");
    assert!(marks.iter().any(|m| m.0 == added), "{marks:?}");
    let unmapped = d.read(|s| s.symbol.as_ref().and_then(|m| m.server.as_ref()).is_some_and(|m| m.unmapped.contains(&removed)));
    assert!(unmapped, "the removed row falls back to the text match");

    d.click(added, 22, cmd());
    d.wait("a jump from the diff's added line", 30, |d| d.active() == TabKind::File(util_rs.clone()));
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::GitDiff(main_rs.clone(), false), "back to the diff");
    // `\tlet unused = util::twice(1);` is gone from the file: found by name.
    // (`twice` is at 23 here: `unused` is a character longer than `total`.)
    d.click(removed, 23, cmd());
    d.wait("a jump from the diff's removed line", 30, |d| d.active() == TabKind::File(util_rs.clone()));
    assert_eq!(d.picked().map(|p| p.0), Some("twice".into()));

    // ── An edit on disk reaches the pane and the server. ─────────────────────
    std::fs::write(&util_rs, format!("// moved down a line\n{UTIL}")).unwrap();
    d.act(|s, cx| s.refresh_git(cx));
    let (shown, sent) = d.read(|s| {
        let shown = match s.files.get(&util_rs) {
            Some(Ok(f)) => f.lines.first().map(|l| l.to_string()),
            _ => None,
        };
        let sent = s.servers.all.values().find_map(|srv| srv.text(&util_rs).map(str::to_string));
        (shown, sent)
    });
    assert_eq!(shown.as_deref(), Some("// moved down a line"));
    assert!(sent.is_some_and(|t| t.starts_with("// moved down a line")));
    d.act(|s, cx| s.open(TabKind::File(main_rs.clone()), cx));
    d.click(3, 22, cmd());
    d.wait("a jump to the moved definition", 30, |d| d.active() == TabKind::File(util_rs.clone()) && d.picked() == Some(("twice".into(), 2)));

    // ── The bolt in the status bar: logs, memory, restart and stop. ─────────
    let ra = |s: &Workspace| s.servers.all.iter().find(|(_, srv)| srv.spec.name == "rust-analyzer").map(|(id, srv)| (*id, srv.status.clone()));
    let open_menu = |d: &mut Driver| {
        // A click on a trigger just after its menu closed counts as closing it;
        // a person's next click comes later than that.
        std::thread::sleep(Duration::from_millis(450));
        let bolt = d.cx.debug_bounds("servers-button").expect("the bolt shows once a server runs");
        d.cx.simulate_click(bolt.center(), Modifiers::none());
        d.settle();
        assert_eq!(d.read(|s| s.popup_anchor), Some(super::servers::SERVERS), "the bolt opens the servers menu");
    };
    // Memory is read as soon as the server runs.
    d.wait("a memory reading", 30, |d| d.read(|s| s.servers.all.values().any(|srv| srv.memory.is_some_and(|m| m > 1024 * 1024))));
    let ws = d.ws.clone();
    let line = d.cx.update(|_, cx| {
        let colors = crate::theme::palette(cx);
        ws.read(cx).servers.all.values().find(|srv| srv.spec.name == "rust-analyzer").map(|srv| super::servers::status_line(srv, colors).text)
    });
    eprintln!("status line: {line:?}");
    assert!(line.as_deref().is_some_and(|l| l.starts_with("Running — ") && l.ends_with(" MB")), "{line:?}");

    // Down to rust-analyzer, → into its submenu, Enter on View Logs.
    open_menu(&mut d);
    d.keys("down right enter");
    assert!(d.read(|s| s.popup.is_none()), "picking in the submenu closes the menu");
    assert_eq!(d.active(), TabKind::ServerLog("rust-analyzer".into(), project.clone()));
    let logged = d.read(|s| s.servers.logs.get(&("rust-analyzer".to_string(), project.clone())).map(|l| l.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    assert!(logged.iter().any(|l| l.contains("started ")), "the start is logged");
    assert!(logged.iter().any(|l| l.contains("→ initialize #")), "requests are logged");
    assert!(logged.iter().any(|l| l.contains("← reply #")), "replies are logged");
    // It opens at the newest line.
    let last: &'static str = Box::leak(format!("log-line:{}", logged.len() - 1).into_boxed_str());
    assert!(d.cx.debug_bounds(last).is_some(), "the log tab opens at its last line");

    // ← backs out of a submenu without closing the menu; Escape then closes it.
    open_menu(&mut d);
    d.keys("down right left");
    assert!(d.read(|s| s.popup.is_some()), "← leaves the menu open");
    d.keys("escape");
    assert!(d.read(|s| s.popup.is_none()));

    // Restart Server: a fresh process, with the files the old one had.
    let (before, _) = d.read(ra).unwrap();
    open_menu(&mut d);
    d.keys("down right down enter");
    let (after, _) = d.read(ra).unwrap();
    assert_ne!(before, after, "restarted as a new server");
    assert!(d.read(|s| s.servers.all[&after].has(&main_rs)), "it has the open files");
    d.wait("the restarted server", 240, |d| d.read(|s| s.lsp_badge(&main_rs).is_some_and(|b| b.text == "rust-analyzer")));
    d.act(|s, cx| s.open(TabKind::File(main_rs.clone()), cx));
    d.click(3, 22, cmd());
    d.wait("a jump with the restarted server", 30, |d| d.active() == TabKind::File(util_rs.clone()));

    // Stop Server: it stays stopped, and says so, even as files open.
    open_menu(&mut d);
    d.keys("down right down down enter");
    assert_eq!(d.read(ra).map(|r| r.1), Some(crate::lsp::Status::Stopped));
    d.act(|s, cx| s.open(TabKind::File(main_rs.clone()), cx));
    assert_eq!(d.read(ra).map(|r| r.1), Some(crate::lsp::Status::Stopped), "opening a file doesn't start it again");
    assert_eq!(d.read(|s| s.servers.all.len()), 1);
    assert_eq!(d.read(|s| s.lsp_badge(&main_rs).map(|b| b.text.to_string())).as_deref(), Some("rust-analyzer stopped"));
    d.click(3, 22, cmd());
    assert_eq!(d.read(|s| s.servers.notice.as_ref().map(|n| n.0.to_string())).as_deref(), Some("rust-analyzer stopped"));

    // Restart All Servers, the last item that can be picked while all are stopped.
    open_menu(&mut d);
    d.keys("up enter");
    d.wait("everything restarted", 240, |d| d.read(|s| s.lsp_badge(&main_rs).is_some_and(|b| b.text == "rust-analyzer")));
    // Memory keeps being read while it runs: once loaded, it's well past what it started at.
    d.cx.executor().advance_clock(Duration::from_secs(3));
    d.wait("a later memory reading", 30, |d| d.read(|s| s.servers.all.values().any(|srv| srv.memory.is_some_and(|m| m > 20 * 1024 * 1024))));
    eprintln!("memory once loaded: {:?}", d.read(|s| s.servers.all.values().filter_map(|srv| srv.memory).map(super::servers::bytes).collect::<Vec<_>>()));

    // ── No server installed: the header says so, and so does ⌘-click. ───────
    let go = project.join("notes.go");
    d.act(|s, cx| s.open(TabKind::File(go.clone()), cx));
    let badge = d.read(|s| s.lsp_badge(&go).map(|b| (b.text.to_string(), b.warn, b.tooltip.map(|t| t.to_string()))));
    eprintln!("go badge: {badge:?}");
    if crate::lsp::which("gopls", env.as_deref()).is_none() {
        let (text, warn, tip) = badge.expect("a badge for Go");
        assert!(warn && text == "gopls not found", "{text}");
        assert!(tip.is_some_and(|t| t.contains("go install")));
        d.click(2, 6, cmd());
        let notice = d.read(|s| s.servers.notice.as_ref().map(|n| n.0.to_string()));
        assert_eq!(notice.as_deref(), Some("gopls not found"));
    }

    // ── Closing every tab lets the server go after a while. ─────────────────
    d.act(|s, cx| {
        while s.tabs.len() > 1 {
            let last = s.tabs.len() - 1;
            s.close(last, cx);
        }
    });
    assert!(d.read(|s| s.servers.all.values().all(|srv| srv.documents().next().is_none())));
    d.cx.executor().advance_clock(lsp::IDLE + Duration::from_secs(1));
    d.settle();
    assert!(d.read(|s| s.servers.all.is_empty()), "idle servers are stopped");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// TypeScript and JSON, installed on first use into a fresh settings folder,
/// then driven like the Rust test. Fetches from npm, so it runs on request:
/// `cargo test installs_and_drives -- --ignored --nocapture`.
#[gpui::test]
#[ignore = "installs language servers from npm"]
fn installs_and_drives_typescript_and_json(cx: &mut TestAppContext) {
    let env = crate::agent::shell_path();
    if crate::lsp::which("npm", env.as_deref()).is_none() {
        eprintln!("npm isn't installed; skipping");
        return;
    }
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("chat-e2e-ts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("web");
    std::fs::create_dir_all(project.join("src")).unwrap();
    let project = std::fs::canonicalize(&project).unwrap();
    let files = [
        ("package.json", "{\n  \"name\": \"web\",\n  \"version\": \"1.0.0\",\n  \"private\": true\n}\n"),
        ("tsconfig.json", "{\n  \"compilerOptions\": { \"strict\": true, \"target\": \"es2020\", \"module\": \"esnext\", \"moduleResolution\": \"bundler\" },\n  \"include\": [\"src\"]\n}\n"),
        ("src/util.ts", "/** Doubles a number. */\nexport function twice(x: number): number {\n\treturn x * 2;\n}\n"),
        ("src/main.ts", "import { twice } from \"./util\";\n\nconst café = twice(21);\nconsole.log(café, twice(café));\n"),
        ("styles.css", ":root { --accent: red; }\nbody { color: var(--accent); }\n"),
    ];
    for (name, text) in files {
        std::fs::write(project.join(name), text).unwrap();
    }
    let config = config_for(&base, &project);
    let mut d = boot(cx, &config);
    let main_ts = project.join("src/main.ts");
    let util_ts = project.join("src/util.ts");
    let badge = |d: &mut Driver, path: &Path| d.read(|s| s.lsp_badge(path).map(|b| b.text.to_string()));

    // ── First use installs it; the header says so, then it runs. ─────────────
    d.act(|s, cx| s.open(TabKind::File(main_ts.clone()), cx));
    let first = badge(&mut d, &main_ts);
    eprintln!("badge on open: {first:?}");
    let on_path = crate::lsp::which("typescript-language-server", env.as_deref()).is_some();
    eprintln!("typescript-language-server on PATH: {on_path}");
    // The project has no TypeScript of its own, so Chat installs one either way.
    assert_eq!(first.as_deref(), Some("typescript-language-server installing…"));
    let started = Instant::now();
    let mut seen = Vec::new();
    d.wait("typescript-language-server to install and start", 300, |d| {
        let b = badge(d, &main_ts);
        if let Some(b) = &b {
            assert!(!b.ends_with("crashed") && !b.ends_with("not found"), "{b}: {:?}", d.read(|s| s.servers.logs.values().flatten().map(|l| l.to_string()).collect::<Vec<_>>()));
            if seen.last() != Some(b) {
                seen.push(b.clone());
            }
        }
        b.as_deref() == Some("typescript-language-server")
    });
    eprintln!("badge went through {seen:?} in {:.1}s", started.elapsed().as_secs_f32());
    let bin = config.join("servers/typescript-language-server/node_modules/.bin/typescript-language-server");
    assert!(bin.exists(), "installed under Chat's settings folder");
    let key = ("typescript-language-server".to_string(), project.clone());
    let log = d.read(|s| s.servers.logs.get(&key).map(|l| l.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    assert!(log.iter().any(|l| l.contains("installed ")), "the install is logged");

    // ── ⌘-click across files: tsserver may still be loading the project. ────
    // `const café = twice(21);`: `twice` at character 13.
    let start = Instant::now();
    loop {
        d.click(2, 13, cmd());
        let jumped = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            d.settle();
            d.active() == TabKind::File(util_ts.clone())
        });
        if jumped {
            break;
        }
        assert!(start.elapsed().as_secs() < 90, "no jump to util.ts: {:?}", d.read(|s| s.servers.notice.as_ref().map(|n| n.0.to_string())));
    }
    assert_eq!(d.picked(), Some(("twice".into(), 1)), "lands on the definition");
    d.keys("ctrl--");
    assert_eq!(d.active(), TabKind::File(main_ts.clone()));

    // ── Highlights count in UTF-16 here: `é` must still land on `café`. ──────
    d.click(2, 6, Modifiers::none());
    assert_eq!(d.picked(), Some(("café".into(), 2)));
    d.wait("highlights for `café`", 60, |d| d.server_marks().is_some());
    let marks = d.server_marks().unwrap();
    eprintln!("`café` highlights: {marks:?}");
    assert!(marks.iter().all(|(_, text, _)| text == "café"), "{marks:?}");
    let rows: Vec<usize> = marks.iter().map(|m| m.0).collect();
    assert!(rows.contains(&2) && rows.contains(&3), "{marks:?}");
    // After `café` on row 3, `twice(café)` still lines up.
    assert_eq!(marks.iter().filter(|m| m.0 == 3).count(), 2, "{marks:?}");

    // ── Hover: the doc comment and the signature. ───────────────────────────
    d.act(|s, cx| s.open(TabKind::File(util_ts.clone()), cx));
    let at = d.point(1, 17);
    d.cx.simulate_mouse_move(at, None, Modifiers::none());
    d.settle();
    d.cx.executor().advance_clock(lsp::HOVER_DELAY + Duration::from_millis(50));
    d.wait("the hover card", 30, |d| d.read(|s| s.servers.hover.as_ref().is_some_and(|h| h.text.is_some())));
    let text = d.read(|s| s.servers.hover.as_ref().and_then(|h| h.text.clone())).unwrap();
    eprintln!("hover: {text:?}");
    assert!(text.contains("Doubles a number") && text.contains("function twice"), "{text}");
    let parts = super::lsp::hover_parts(&text);
    assert!(matches!(parts.first(), Some(super::lsp::HoverPart::Code { lang, .. }) if lang == "typescript"), "{parts:?}");

    // ── JSON installs its package; CSS shares it and starts at once. ─────────
    let package = project.join("package.json");
    d.act(|s, cx| s.open(TabKind::File(package.clone()), cx));
    assert_eq!(badge(&mut d, &package).as_deref(), Some("vscode-json-language-server installing…"));
    d.wait("the JSON server", 300, |d| badge(d, &package).as_deref() == Some("vscode-json-language-server"));
    let css = project.join("styles.css");
    d.act(|s, cx| s.open(TabKind::File(css.clone()), cx));
    d.wait("the CSS server", 60, |d| badge(d, &css).as_deref() == Some("vscode-css-language-server"));
    let css_log = d.read(|s| s.servers.logs.get(&("vscode-css-language-server".to_string(), project.clone())).map(|l| l.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    eprintln!("css log: {:?}", css_log.iter().take(4).collect::<Vec<_>>());
    assert!(!css_log.iter().any(|l| l.contains("installing vscode-css-language-server into")), "CSS reuses the JSON install");
    // ⌘-click a CSS variable to where it's declared.
    // `body { color: var(--accent); }`: `accent` at character 22.
    let start = Instant::now();
    loop {
        d.click(1, 22, cmd());
        let landed = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            d.settle();
            d.picked().is_some_and(|(_, row)| row == 0)
        });
        if landed {
            break;
        }
        assert!(start.elapsed().as_secs() < 30, "no jump to the declaration: {:?}", d.read(|s| s.servers.notice.as_ref().map(|n| n.0.to_string())));
    }

    // Chat's own install can be updated from the menu.
    let managed = d.read(|s| s.servers.all.values().filter(|srv| crate::lsp::install::managed(&srv.spec, env.as_deref())).map(|srv| srv.spec.name.clone()).collect::<Vec<_>>());
    eprintln!("managed by Chat: {managed:?}");
    assert!(managed.contains(&"typescript-language-server".to_string()));
    let _ = std::fs::remove_dir_all(&base);
}

/// The terminal's scrollbar, through its real mouse handlers: dragging the
/// thumb, pressing the track, and the wheel bringing it back.
#[gpui::test]
fn scrolls_the_terminal_with_its_bar(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    let cwd = project.clone();
    d.act(move |s, cx| s.open(TabKind::Terminal(cwd), cx));
    let term = d.read(|s| match s.terminals.values().next() {
        Some(Ok(t)) => t.clone(),
        _ => panic!("no terminal"),
    });
    let state = |d: &mut Driver| d.cx.update(|_, cx| term.read(cx).scroll_state());
    // Enough output to scroll back through.
    d.cx.update(|_, cx| term.update(cx, |t, cx| t.type_text("seq 1 500\n", cx)));
    d.wait("output with history", 30, |d| state(d).1 > 300);
    let (offset, history) = state(&mut d);
    assert_eq!(offset, 0, "it follows the output");
    let bar = d.cx.debug_bounds("terminal-bar").expect("a bar once there's history");
    eprintln!("bar {bar:?}, history {history}");

    // The thumb starts at the bottom; drag it to the top.
    let low = point(bar.center().x, bar.bottom() - px(4.));
    d.cx.simulate_mouse_move(low, None, Modifiers::none());
    d.cx.simulate_mouse_down(low, gpui::MouseButton::Left, Modifiers::none());
    let high = point(bar.center().x, bar.top() - px(200.));
    d.cx.simulate_mouse_move(high, MouseButton::Left, Modifiers::none());
    d.cx.simulate_mouse_up(high, gpui::MouseButton::Left, Modifiers::none());
    d.settle();
    assert_eq!(state(&mut d).0, history, "dragged to the top of the history");

    // Pressing the track's middle goes about halfway.
    d.cx.simulate_click(bar.center(), Modifiers::none());
    d.settle();
    let middle = state(&mut d).0 as f32 / history as f32;
    assert!((0.35..0.65).contains(&middle), "pressing the middle went to {middle}");

    // Presses on the bar aren't the start of a selection in the terminal.
    assert!(!d.cx.update(|_, cx| term.read(cx).has_selection()), "a press on the bar selected text");
    // The wheel still scrolls, down to the bottom.
    d.cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(bar.left() - px(100.), bar.center().y),
        delta: gpui::ScrollDelta::Lines(point(0., -10_000.)),
        ..Default::default()
    });
    d.settle();
    assert_eq!(state(&mut d).0, 0, "the wheel goes back to the bottom");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// Each folder of a project gets its own terminal tab, and closing one leaves
/// the other's shell running.
#[gpui::test]
fn keeps_a_terminal_per_folder(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let other = project.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mut d = boot(cx, &config);
    let (a, b) = (project.clone(), other.clone());
    d.act(move |s, cx| {
        s.open(TabKind::Terminal(a.clone()), cx);
        s.open(TabKind::Terminal(b), cx);
        // Asking for the first folder again goes back to its tab.
        s.open(TabKind::Terminal(a), cx);
    });
    let terminals = |s: &Workspace| s.tabs.iter().filter(|t| matches!(t.kind, TabKind::Terminal(_))).count();
    assert_eq!(d.read(terminals), 2);
    assert_eq!(d.read(|s| s.terminals.len()), 2);
    let shown = project.clone();
    assert!(d.read(move |s| s.tabs[s.active].kind == TabKind::Terminal(shown)));
    d.act(|s, cx| s.close(s.active, cx));
    assert_eq!(d.read(terminals), 1);
    let kept = other.clone();
    assert!(d.read(move |s| s.terminals.contains_key(&kept) && s.terminals.len() == 1));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// Open with: the file header's button lists the editors, with their icons
/// and Reveal in Finder; a right-click in the Files page offers them in a
/// submenu, leaving out those that don't open folders when it's a folder.
#[gpui::test]
fn offers_other_editors(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    // Stand-ins for whatever this Mac has installed, put in once the real
    // lookup at startup is done.
    let fake = |name: &str, icon: bool, folders: bool| crate::editors::Editor {
        name: name.into(),
        app: PathBuf::from(format!("/nonexistent/{name}.app")),
        icon: icon.then(|| PathBuf::from("/nonexistent/icon.png")),
        folders,
    };
    let editors = crate::editors::Editors { list: vec![fake("Coder", true, true), fake("Jotter", false, false)], finder: None };
    let file = project.join("src/main.rs");
    let opened = file.clone();
    d.act(move |s, cx| {
        s.editors = Rc::new(editors);
        s.side = super::Side::Files;
        s.open(TabKind::File(opened), cx);
    });
    let labels = |d: &mut Driver| {
        let ws = d.ws.clone();
        d.cx.update(|_, cx| ws.read(cx).popup.clone().map(|m| m.read(cx).labels(cx)))
    };

    // The header's button, right above the file.
    let button = d.cx.debug_bounds(super::panes::OPEN_WITH).expect("an Open with button in the file's header");
    d.cx.simulate_click(button.center(), Modifiers::none());
    d.settle();
    assert_eq!(d.read(|s| s.popup_anchor), Some(super::panes::OPEN_WITH));
    let (items, _) = labels(&mut d).expect("a list of editors");
    assert_eq!(items, ["Coder [img]", "Jotter", "Reveal in Finder"]);
    d.keys("escape");
    assert!(d.read(|s| s.popup.is_none()));

    // A right-click on a file: Open with ▸ its editors, Reveal in Finder, and
    // since the project is a Git repository, its links.
    let right_click = |d: &mut Driver, name: &str| {
        let key: &'static str = Box::leak(format!("file-row:{name}").into_boxed_str());
        let row = d.cx.debug_bounds(key).unwrap_or_else(|| panic!("{name} isn't in the Files page"));
        d.cx.simulate_mouse_down(row.center(), gpui::MouseButton::Right, Modifiers::none());
        d.cx.simulate_mouse_up(row.center(), gpui::MouseButton::Right, Modifiers::none());
        d.settle();
        row.center()
    };
    // The menu's top-left corner is where it was opened; its first row, Open
    // with, sits just inside. Hovering it opens its submenu.
    let hover_first = |d: &mut Driver, at: Point<Pixels>| {
        d.cx.simulate_mouse_move(at + point(px(40.), px(18.)), None, Modifiers::none());
        d.settle();
    };
    let at = right_click(&mut d, "notes.go");
    let (items, _) = labels(&mut d).expect("a menu on the file");
    assert_eq!(items, ["Open with", "Reveal in Finder", "Copy link", "Copy permalink"]);
    hover_first(&mut d, at);
    let (_, sub) = labels(&mut d).unwrap();
    assert_eq!(sub.expect("the submenu opens"), ["Coder [img]", "Jotter"]);
    // The Files page is at the window's right edge: the submenu opens to the
    // menu's left rather than over it.
    let (menu, sub) = (d.cx.debug_bounds("menu").unwrap(), d.cx.debug_bounds("submenu").unwrap());
    assert!(!menu.intersects(&sub), "the submenu covers its menu: {menu:?} {sub:?}");
    // A press elsewhere closes it.
    d.cx.simulate_click(point(px(1.), px(300.)), Modifiers::none());
    d.settle();
    assert!(d.read(|s| s.popup.is_none()));

    // On a folder, only editors that open folders.
    let at = right_click(&mut d, "src");
    hover_first(&mut d, at);
    let (_, sub) = labels(&mut d).expect("a menu on the folder");
    assert_eq!(sub.expect("the submenu opens"), ["Coder [img]"]);
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// Typing on the chat with nothing focused starts the message in the composer;
/// shortcuts still go where they did.
#[gpui::test]
fn typing_fills_the_composer(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    assert_eq!(d.active(), TabKind::Chat);
    let ws = d.ws.clone();
    d.cx.update(|window, cx| ws.read(cx).focus.focus(window));
    d.settle();
    d.keys("h i");
    let (value, focused) = d.cx.update(|window, cx| {
        let c = ws.read(cx).composer.read(cx);
        (c.value().to_string(), gpui::Focusable::focus_handle(c, cx).is_focused(window))
    });
    assert_eq!(value, "hi");
    assert!(focused, "the composer takes the cursor");

    // Pasting does the same, adding to what's there.
    d.cx.update(|window, cx| ws.read(cx).focus.focus(window));
    d.cx.write_to_clipboard(gpui::ClipboardItem::new_string(" there".into()));
    d.keys("cmd-v");
    let value = d.cx.update(|_, cx| ws.read(cx).composer.read(cx).value().to_string());
    assert_eq!(value, "hi there");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// Code in file and diff panes can be selected like text: a drag runs across
/// lines, a double click takes a word, and ⌘ C copies what's selected.
#[gpui::test]
fn selects_text_in_code_panes(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let util = project.join("src/util.rs");
    let opened = util.clone();
    d.act(move |s, cx| s.open(TabKind::File(opened), cx));
    d.wait("util.rs", 10, |d| d.read(|s| matches!(s.files.get(&util), Some(Ok(_)))));
    let selected = |d: &mut Driver| d.read(|s| s.code_text_selected());

    // From `fn` on row 1 to just before `* 2` on row 2.
    let (from, to) = (d.point(1, 4), d.point(2, 6));
    d.cx.simulate_mouse_move(from, None, Modifiers::none());
    d.cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    d.cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    d.cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    d.settle();
    assert_eq!(selected(&mut d).as_deref(), Some("fn twice(x: i32) -> i32 {\n    x "));
    d.keys("cmd-c");
    assert_eq!(d.cx.read_from_clipboard().and_then(|c| c.text()).as_deref(), Some("fn twice(x: i32) -> i32 {\n    x "));

    // A double click takes the word.
    let at = d.point(1, 9);
    d.cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    d.cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    d.cx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::none(), click_count: 2, first_mouse: false });
    d.cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    d.settle();
    assert_eq!(selected(&mut d).as_deref(), Some("twice"));
    d.keys("escape");
    assert_eq!(selected(&mut d), None);

    // In a diff, only the drawn text is taken: no `+`, no hunk header.
    let main_rs = project.join("src/main.rs");
    d.act(|s, cx| s.open(TabKind::GitDiff(main_rs.clone(), false), cx));
    let diff = LineSource::GitDiff(main_rs.clone(), false);
    d.wait("the Git diff", 30, |d| d.read(|s| s.diff_sides(&diff).is_some()));
    assert_eq!(selected(&mut d), None, "a selection in another pane doesn't follow");
    let added = d.diff_row(&diff, LineKind::Add, "again");
    let at = d.point(added, 30);
    d.cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    d.cx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: Modifiers::none(), click_count: 3, first_mouse: false });
    d.cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    d.settle();
    assert_eq!(selected(&mut d).as_deref(), Some("\tlet again = util::twice(total);"));
}

/// The agent's questions show as a card: a choice is picked, the "Other" box
/// takes typing, and Submit sends both back, after which the card shows them.
#[gpui::test]
fn answers_the_agents_questions(cx: &mut TestAppContext) {
    use crate::model::{AskOutcome, Block};
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let ws = d.ws.clone();
    d.cx.update(|window, cx| {
        ws.update(cx, |s, cx| {
            s.composer.update(cx, |c, cx| c.set_value("ask me a question", window, cx));
            s.submit(&super::Submit, window, cx);
        })
    });
    let ask = |s: &Workspace| {
        s.current_thread()?.blocks.iter().find_map(|b| match b {
            Block::Ask(a) => Some((a.id, a.outcome.is_some())),
            _ => None,
        })
    };
    d.wait("the question", 30, |d| d.read(|s| ask(s).is_some()));
    let (id, _) = d.read(|s| ask(s).unwrap());
    // The card drew, and with it the input for the "Other" box.
    let other = d.read(|s| s.ask_inputs.iter().find(|((_, a, f), _)| *a == id && f == "question_0_custom").map(|(_, i)| i.clone()));
    let other = other.expect("the free-text answer gets an input");
    d.act(|s, cx| s.pick(id, "question_0".into(), "Redis".into(), cx));
    d.cx.update(|window, cx| other.update(cx, |i, cx| i.set_value("for caching", window, cx)));
    d.act(|s, cx| s.reply(id, false, cx));
    let shown = d.read(|s| {
        s.current_thread().unwrap().blocks.iter().find_map(|b| match b {
            Block::Ask(a) => match &a.outcome {
                Some(AskOutcome::Answered(answers)) => Some(answers.clone()),
                _ => None,
            },
            _ => None,
        })
    });
    assert_eq!(shown, Some(vec![("Database".into(), "Redis".into()), ("Other".into(), "for caching".into())]));
    assert!(d.read(|s| s.ask_inputs.is_empty()), "a settled card lets go of its inputs");
    let said = |s: &Workspace| s.current_thread().unwrap().blocks.iter().any(|b| matches!(b, Block::Text(t) if t.contains(r#""question_0": "Redis""#) && t.contains("for caching")));
    d.wait("the agent to read the answers", 30, |d| d.read(said));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// The question card answers from the keyboard: it takes the keys when it
/// shows, arrows and Space choose, Tab moves into the "Other" box (where Space
/// types), and ⌘ Enter sends it all and hands the keys back to the composer.
#[gpui::test]
fn answers_the_agents_questions_from_the_keyboard(cx: &mut TestAppContext) {
    use crate::model::{AskOutcome, Block};
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let ws = d.ws.clone();
    d.cx.update(|window, cx| {
        ws.update(cx, |s, cx| {
            s.composer.update(cx, |c, cx| c.set_value("ask me a question", window, cx));
            s.submit(&super::Submit, window, cx);
        })
    });
    d.wait("the question", 30, |d| d.read(|s| s.waiting_ask().is_some()));
    d.settle();
    let card_focused = |d: &mut Driver| d.cx.update(|window, cx| ws.read(cx).ask_focus.is_focused(window));
    assert!(card_focused(&mut d), "a question takes the keys when it shows");
    assert_eq!(d.read(|s| s.ask_position().map(|(_, _, q, c)| (q, c))), Some((0, 0)));

    d.keys("down");
    assert_eq!(d.read(|s| s.ask_position().map(|(_, _, q, c)| (q, c))), Some((0, 1)));
    d.keys("space");
    let picks = d.read(|s| s.waiting_ask().unwrap().1.picks.get("question_0").cloned());
    assert_eq!(picks, Some(vec!["Redis".to_string()]));

    d.keys("tab");
    d.keys("f o r space c a c h i n g");
    let typed = d.read(|s| s.ask_inputs.iter().find(|((_, _, f), _)| f == "question_0_custom").map(|(_, i)| i.clone()));
    let typed = d.cx.update(|_, cx| typed.unwrap().read(cx).value().to_string());
    assert_eq!(typed, "for caching");

    d.keys("cmd-enter");
    let shown = d.read(|s| {
        s.current_thread().unwrap().blocks.iter().find_map(|b| match b {
            Block::Ask(a) => match &a.outcome {
                Some(AskOutcome::Answered(answers)) => Some(answers.clone()),
                _ => None,
            },
            _ => None,
        })
    });
    assert_eq!(shown, Some(vec![("Database".into(), "Redis".into()), ("Other".into(), "for caching".into())]));
    let composer_focused = d.cx.update(|window, cx| gpui::Focusable::focus_handle(ws.read(cx).composer.read(cx), cx).is_focused(window));
    assert!(composer_focused, "the composer gets the keys back");

    // Escape skips, without also stopping the agent.
    d.wait("the first turn to end", 30, |d| d.read(|s| !s.current_thread().unwrap().state.busy()));
    d.cx.update(|window, cx| {
        ws.update(cx, |s, cx| {
            s.composer.update(cx, |c, cx| c.set_value("another question", window, cx));
            s.submit(&super::Submit, window, cx);
        })
    });
    d.wait("the second question", 30, |d| d.read(|s| s.waiting_ask().is_some()));
    d.settle();
    d.keys("escape");
    let said = |s: &Workspace| s.current_thread().unwrap().blocks.iter().any(|b| matches!(b, Block::Text(t) if t.contains("decline")));
    d.wait("the agent to hear it was skipped", 30, |d| d.read(said));
    let stopped = d.read(|s| s.current_thread().unwrap().blocks.iter().any(|b| matches!(b, Block::Notice(n) if n == "Stopped")));
    assert!(!stopped, "skipping isn't stopping");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// ⌘ F finds text in the thread showing, even from the composer: matches are
/// counted, ↵ and ⇧ ↵ step through them (wrapping), and esc puts the bar away.
#[gpui::test]
fn finds_text_in_the_thread(cx: &mut TestAppContext) {
    use crate::model::{Block, Message};
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    d.act(|s, cx| {
        s.start_thread(cx);
        let thread = s.current_thread_mut().unwrap();
        thread.blocks.push(Block::User(Message { text: "Where is the Needle?".into(), at: None }));
        for n in 0..40 {
            thread.blocks.push(Block::Text(format!("Paragraph {n} says nothing much.")));
        }
        thread.blocks.push(Block::Text("Found a needle, and **another needle**.".into()));
    });
    // A new thread shows its end; start from the top, where the first match is.
    d.act(|s, _| {
        s.follow = false;
        s.transcript.set_offset(point(px(0.), px(0.)));
    });
    let composer = d.read(|s| s.composer.clone());
    d.cx.update(|window, cx| composer.update(cx, |c, cx| c.focus(window, cx)));
    d.keys("cmd-f");
    assert!(d.read(|s| s.thread_find_open), "⌘ F opens the bar, over the composer too");
    d.keys("n e e d l e");
    let count = |d: &mut Driver| d.read(|s| s.thread_find_count());
    assert_eq!(count(&mut d), Some((Some(0), 3)), "the first match in view is current");
    d.keys("enter");
    assert_eq!(count(&mut d), Some((Some(1), 3)));
    // The match far down was scrolled to.
    let shown = d.read(|s| {
        let selection = s.selection.borrow();
        let current = selection.find.as_ref().and_then(|f| f.current).unwrap();
        let (at, _) = selection.position(current).unwrap();
        let view = s.transcript.bounds();
        at.y >= view.top() && at.y <= view.bottom()
    });
    assert!(shown, "the current match scrolls into view");
    d.keys("enter enter");
    assert_eq!(count(&mut d), Some((Some(0), 3)), "stepping wraps around");
    d.keys("shift-enter");
    assert_eq!(count(&mut d), Some((Some(2), 3)));
    d.keys("escape");
    assert!(d.read(|s| !s.thread_find_open && s.selection.borrow().find.is_none()));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// After a restart, only the thread on show has its blocks read in; another
/// is read when opened, and let go of again once put away and saved.
#[gpui::test]
fn threads_load_when_opened(cx: &mut TestAppContext) {
    use crate::model::{Block, Message};
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    {
        let mut d = boot(cx, &config);
        d.settle();
        for (n, at) in [("older", 1_000), ("newer", 2_000)] {
            d.act(|s, cx| {
                s.next_thread += 1;
                let mut t = crate::model::Thread::new(s.next_thread, 0);
                t.title = n.into();
                t.blocks.push(Block::User(Message { text: format!("the {n} one").into(), at: Some(at) }));
                t.mark_active(at);
                s.projects[s.project].threads.push(t);
                let _ = cx;
            });
        }
        d.act(|s, _| s.save_now());
    }
    let mut d = boot(cx, &config);
    d.settle();
    let loaded = |d: &mut Driver| d.read(|s| s.current_project().threads.iter().map(|t| (t.title.to_string(), !t.unloaded)).collect::<Vec<_>>());
    let state = loaded(&mut d);
    assert!(state.contains(&("newer".into(), true)) && state.contains(&("older".into(), false)), "only the one on show: {state:?}");
    let older = d.read(|s| s.current_project().threads.iter().position(|t| t.title.as_ref() == "older").unwrap());
    d.act(|s, cx| s.select_thread(older, cx));
    assert!(d.read(|s| matches!(s.current_thread().unwrap().blocks.first(), Some(Block::User(m)) if m.text.as_ref() == "the older one")));
    // Back to the other; the one put away has no agent running, so the next
    // save lets go of it.
    let newer = d.read(|s| s.current_project().threads.iter().position(|t| t.title.as_ref() == "newer").unwrap());
    d.act(|s, cx| {
        s.select_thread(newer, cx);
        for t in &mut s.projects[s.project].threads {
            t.session = None;
        }
        s.save_now();
    });
    assert!(loaded(&mut d).contains(&("older".into(), false)), "put away and saved: let go of");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// A search result picked from the thread list opens its thread at the
/// paragraph it came from, scrolled into view and lit for a moment.
#[gpui::test]
fn search_results_open_at_their_paragraph(cx: &mut TestAppContext) {
    use crate::model::{Block, Message};
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let block = 31;
    d.act(|s, cx| {
        s.start_thread(cx);
        let thread = s.current_thread_mut().unwrap();
        thread.blocks.push(Block::User(Message { text: "Tell me a story".into(), at: None }));
        for n in 0..30 {
            thread.blocks.push(Block::Text(format!("Paragraph {n} says nothing much.")));
        }
        thread.blocks.push(Block::Text("Once.\n\nThe lighthouse keeper woke.\n\nThe end.".into()));
        for n in 30..60 {
            thread.blocks.push(Block::Text(format!("Paragraph {n} says nothing much.")));
        }
    });
    let id = d.read(|s| s.current_thread().unwrap().id);
    // Looking at another thread when the result is picked.
    d.act(|s, cx| s.start_thread(cx));
    let search = d.read(|s| s.thread_search.clone());
    d.cx.update(|window, cx| search.update(cx, |i, cx| i.set_value("lighthouse", window, cx)));
    d.settle();
    let index = d.read(|s| s.current_project().threads.iter().position(|t| t.id == id).unwrap());
    d.act(|s, cx| s.reveal_hit(index, block, 0, cx));
    d.settle();
    let (lit, shown) = d.read(|s| {
        let selection = s.selection.borrow();
        let flash = selection.flash.as_ref().map(|f| f.key);
        let view = s.transcript.bounds();
        let shown = flash.and_then(|k| selection.bounds(k)).is_some_and(|b| b.top() >= view.top() && b.bottom() <= view.bottom());
        (flash, shown)
    });
    assert_eq!(d.read(|s| s.thread), index, "the result's thread opens");
    assert_eq!(lit.map(|k| k.0), Some(block), "the paragraph's block is lit");
    assert!(shown, "and scrolled into view");
    let text = d.read(|s| s.selection.borrow().flash.as_ref().map(|f| f.needle.clone()));
    assert_eq!(text.as_deref(), Some("lighthouse"));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// ⌘ ⇧ O finds files by name on the Files page: ↓ picks the next, ↵ opens
/// the pick, and esc goes back to the tree.
#[gpui::test]
fn finds_files_by_name(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    d.keys("cmd-shift-o");
    assert!(d.read(|s| s.file_find_open && s.side_open && s.side == super::Side::Files));
    d.keys("u t i l");
    d.wait("the file index", 10, |d| d.read(|s| !s.mention_indexing && s.mention_index.is_some()));
    let ws = d.ws.clone();
    let found = |d: &mut Driver| d.cx.update(|_, cx| ws.read(cx).found_files(cx).iter().map(|c| c.path.clone()).collect::<Vec<_>>());
    let names = found(&mut d);
    assert_eq!(names.first(), Some(&project.join("src/util.rs")), "the name match comes first: {names:?}");
    // Letters in order, not together: "smn" finds src/main.rs.
    let input = d.read(|s| s.file_find.clone());
    d.cx.update(|window, cx| input.update(cx, |i, cx| i.set_value("smn", window, cx)));
    d.settle();
    let names = found(&mut d);
    assert!(names.contains(&project.join("src/main.rs")), "{names:?}");
    d.keys("down up enter");
    assert_eq!(d.active(), TabKind::File(names[0].clone()));
    d.cx.update(|window, cx| input.update(cx, |i, cx| i.focus(window, cx)));
    d.keys("escape");
    assert!(d.read(|s| !s.file_find_open));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// ⌘ , opens settings with the search box taking the keys; typing filters
/// across sections, and esc closes the dialog even from the search box.
#[gpui::test]
fn searches_and_closes_settings(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    d.keys("cmd-,");
    assert!(d.read(|s| s.prefs_open));
    let search = d.read(|s| s.prefs_search.clone());
    let focused = d.cx.update(|window, cx| gpui::Focusable::focus_handle(search.read(cx), cx).is_focused(window));
    assert!(focused, "the search box takes the keys");
    // In one go: gpui keeps the bounds of rows drawn in earlier frames, so a
    // letter at a time would leave every row's behind.
    d.cx.update(|window, cx| search.update(cx, |i, cx| i.set_value("indent", window, cx)));
    d.settle();
    // The matching row drew, from another section; the others didn't.
    assert!(d.cx.debug_bounds("pref-row:Indent guides").is_some());
    assert!(d.cx.debug_bounds("pref-row:Tab width").is_none());
    d.keys("escape");
    assert!(d.read(|s| !s.prefs_open));
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// The limit in the status bar opens the usage widget, with the week's bar and
/// the five-hour clock; a second click, Escape or a click away closes it.
#[gpui::test]
fn opens_the_usage_widget(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let ws = d.ws.clone();
    d.cx.update(|window, cx| {
        ws.update(cx, |s, cx| {
            s.composer.update(cx, |c, cx| c.set_value("explain it", window, cx));
            s.submit(&super::Submit, window, cx);
        })
    });
    // The mock agent reports its limits when a turn ends.
    d.wait("the agent's limits", 60, |d| d.cx.debug_bounds("rate-limit").is_some());
    let click = |d: &mut Driver| {
        // A click just after a click away closed it counts as that close.
        std::thread::sleep(Duration::from_millis(450));
        let item = d.cx.debug_bounds("rate-limit").unwrap();
        d.cx.simulate_click(item.center(), Modifiers::none());
        d.settle();
    };
    click(&mut d);
    assert!(d.read(|s| s.usage_open()), "a click opens the widget");
    let pop = d.cx.debug_bounds("usage-popover").expect("the widget is drawn");
    let item = d.cx.debug_bounds("rate-limit").unwrap();
    assert!(pop.bottom() <= item.top(), "it opens above the status bar");
    click(&mut d);
    assert!(!d.read(|s| s.usage_open()), "a second click closes it");
    click(&mut d);
    d.keys("escape");
    assert!(!d.read(|s| s.usage_open()), "Escape closes it");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}

/// A finished turn is counted: "Chat" in the title bar opens the dashboard
/// with its tokens, spend and tools, a range can be picked, and Escape closes it.
#[gpui::test]
fn counts_turns_on_the_dashboard(cx: &mut TestAppContext) {
    let _one = E2E.lock().unwrap_or_else(|e| e.into_inner());
    let (project, config) = fixture();
    let mut d = boot(cx, &config);
    d.settle();
    let ws = d.ws.clone();
    d.cx.update(|window, cx| {
        ws.update(cx, |s, cx| {
            s.composer.update(cx, |c, cx| c.set_value("greet the user", window, cx));
            s.submit(&super::Submit, window, cx);
        })
    });
    // Allow the edit it asks about.
    d.wait("the permission request", 60, |d| d.read(|s| s.current_thread().is_some_and(|t| t.awaiting_user())));
    let id = d.read(|s| {
        s.current_thread().unwrap().blocks.iter().find_map(|b| match b {
            crate::model::Block::Permission(p) if p.chosen.is_none() => Some(p.id),
            _ => None,
        })
    });
    d.act(|s, cx| s.answer(id.unwrap(), Some(("yes".into(), "Allow".into())), cx));
    d.wait("the turn to finish", 60, |d| d.read(|s| s.current_thread().is_some_and(|t| t.turn_began.is_none() && !t.blocks.is_empty())));
    let chat = d.cx.debug_bounds("open-dashboard").expect("Chat is in the title bar");
    d.cx.simulate_click(chat.center(), Modifiers::none());
    d.wait("the dashboard's numbers", 30, |d| d.read(|s| s.dashboard.as_ref().is_some_and(|d| d.stats.is_some())));
    assert!(d.cx.debug_bounds("dashboard").is_some(), "it's drawn in place of the threads");
    let stats = d.read(|s| s.dashboard.as_ref().and_then(|d| d.stats.clone()).unwrap());
    assert_eq!(stats.turns, 1);
    assert_eq!(stats.tokens, 42_500, "the mock agent's turn, cache included");
    assert_eq!(stats.models, vec![("mock-large".to_string(), 900)]);
    assert!(stats.cost > 0., "the session's cost went up during the turn");
    assert!(stats.tools.iter().any(|(k, n, _)| k == "Run" && *n >= 1), "tools counted: {:?}", stats.tools);
    assert!(stats.weeks.last().is_some_and(|w| w.adds > 0), "lines changed counted");
    assert_eq!((stats.allowed, stats.denied), (1, 0));

    d.act(|s, cx| s.set_range(crate::stats::Range::Year, cx));
    assert_eq!(d.read(|s| s.dashboard.as_ref().map(|d| d.range)), Some(crate::stats::Range::Year));
    d.keys("escape");
    assert!(d.read(|s| !s.dashboard_open()), "Escape closes it");
    let _ = std::fs::remove_dir_all(project.parent().unwrap());
}
