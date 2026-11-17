//! A tool the agent can call to say which Git worktree it is working in, for a
//! project folder. The agent starts this binary again as a stdio MCP server
//! (`chat --worktree-tool …`); a call is checked against Git and passed to the
//! running app over a Unix socket, so the Changes page can follow the thread.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

/// The flag that starts the binary as the tool server instead of the app.
pub const FLAG: &str = "--worktree-tool";
const TOOL: &str = "set_worktree";

/// A report from a thread's agent: `folder` is the project folder, `worktree`
/// the checkout it now works in, or `None` for the folder itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub thread: usize,
    pub folder: PathBuf,
    pub worktree: Option<PathBuf>,
}

static SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// Where the app listens, once it does.
pub fn socket() -> Option<&'static Path> {
    SOCKET.get().map(PathBuf::as_path)
}

/// Listen for reports; each arrives on `send`. Failing to listen leaves the
/// tool out of new sessions and nothing else.
pub fn listen(send: impl Fn(Report) + Send + 'static) -> std::io::Result<()> {
    let path = std::env::temp_dir().join(format!("chat-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let _ = SOCKET.set(path);
    std::thread::Builder::new().name("worktree-tool".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let reply = match serde_json::from_str::<Report>(&line) {
                Ok(report) => {
                    send(report);
                    "ok\n"
                }
                Err(_) => "bad report\n",
            };
            let _ = (&stream).write_all(reply.as_bytes());
        }
    })?;
    Ok(())
}

/// Remove the socket as the app quits.
pub fn cleanup() {
    if let Some(path) = socket() {
        let _ = std::fs::remove_file(path);
    }
}

/// The arguments the agent starts the tool server with.
pub fn args(socket: &Path, thread: usize, folders: &[PathBuf]) -> Vec<String> {
    let mut args = vec![FLAG.to_owned(), socket.to_string_lossy().into_owned(), thread.to_string()];
    args.extend(folders.iter().map(|f| f.to_string_lossy().into_owned()));
    args
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned())
}

/// A checkout's top level and the repository it belongs to, both absolute.
fn checkout(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = git(dir, &["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir"])?;
    let mut lines = out.lines();
    let top = std::fs::canonicalize(lines.next()?).ok()?;
    let common = std::fs::canonicalize(lines.next()?).ok()?;
    Some((top, common))
}

/// Which project folder `worktree` belongs to, and whether it is that folder's
/// own checkout. `folder`, when given, picks among several.
fn resolve(folders: &[PathBuf], folder: Option<&str>, worktree: &str) -> Result<Report, String> {
    let path = Path::new(worktree);
    if !path.is_absolute() {
        return Err(format!("`worktree` must be an absolute path, not {worktree:?}."));
    }
    let (top, common) = checkout(path).ok_or_else(|| format!("{worktree} is not inside a Git checkout."))?;
    let wanted = folder.map(|f| std::fs::canonicalize(f).unwrap_or_else(|_| PathBuf::from(f)));
    let mut matches = folders.iter().filter_map(|f| {
        let (own, repo) = checkout(f)?;
        let named = wanted.as_ref().is_none_or(|w| std::fs::canonicalize(f).is_ok_and(|c| &c == w));
        (repo == common && named).then(|| (f.clone(), own))
    });
    let Some((folder, own)) = matches.next() else {
        let list = folders.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ");
        return Err(format!("{} is not a worktree of any project folder ({list}).", top.display()));
    };
    if matches.next().is_some() {
        return Err("Several project folders share this repository; pass `folder` to pick one.".into());
    }
    Ok(Report { thread: 0, folder, worktree: (top != own).then_some(top) })
}

fn send(socket: &Path, report: &Report) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("The app isn't listening: {e}"))?;
    let mut line = serde_json::to_string(report).map_err(|e| e.to_string())?;
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    if reply.trim() == "ok" { Ok(()) } else { Err(reply.trim().to_owned()) }
}

fn describe() -> Value {
    json!({
        "name": TOOL,
        "description": "Tell the user's app which Git worktree you are working in. Call this whenever you start \
            making changes in a worktree other than a project folder itself (for example after `git worktree add`), \
            once for each project folder involved, so the user's Changes panel shows that worktree for this thread. \
            Call it with the project folder's own path to go back to it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "worktree": {"type": "string", "description": "Absolute path of the worktree checkout you are working in."},
                "folder": {"type": "string", "description": "The project folder this worktree belongs to. Only needed when several project folders share one repository."}
            },
            "required": ["worktree"]
        }
    })
}

/// Answer one JSON-RPC message; `None` for notifications.
fn handle(message: &Value, socket: &Path, thread: usize, folders: &[PathBuf]) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "chat", "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": [describe()]}),
        "tools/call" if message["params"]["name"] == TOOL => {
            let args = &message["params"]["arguments"];
            let outcome = match args["worktree"].as_str() {
                None => Err("`worktree` is required.".to_owned()),
                Some(worktree) => resolve(folders, args["folder"].as_str(), worktree).and_then(|mut report| {
                    report.thread = thread;
                    send(socket, &report)?;
                    Ok(match &report.worktree {
                        Some(w) => format!("Recorded: {} is being worked on in {}.", report.folder.display(), w.display()),
                        None => format!("Recorded: back to working in {} itself.", report.folder.display()),
                    })
                }),
            };
            let (text, error) = match outcome {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": error})
        }
        _ => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Unknown method {method}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Run as the tool server: `FLAG socket thread folder…`, MCP over stdio.
pub fn serve(args: &[String]) {
    let (Some(socket), Some(thread)) = (args.first(), args.get(1).and_then(|t| t.parse().ok())) else {
        eprintln!("usage: chat {FLAG} <socket> <thread> <folder>…");
        std::process::exit(2);
    };
    let folders: Vec<PathBuf> = args[2..].iter().map(PathBuf::from).collect();
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(reply) = handle(&message, Path::new(socket), thread, &folders) {
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "chat-wt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("main")).unwrap();
        let main = root.join("main");
        for args in [
            &["init", "-q"][..],
            &["-c", "user.name=T", "-c", "user.email=t@e", "commit", "-q", "--allow-empty", "-m", "x"],
            &["worktree", "add", "-q", "-b", "side", "../side"],
        ] {
            assert!(Command::new("git").arg("-C").arg(&main).args(args).status().unwrap().success());
        }
        std::fs::canonicalize(root).unwrap()
    }

    #[test]
    fn resolves_worktrees_to_their_project_folder() {
        let root = repo();
        let folders = vec![root.join("main")];
        let side = root.join("side");
        let report = resolve(&folders, None, side.to_str().unwrap()).unwrap();
        assert_eq!((report.folder, report.worktree), (root.join("main"), Some(side.clone())));
        // The folder itself clears the report.
        let report = resolve(&folders, None, root.join("main").to_str().unwrap()).unwrap();
        assert_eq!(report.worktree, None);
        assert!(resolve(&folders, None, "relative").is_err());
        assert!(resolve(&folders, None, std::env::temp_dir().to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn speaks_enough_mcp() {
        let folders = vec![];
        let init = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), Path::new("/nope"), 1, &folders).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), Path::new("/nope"), 1, &folders).is_none());
        let list = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), Path::new("/nope"), 1, &folders).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], TOOL);
        let call = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": TOOL, "arguments": {}}}), Path::new("/nope"), 1, &folders).unwrap();
        assert_eq!(call["result"]["isError"], true);
    }
}
