//! One language server process, spoken to in JSON-RPC over its stdio. Three
//! threads carry the bytes: one writes frames, one reads them, one drains
//! stderr. Replies are matched to requests by id; the server's own requests
//! are answered here, so callers only ever see notifications.
use anyhow::{Context as _, Result, anyhow};
use futures::channel::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded},
    oneshot,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
};

/// What a server says unprompted.
#[derive(Debug)]
pub enum Incoming {
    Notification { method: String, params: Value },
    /// A line for the server's log: what it printed to stderr, or a message in brief.
    Log(String),
    /// The server stopped, or its output stopped making sense.
    Exited(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

pub struct Client {
    outgoing: mpsc::Sender<Vec<u8>>,
    log: UnboundedSender<Incoming>,
    pending: Pending,
    next_id: AtomicI64,
    child: Mutex<Child>,
}

fn logging() -> bool {
    std::env::var_os("CHAT_LSP_LOG").is_some()
}

impl Client {
    /// Start `command` in `cwd` with `path` as its PATH.
    pub fn spawn(command: &str, args: &[String], cwd: &Path, path: Option<&str>) -> Result<(Arc<Self>, UnboundedReceiver<Incoming>)> {
        let mut cmd = Command::new(command);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(path) = path {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().with_context(|| format!("could not start {command}"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let (outgoing, queue) = mpsc::channel::<Vec<u8>>();
        let (events, incoming) = unbounded();
        let pending: Pending = Arc::default();
        let name = command.to_string();

        std::thread::spawn(move || write_frames(stdin, queue));
        let log = events.clone();
        {
            let (pending, outgoing, name) = (pending.clone(), outgoing.clone(), name.clone());
            std::thread::spawn(move || read_frames(BufReader::new(stdout), &name, &pending, &outgoing, &events));
        }
        {
            let log = log.clone();
            std::thread::spawn(move || {
                // A server that fills its stderr pipe would block; keep it empty.
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if logging() {
                        eprintln!("[{name} stderr] {line}");
                    }
                    let _ = log.unbounded_send(Incoming::Log(line));
                }
            });
        }

        let client = Arc::new(Self { outgoing, log, pending, next_id: AtomicI64::new(1), child: Mutex::new(child) });
        Ok((client, incoming))
    }

    /// The server's process id.
    pub fn pid(&self) -> u32 {
        self.child.lock().unwrap().id()
    }

    fn send(&self, message: Value) {
        if logging() {
            eprintln!("→ {message}");
        }
        let _ = self.log.unbounded_send(Incoming::Log(brief('→', &message)));
        let _ = self.outgoing.send(frame(&message));
    }

    /// Send a request; the future resolves with its result, or its error.
    pub fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value>> + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, reply);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let method = method.to_string();
        async move { answer.await.map_err(|_| anyhow!("{method}: the server went away"))? }
    }

    /// A request by its `lsp_types` type, with typed params and result.
    pub fn call<R: lsp_types::request::Request>(&self, params: R::Params) -> impl Future<Output = Result<R::Result>> + use<R> {
        let sent = serde_json::to_value(params).map(|p| self.request(R::METHOD, p));
        async move {
            let value = sent?.await?;
            Ok(serde_json::from_value(value)?)
        }
    }

    pub fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        if let Ok(params) = serde_json::to_value(params) {
            self.send(json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params }));
        }
    }

    /// Stop the server now, without asking.
    pub fn kill(&self) {
        let _ = self.child.lock().unwrap().kill();
    }

    /// Ask the server to stop, then make sure it does.
    pub fn shutdown(self: &Arc<Self>) {
        let this = self.clone();
        let asked = self.request("shutdown", Value::Null);
        std::thread::spawn(move || {
            let polite = std::thread::spawn(move || futures::executor::block_on(asked));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !polite.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            this.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = this.child.lock().unwrap().kill();
        });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
        }
    }
}

/// A message as one log line: its direction, method and id, and a little of
/// what it carries.
pub(super) fn brief(arrow: char, message: &Value) -> String {
    const ROOM: usize = 240;
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").filter(|id| !id.is_null()).map(|id| format!(" #{id}")).unwrap_or_default();
    let (head, body) = match (method, message.get("error")) {
        (Some(method), _) => (format!("{method}{id}"), message.get("params")),
        (None, Some(error)) => (format!("error{id}"), Some(error)),
        (None, None) => (format!("reply{id}"), message.get("result")),
    };
    let mut body = body.map(Value::to_string).unwrap_or_default();
    if body.len() > ROOM {
        let mut cut = ROOM;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push('…');
    }
    format!("{arrow} {head} {body}").trim_end().to_string()
}

fn frame(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

fn write_frames(mut stdin: impl Write, queue: mpsc::Receiver<Vec<u8>>) {
    for bytes in queue {
        if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
            break;
        }
    }
}

/// The next message's body, or `None` once the stream ends.
pub(super) fn read_frame(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            // Stray blank lines before a header are tolerated.
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().context("bad Content-Length")?);
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// What to answer a request from the server with. Settings are left to each
/// server's defaults; progress and registrations are simply accepted.
fn answer(method: &str, params: &Value) -> Value {
    match method {
        "workspace/configuration" => {
            let items = params.get("items").and_then(Value::as_array).map_or(0, Vec::len);
            Value::Array(vec![Value::Null; items])
        }
        "workspace/workspaceFolders" => Value::Null,
        _ => Value::Null,
    }
}

fn read_frames(
    mut reader: impl BufRead,
    name: &str,
    pending: &Pending,
    outgoing: &mpsc::Sender<Vec<u8>>,
    events: &UnboundedSender<Incoming>,
) {
    let why = loop {
        let body = match read_frame(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => break format!("{name} exited"),
            Err(e) => break format!("{name}: {e}"),
        };
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        if logging() {
            eprintln!("← {message}");
        }
        let _ = events.unbounded_send(Incoming::Log(brief('←', &message)));
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            // A request from the server.
            (Some(method), Some(id)) => {
                let result = answer(method, message.get("params").unwrap_or(&Value::Null));
                let _ = outgoing.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": result })));
            }
            (Some(method), None) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let _ = events.unbounded_send(Incoming::Notification { method: method.to_string(), params });
            }
            (None, Some(id)) => {
                let Some(reply) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { continue };
                let result = match message.get("error") {
                    Some(error) => Err(anyhow!("{}", error.get("message").and_then(Value::as_str).unwrap_or("request failed"))),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(result);
            }
            (None, None) => {}
        }
    };
    // Whoever is still waiting hears that no answer is coming.
    pending.lock().unwrap().clear();
    let _ = events.unbounded_send(Incoming::Exited(why));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_frames_back_to_back() {
        let a = frame(&json!({ "id": 1 }));
        let b = frame(&json!({ "method": "x" }));
        let mut bytes = a.clone();
        bytes.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        bytes.extend_from_slice(&b);
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), br#"{"id":1}"#);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), br#"{"method":"x"}"#);
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn counts_bytes_not_characters() {
        let message = json!({ "text": "café ✓" });
        let mut reader = Cursor::new(frame(&message));
        let body = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), message);
    }

    #[test]
    fn logs_messages_in_brief() {
        assert_eq!(brief('→', &json!({ "id": 3, "method": "textDocument/hover", "params": { "a": 1 } })), r#"→ textDocument/hover #3 {"a":1}"#);
        assert_eq!(brief('←', &json!({ "id": 3, "result": null })), "← reply #3 null");
        assert_eq!(brief('←', &json!({ "id": 4, "error": { "message": "no" } })), r#"← error #4 {"message":"no"}"#);
        let long = brief('←', &json!({ "method": "x", "params": "é".repeat(400) }));
        assert!(long.ends_with('…') && long.len() < 300);
    }

    #[test]
    fn answers_configuration_with_one_null_per_item() {
        let params = json!({ "items": [{ "section": "a" }, { "section": "b" }] });
        assert_eq!(answer("workspace/configuration", &params), json!([null, null]));
    }

    #[test]
    fn matches_replies_and_forwards_notifications() {
        let pending: Pending = Arc::default();
        let (reply, answer) = oneshot::channel();
        pending.lock().unwrap().insert(7, reply);
        let mut bytes = frame(&json!({ "jsonrpc": "2.0", "method": "$/progress", "params": { "token": "t" } }));
        bytes.extend(frame(&json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } })));
        bytes.extend(frame(&json!({ "jsonrpc": "2.0", "id": 3, "method": "workspace/configuration", "params": { "items": [{}] } })));
        let (outgoing, sent) = mpsc::channel();
        let (events, mut incoming) = unbounded();
        read_frames(Cursor::new(bytes), "test", &pending, &outgoing, &events);

        assert_eq!(futures::executor::block_on(answer).unwrap().unwrap(), json!({ "ok": true }));
        // Every message is logged in brief; notifications also come through whole.
        let mut events = std::iter::from_fn(|| incoming.try_recv().ok()).filter(|e| !matches!(e, Incoming::Log(_)));
        assert!(matches!(events.next().unwrap(), Incoming::Notification { ref method, .. } if method == "$/progress"));
        assert!(matches!(events.next().unwrap(), Incoming::Exited(_)));
        let response = sent.try_recv().unwrap();
        let body = read_frame(&mut Cursor::new(response)).unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({ "jsonrpc": "2.0", "id": 3, "result": [null] }));
    }
}
