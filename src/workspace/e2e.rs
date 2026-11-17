//! The workspace driven as a person would drive it, against a real language
//! server: clicks, ⌘-hover, ⌘-click, keys and diffs, on a small Rust project
//! with tabs, a non-ASCII name and an uncommitted change. Runs only where
//! rust-analyzer is installed.
// Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
use super::panes::{LineKind, LineSource, diff_lines};
use super::{TabKind, Workspace, lsp};
use gpui::{AppContext as _, Context, Entity, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, point, px};
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
    // Moving off the word, onto the space after `pub`, takes it away.
    let off = d.point(1, 3);
    d.cx.simulate_mouse_move(off, None, Modifiers::none());
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
