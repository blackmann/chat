//! Keeping an eye on language servers: a bolt in the status bar opens a menu
//! of them by project, each with its state, memory, logs, and a restart or
//! stop; their logs open in a tab of their own.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::scroll::ScrollableElement;
use crate::lsp::{Server, Status};
use crate::menu::{Entry, Item};
use std::collections::VecDeque;

/// The anchor the servers menu opens at.
pub(super) const SERVERS: &str = "lsp-servers";
/// Lines kept per server.
const LOG_LINES: usize = 5000;
/// How often memory is read while servers run.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// A server across restarts: its name and where it's rooted.
pub(super) type Key = (String, PathBuf);

/// What a server is up to, as a dot and a few words, and more when there's more to say.
pub(super) struct Line {
    pub dot: u32,
    pub text: String,
    pub more: Option<String>,
}

pub(super) fn status_line(server: &Server, colors: Palette) -> Line {
    let memory = server.memory.map(|m| format!(" — {}", bytes(m))).unwrap_or_default();
    let (dot, text, more) = match &server.status {
        Status::Installing => (colors.blue, "Installing…".to_string(), None),
        Status::Starting => (colors.blue, "Starting…".to_string(), None),
        Status::Ready => match server.busy() {
            Some(work) => (colors.blue, format!("{work}{memory}"), None),
            None => (colors.green, format!("Running{memory}"), None),
        },
        Status::NotFound => (
            colors.amber,
            "Not installed".to_string(),
            Some(match (&server.note, crate::lsp::install_hint(&server.spec.name)) {
                (Some(note), Some(hint)) => format!("{note} Or: {hint}"),
                (Some(note), None) => note.clone(),
                (None, Some(hint)) => hint.to_string(),
                (None, None) => format!("`{}` isn't on your PATH.", server.spec.command),
            }),
        ),
        Status::Failed(why) => (colors.red, "Crashed".to_string(), Some(why.chars().take(200).collect())),
        Status::Stopped => (colors.quiet, "Stopped".to_string(), None),
    };
    Line { dot, text, more }
}

/// Bytes as megabytes, or gigabytes past a thousand of them.
pub(super) fn bytes(n: u64) -> String {
    let mb = n as f64 / (1024. * 1024.);
    if mb >= 1000. { format!("{:.1} GB", mb / 1024.) } else { format!("{mb:.1} MB") }
}

/// The resident memory of `pid` and everything it started, from `ps -A -o pid=,ppid=,rss=`
/// (kilobytes), in bytes. A TypeScript server's real work is in the `tsserver` it starts.
pub(super) fn tree_rss(table: &str, pid: u32) -> Option<u64> {
    let rows: Vec<(u32, u32, u64)> = table
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace().map(str::parse::<u64>);
            Some((f.next()?.ok()? as u32, f.next()?.ok()? as u32, f.next()?.ok()?))
        })
        .collect();
    rows.iter().any(|r| r.0 == pid).then_some(())?;
    let mut total = 0;
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        for (child, parent, kb) in &rows {
            if *child == p {
                total += kb * 1024;
            }
            if *parent == p && *child != p {
                stack.push(*child);
            }
        }
    }
    Some(total)
}

fn key_of(server: &Server) -> Key {
    (server.spec.name.clone(), server.root.clone())
}

fn folder(root: &Path) -> String {
    root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl Workspace {
    fn server_id(&self, key: &Key) -> Option<u64> {
        self.servers.all.iter().find(|(_, s)| key_of(s) == *key).map(|(id, _)| *id)
    }

    // ── Logs ──────────────────────────────────────────────────────────────

    /// Add a line to a server's log, following along if its tab is showing.
    pub(super) fn log(&mut self, key: &Key, line: String, cx: &mut Context<Self>) {
        let stamp = chrono::Local::now().format("%H:%M:%S%.3f");
        let lines = self.servers.logs.entry(key.clone()).or_default();
        for part in line.lines() {
            lines.push_back(format!("{stamp}  {part}").into());
        }
        while lines.len() > LOG_LINES {
            lines.pop_front();
        }
        let showing = matches!(&self.tabs[self.active].kind, TabKind::ServerLog(n, r) if n == &key.0 && r == &key.1);
        if showing {
            let len = lines.len();
            if let Some(scroll) = self.servers.log_scroll.get(key) {
                // Follow new lines only from the bottom: reading further up stays put.
                let at_bottom = {
                    let state = scroll.0.borrow();
                    let handle = &state.base_handle;
                    -handle.offset().y >= handle.max_offset().height - px(48.)
                };
                if at_bottom {
                    scroll.scroll_to_item(len.saturating_sub(1), ScrollStrategy::Bottom);
                }
            }
            cx.notify();
        }
    }

    pub(super) fn open_server_log(&mut self, key: Key, cx: &mut Context<Self>) {
        let scroll = self.servers.log_scroll.entry(key.clone()).or_default().clone();
        let len = self.servers.logs.get(&key).map_or(0, VecDeque::len);
        self.open(TabKind::ServerLog(key.0, key.1), cx);
        scroll.scroll_to_item(len.saturating_sub(1), ScrollStrategy::Bottom);
    }

    pub(super) fn server_log_view(&self, name: &str, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let key = (name.to_string(), root.to_path_buf());
        let lines: Rc<Vec<SharedString>> = Rc::new(self.servers.logs.get(&key).map(|l| l.iter().cloned().collect()).unwrap_or_default());
        let scroll = self.servers.log_scroll.get(&key).cloned().unwrap_or_default();
        let state = self.servers.all.values().find(|s| key_of(s) == key).map(|s| status_line(s, colors));
        let header = super::panes::pane_header(
            colors,
            Glyph::Zap,
            &format!("{name} — {}", folder(root)),
            row()
                .gap_3()
                .font_family(crate::theme::ui_font())
                .when_some(state, |s, line| {
                    s.child(row().gap(px(6.)).child(div().size(px(7.)).rounded_full().bg(rgb(line.dot))).child(label(colors, line.text)))
                })
                .child(label(colors, format!("{} lines", lines.len()))),
        );
        let count = lines.len();
        let body = if count == 0 {
            div().p_4().text_color(rgb(colors.muted)).child("Nothing logged yet.").into_any_element()
        } else {
            uniform_list("server-log", count, move |range, _, _| {
                range
                    .map(|i| {
                        let line = lines[i].clone();
                        // Messages in brief read quieter than what the server itself printed.
                        let quiet = line.split_once("  ").is_some_and(|(_, rest)| rest.starts_with(['→', '←']));
                        div()
                            .debug_selector(move || format!("log-line:{i}"))
                            .h(px(20.))
                            .px_3()
                            .flex()
                            .items_center()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(rgb(if quiet { colors.muted } else { colors.code }))
                            .child(line)
                    })
                    .collect()
            })
            .track_scroll(scroll.clone())
            .size_full()
            .py_2()
            .font_family(mono())
            .text_size(px(crate::theme::mono_size()))
            .into_any_element()
        };
        col()
            .size_full()
            .child(header)
            .child(div().relative().flex_1().min_h_0().child(body).vertical_scrollbar(&scroll))
            .into_any_element()
    }

    // ── Restart and stop ───────────────────────────────────────────────────

    /// Start a server afresh, with the files it had open.
    pub(super) fn restart_server(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(id) = self.server_id(key) else { return };
        let Some(mut old) = self.servers.all.remove(&id) else { return };
        let files: Vec<PathBuf> = old.documents().cloned().collect();
        old.stop();
        self.log(key, "— restarting —".into(), cx);
        let new = self.start_server(old.spec.clone(), old.root.clone(), files, cx);
        for owner in self.servers.adopted.values_mut() {
            if *owner == id {
                *owner = new;
            }
        }
        // Its ranges came from the server that's gone.
        if let Some(mark) = self.symbol.as_mut() {
            mark.server = None;
        }
        self.refresh_servers_menu(cx);
        cx.notify();
    }

    pub(super) fn stop_server(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(server) = self.server_id(key).and_then(|id| self.servers.all.get_mut(&id)) else { return };
        if matches!(server.status, Status::Starting | Status::Ready) {
            server.stop();
            self.log(key, "— stopped —".into(), cx);
            self.servers.link = None;
            self.refresh_servers_menu(cx);
            cx.notify();
        }
    }

    /// Install a server Chat installed again, at its newest, and restart it.
    pub(super) fn update_server(&mut self, key: &Key, cx: &mut Context<Self>) {
        let Some(id) = self.server_id(key) else { return };
        let Some(server) = self.servers.all.get_mut(&id) else { return };
        server.stop();
        server.status = Status::Installing;
        let spec = server.spec.clone();
        self.log(key, "— updating —".into(), cx);
        self.install_server(id, spec, true, cx);
        self.refresh_servers_menu(cx);
        cx.notify();
    }

    pub(super) fn restart_all_servers(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<Key> = self.servers.all.values().map(key_of).collect();
        for key in keys {
            self.restart_server(&key, cx);
        }
    }

    pub(super) fn stop_all_servers(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<Key> = self.servers.all.values().map(key_of).collect();
        for key in keys {
            self.stop_server(&key, cx);
        }
    }

    // ── Memory ─────────────────────────────────────────────────────────────

    /// Read servers' memory every so often while any runs.
    pub(super) fn sample_memory(&mut self, cx: &mut Context<Self>) {
        if self.servers.sampling {
            return;
        }
        self.servers.sampling = true;
        cx.spawn(async move |this, cx| {
            loop {
                let pids = this.update(cx, |s, _| {
                    let pids: Vec<(u64, u32)> = s.servers.all.iter().filter_map(|(id, srv)| Some((*id, srv.client.as_ref()?.pid()))).collect();
                    if pids.is_empty() {
                        s.servers.sampling = false;
                    }
                    pids
                });
                let Ok(pids) = pids else { break };
                if pids.is_empty() {
                    break;
                }
                let table = cx
                    .background_executor()
                    .spawn(async {
                        std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,rss="]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    })
                    .await;
                let done = this.update(cx, |s, cx| {
                    if let Some(table) = &table {
                        for (id, pid) in &pids {
                            if let Some(server) = s.servers.all.get_mut(id) {
                                server.memory = tree_rss(table, *pid);
                            }
                        }
                        s.refresh_servers_menu(cx);
                    }
                });
                if done.is_err() {
                    break;
                }
                cx.background_executor().timer(SAMPLE_EVERY).await;
            }
        })
        .detach();
    }

    // ── The menu ───────────────────────────────────────────────────────────

    /// Draw the servers menu again if it's open: what it shows has changed.
    pub(super) fn refresh_servers_menu(&self, cx: &mut Context<Self>) {
        if self.popup_anchor == Some(SERVERS)
            && let Some(menu) = self.popup.clone()
        {
            // Often asked from inside the menu itself (a restart picked in it),
            // which is mid-update until this returns.
            cx.defer(move |cx| menu.update(cx, |m, cx| m.refresh(cx)));
        }
    }

    /// The bolt in the status bar, once any server has started: blue while one
    /// is busy, amber when one is missing or has crashed.
    pub(super) fn servers_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.servers.all.is_empty() {
            return None;
        }
        let colors = palette(cx);
        let trouble = self.servers.all.values().any(|s| matches!(s.status, Status::NotFound | Status::Failed(_)));
        let busy = self.servers.all.values().any(|s| matches!(s.status, Status::Installing | Status::Starting) || (s.status == Status::Ready && s.busy().is_some()));
        let tint = if trouble {
            colors.amber
        } else if busy {
            colors.blue
        } else {
            colors.muted
        };
        let trigger = div()
            .id("servers-button")
            .debug_selector(|| "servers-button".into())
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(colors.hover)))
            .child(icon(Glyph::Zap).size(px(14.)).text_color(rgb(tint)))
            .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new("Language servers").build(w, cx));
        Some(self.popover_trigger(SERVERS, trigger, Corner::BottomLeft, 260., None, servers_menu, cx).into_any_element())
    }
}

/// Servers by project, then restarting or stopping them all.
fn servers_menu(s: &Workspace, weak: WeakEntity<Workspace>, cx: &App) -> Vec<Entry> {
    let colors = palette(cx);
    let mut servers: Vec<&Server> = s.servers.all.values().collect();
    servers.sort_by(|a, b| (&a.root, &a.spec.name).cmp(&(&b.root, &b.spec.name)));
    let mut entries = Vec::new();
    let mut last_root: Option<&Path> = None;
    for server in servers {
        if last_root != Some(server.root.as_path()) {
            entries.push(Entry::Label(folder(&server.root).into()));
            last_root = Some(&server.root);
        }
        let key = key_of(server);
        let sub = {
            let (weak, key) = (weak.clone(), key.clone());
            Rc::new(move |cx: &App| weak.upgrade().map(|w| server_menu(w.read(cx), weak.clone(), &key, cx)).unwrap_or_default()) as crate::menu::Builder
        };
        entries.push(Entry::Item(Item::new(server.spec.name.clone()).dot(status_line(server, colors).dot).submenu(sub)));
    }
    let running = s.servers.all.values().any(|s| matches!(s.status, Status::Starting | Status::Ready));
    entries.push(Entry::Separator);
    let w = weak.clone();
    entries.push(Entry::Item(Item::new("Restart All Servers").on_click(move |_, cx| {
        w.update(cx, |s, cx| s.restart_all_servers(cx)).ok();
    })));
    entries.push(Entry::Item(Item::new("Stop All Servers").disabled(!running).on_click(move |_, cx| {
        weak.update(cx, |s, cx| s.stop_all_servers(cx)).ok();
    })));
    entries
}

/// One server: its logs, a restart or stop, and how it's doing.
fn server_menu(s: &Workspace, weak: WeakEntity<Workspace>, key: &Key, cx: &App) -> Vec<Entry> {
    let colors = palette(cx);
    let Some(server) = s.servers.all.values().find(|srv| key_of(srv) == *key) else { return Vec::new() };
    let running = matches!(server.status, Status::Starting | Status::Ready);
    let line = status_line(server, colors);
    // Only what Chat installed is Chat's to update.
    let env = crate::agent::shell_path();
    let updatable = server.status != Status::Installing && crate::lsp::install::managed(&server.spec, env.as_deref());
    let (w1, w2, w3, w4) = (weak.clone(), weak.clone(), weak.clone(), weak);
    let (k1, k2, k3, k4) = (key.clone(), key.clone(), key.clone(), key.clone());
    let mut status = Item::new(line.text).dot(line.dot).disabled(true);
    if let Some(more) = line.more {
        status = status.description(more);
    }
    vec![
        Entry::Item(Item::new("View Logs").on_click(move |_, cx| {
            w1.update(cx, |s, cx| s.open_server_log(k1.clone(), cx)).ok();
        })),
        Entry::Item(Item::new("Restart Server").on_click(move |_, cx| {
            w2.update(cx, |s, cx| s.restart_server(&k2, cx)).ok();
        })),
        Entry::Item(Item::new("Stop Server").disabled(!running).on_click(move |_, cx| {
            w3.update(cx, |s, cx| s.stop_server(&k3, cx)).ok();
        })),
    ]
    .into_iter()
    .chain(updatable.then(|| {
        Entry::Item(Item::new("Update Server").on_click(move |_, cx| {
            w4.update(cx, |s, cx| s.update_server(&k4, cx)).ok();
        }))
    }))
    .chain([Entry::Separator, Entry::Item(status)])
    .collect()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` attribute would shadow the built-in one.
    use super::{bytes, tree_rss};

    #[test]
    fn sums_memory_down_the_process_tree() {
        let table = "  1     0   100\n 50     1  2048\n 51    50  1024\n 52    51   512\n 60     1  4096\n";
        assert_eq!(tree_rss(table, 50), Some((2048 + 1024 + 512) * 1024));
        assert_eq!(tree_rss(table, 60), Some(4096 * 1024));
        assert_eq!(tree_rss(table, 99), None);
    }

    #[test]
    fn reads_bytes_as_people_do() {
        assert_eq!(bytes(153 * 1024 * 1024 + 300 * 1024), "153.3 MB");
        assert_eq!(bytes(1536 * 1024 * 1024), "1.5 GB");
    }
}
