//! The phone's side of remote access: an iroh endpoint that dials the Mac by
//! its public key and carries newline-delimited JSON both ways. Swift drives it
//! through the small C ABI below and decodes the JSON itself.
//!
//! Events come back through one callback, on a background thread:
//! `CONNECTED` and `PATH` carry "direct" or "relayed", `LINE` a JSON line from
//! the Mac, `DISCONNECTED` why. Once `chat_bridge_disconnect` returns, no more
//! events arrive for that connection.
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub const ALPN: &[u8] = b"chat/remote/1";
const MAX_LINE: u64 = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);

pub const EVENT_CONNECTED: i32 = 1;
pub const EVENT_LINE: i32 = 2;
pub const EVENT_DISCONNECTED: i32 = 3;
pub const EVENT_PATH: i32 = 4;

/// `generation` is what `chat_bridge_connect` returned for the connection the event belongs to.
pub type Callback = extern "C" fn(ctx: *mut c_void, generation: u64, kind: i32, text: *const c_char);

/// The Swift side's context pointer. It outlives every connection (the app
/// keeps it for its lifetime), and is only handed back, never read here.
#[derive(Clone, Copy)]
struct Ctx(*mut c_void);
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

struct Live {
    out: tokio::sync::mpsc::UnboundedSender<String>,
    task: tokio::task::JoinHandle<()>,
}

/// Owned by the app through a raw pointer. The runtime lives here, apart from
/// what tasks hold, so it's never dropped from inside itself.
pub struct Bridge {
    runtime: tokio::runtime::Runtime,
    inner: Arc<Inner>,
}

struct Inner {
    key: iroh::SecretKey,
    endpoint: tokio::sync::OnceCell<iroh::Endpoint>,
    /// The current connection, and a counter so a replaced one goes quiet.
    live: Arc<Mutex<(u64, Option<Live>)>>,
}

/// The six digits both sides show while pairing; the Mac computes the same.
pub fn pair_code(mac: &str, phone: &str, secret: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"chat-pair-v1\0");
    for part in [mac, phone, secret] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{n:06}")
}

impl Bridge {
    pub fn new(secret: Option<&str>) -> Option<Self> {
        let key = match secret {
            Some(hex_key) => {
                let bytes: [u8; 32] = hex::decode(hex_key).ok()?.try_into().ok()?;
                iroh::SecretKey::from_bytes(&bytes)
            }
            None => iroh::SecretKey::generate(),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().ok()?;
        let inner = Arc::new(Inner { key, endpoint: tokio::sync::OnceCell::new(), live: Arc::default() });
        Some(Self { runtime, inner })
    }
    pub fn secret_hex(&self) -> String {
        hex::encode(self.inner.key.to_bytes())
    }
    pub fn id(&self) -> String {
        self.inner.key.public().to_string()
    }

    /// Dial the Mac and say hello. Replaces any connection already open.
    /// Returns the connection's generation, which every event for it carries:
    /// events can still be on their way when the next connection starts, and
    /// the app drops those from an older one.
    pub fn connect(&self, mac: &str, hello: String, emit: impl Fn(u64, i32, &str) + Send + Sync + 'static) -> u64 {
        self.disconnect();
        let emit = Arc::new(emit);
        let (out, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut live = self.inner.live.lock().unwrap();
        live.0 += 1;
        let generation = live.0;
        let this = self.inner.clone();
        let state = self.inner.live.clone();
        let mac = mac.to_string();
        // Only the current connection speaks.
        let say = {
            let emit = emit.clone();
            move |kind: i32, text: &str| {
                if state.lock().unwrap().0 == generation {
                    emit(generation, kind, text);
                }
            }
        };
        let task = self.runtime.spawn(async move {
            let reason = match this.run(&mac, hello, &mut outgoing, &say).await {
                Ok(()) => "closed".to_string(),
                Err(e) => e,
            };
            say(EVENT_DISCONNECTED, &reason);
        });
        live.1 = Some(Live { out, task });
        generation
    }

    pub fn send(&self, line: &str) -> bool {
        let live = self.inner.live.lock().unwrap();
        live.1.as_ref().is_some_and(|l| l.out.send(line.to_string()).is_ok())
    }

    pub fn disconnect(&self) {
        let mut live = self.inner.live.lock().unwrap();
        if let Some(old) = live.1.take() {
            old.task.abort();
        }
        // Anything still in flight from the old connection is now stale.
        live.0 += 1;
    }
}

impl Inner {
    async fn run(
        &self,
        mac: &str,
        hello: String,
        outgoing: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
        say: &(impl Fn(i32, &str) + Send + Sync),
    ) -> Result<(), String> {
        let id: iroh::EndpointId = mac.parse().map_err(|_| "That isn't a Chat pairing code.".to_string())?;
        let endpoint = self
            .endpoint
            .get_or_try_init(|| async {
                iroh::Endpoint::builder(iroh::endpoint::presets::N0).secret_key(self.key.clone()).bind().await
            })
            .await
            .map_err(|e| format!("Couldn't start networking: {e}"))?;
        let connection = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(id, ALPN))
            .await
            .map_err(|_| "Timed out reaching the Mac.".to_string())?
            .map_err(|e| format!("Couldn't reach the Mac: {e}"))?;
        let (mut send, recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        send.write_all(format!("{hello}\n").as_bytes()).await.map_err(|e| e.to_string())?;
        let direct = |c: &iroh::endpoint::Connection| c.paths().iter().any(|p| p.is_selected() && p.is_ip());
        let name = |d: bool| if d { "direct" } else { "relayed" };
        let mut was = direct(&connection);
        say(EVENT_CONNECTED, name(was));
        let mut reader = BufReader::new(recv);
        let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
        // Reading on its own, so a half-read line survives a write.
        let reading = tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match (&mut reader).take(MAX_LINE).read_line(&mut line).await {
                    Ok(n) if n > 0 && line.ends_with('\n') => {
                        if lines_tx.send(Some(line.trim_end().to_string())).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = lines_tx.send(None);
                        break;
                    }
                }
            }
        });
        let mut check = tokio::time::interval(Duration::from_secs(2));
        let result = loop {
            tokio::select! {
                line = lines.recv() => match line {
                    Some(Some(line)) => say(EVENT_LINE, &line),
                    _ => break Ok(()),
                },
                message = outgoing.recv() => match message {
                    Some(mut text) => {
                        text.push('\n');
                        if let Err(e) = send.write_all(text.as_bytes()).await {
                            break Err(e.to_string());
                        }
                    }
                    None => break Ok(()),
                },
                _ = check.tick() => {
                    let now = direct(&connection);
                    if now != was {
                        was = now;
                        say(EVENT_PATH, name(now));
                    }
                }
                _ = connection.closed() => break Ok(()),
            }
        };
        reading.abort();
        let _ = send.finish();
        connection.close(0u32.into(), b"bye");
        result
    }

}

// ── C ABI ─────────────────────────────────────────────────────────────────

fn string_out(s: String) -> *mut c_char {
    CString::new(s).map_or(std::ptr::null_mut(), CString::into_raw)
}

unsafe fn str_in<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// A bridge with the given hex secret key, or a new key when null. Null if the key is malformed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_new(secret_hex: *const c_char) -> *const c_void {
    let secret = unsafe { str_in(secret_hex) };
    match Bridge::new(secret) {
        Some(b) => Box::into_raw(Box::new(b)) as *const c_void,
        None => std::ptr::null(),
    }
}

unsafe fn bridge<'a>(b: *const c_void) -> &'a Bridge {
    // A `Box<Bridge>` from `chat_bridge_new`, alive until `chat_bridge_free`.
    unsafe { &*(b as *const Bridge) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_secret(b: *const c_void) -> *mut c_char {
    string_out(unsafe { bridge(b) }.secret_hex())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_id(b: *const c_void) -> *mut c_char {
    string_out(unsafe { bridge(b) }.id())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_connect(b: *const c_void, mac: *const c_char, hello: *const c_char, ctx: *mut c_void, callback: Callback) -> u64 {
    let (Some(mac), Some(hello)) = (unsafe { str_in(mac) }, unsafe { str_in(hello) }) else { return 0 };
    let ctx = Ctx(ctx);
    unsafe { bridge(b) }.connect(mac, hello.to_string(), move |generation, kind, text| {
        let ctx = ctx;
        if let Ok(text) = CString::new(text) {
            callback(ctx.0, generation, kind, text.as_ptr());
        }
    })
}

/// Queue a line for the Mac; false when not connected.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_send(b: *const c_void, line: *const c_char) -> bool {
    let Some(line) = (unsafe { str_in(line) }) else { return false };
    unsafe { bridge(b) }.send(line)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_disconnect(b: *const c_void) {
    unsafe { bridge(b) }.disconnect();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_pair_code(mac: *const c_char, phone: *const c_char, secret: *const c_char) -> *mut c_char {
    match (unsafe { str_in(mac) }, unsafe { str_in(phone) }, unsafe { str_in(secret) }) {
        (Some(m), Some(p), Some(s)) => string_out(pair_code(m, p, s)),
        _ => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chat_bridge_free(b: *const c_void) {
    if !b.is_null() {
        let b = unsafe { Box::from_raw(b as *mut Bridge) };
        b.disconnect();
        b.runtime.shutdown_background();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Mac computes the same digits (see `src/remote.rs`).
    #[test]
    fn pair_code_matches_the_mac() {
        assert_eq!(pair_code("mac", "phone", "secret"), "852198");
    }

    #[test]
    fn keys_round_trip() {
        let b = Bridge::new(None).unwrap();
        let again = Bridge::new(Some(&b.secret_hex())).unwrap();
        assert_eq!(b.id(), again.id());
        assert!(Bridge::new(Some("nothex")).is_none());
    }
}
