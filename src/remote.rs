//! Remote access: paired phones reach this Mac over iroh, dialing it by its
//! public key. Traffic is end-to-end encrypted, direct when the two can see
//! each other and through a relay when they can't; the relay only ever sees
//! ciphertext. This module is the transport and what's saved about it. Who
//! gets in, and what they see, the workspace decides.
//!
//! Each connection carries one bidirectional stream of newline-delimited JSON.
//! The phone speaks first with a `hello`, naming itself and, while pairing,
//! the one-time secret from the QR code.
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

/// The protocol a phone must ask for; anything else is turned away in the handshake.
pub const ALPN: &[u8] = b"chat/remote/1";
/// The longest line either side may send. A transcript snapshot can be large;
/// anything past this is a broken or hostile peer.
const MAX_LINE: u64 = 8 * 1024 * 1024;
/// How long a phone has to say hello before it's dropped.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// A phone allowed in: its iroh id (public key) and the name it gave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub paired_at: i64,
    #[serde(default)]
    pub last_seen: Option<i64>,
}

/// What's kept between launches, in `remote.json` beside the settings,
/// readable only by the user. The Mac's secret key lives in the Keychain; it's
/// only here for a throwaway settings folder (`CHAT_CONFIG_DIR`), or from
/// before it moved, until it does.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// Remote access is on. Off by default, and off doesn't unpair anyone.
    pub enabled: bool,
    /// The Mac's iroh secret key, hex, when it's kept in the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Phones let in, by public key: nothing secret.
    pub devices: Vec<Device>,
}

impl Saved {
    fn path() -> Option<PathBuf> {
        crate::settings::dir().map(|d| d.join("remote.json"))
    }
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> anyhow::Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("no settings folder"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

/// Where the Mac's secret key is kept.
const KEYCHAIN_SERVICE: &str = "Chat Remote Access";
const KEYCHAIN_ACCOUNT: &str = "mac-key";

/// Whether the key belongs in the Keychain: yes, unless settings come from a
/// throwaway folder (tests, trying things out), which keeps its own key.
pub fn keychain_enabled() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("CHAT_CONFIG_DIR").is_none()
}

fn parse_key(hex_key: &str) -> Option<[u8; 32]> {
    hex::decode(hex_key.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok())
}

#[cfg(target_os = "macos")]
fn keychain_get() -> Option<[u8; 32]> {
    let bytes = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).ok()?;
    parse_key(std::str::from_utf8(&bytes).ok()?)
}

#[cfg(target_os = "macos")]
fn keychain_set(key: &[u8; 32]) -> bool {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, hex::encode(key).as_bytes()).is_ok()
}

#[cfg(not(target_os = "macos"))]
fn keychain_get() -> Option<[u8; 32]> {
    None
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_: &[u8; 32]) -> bool {
    false
}

/// Where the key came from, and so what's left to do with the file.
pub enum KeySource {
    /// From the Keychain, or just put there: any copy in the file should go.
    Keychain,
    /// Kept in the file: a throwaway settings folder, or the Keychain refused it.
    File,
}

/// The Mac's secret key: from the Keychain, moved there from the file if it
/// was there, or made fresh. May block on a Keychain prompt, so call it off
/// the main thread.
pub fn load_key(in_file: Option<&str>) -> ([u8; 32], KeySource) {
    let keychain: Option<(&dyn Fn() -> Option<[u8; 32]>, &dyn Fn(&[u8; 32]) -> bool)> =
        keychain_enabled().then_some((&keychain_get, &keychain_set));
    load_key_from(in_file, keychain)
}

/// `load_key`, with the Keychain's get and set given (`None` when it isn't used).
fn load_key_from(
    in_file: Option<&str>,
    keychain: Option<(&dyn Fn() -> Option<[u8; 32]>, &dyn Fn(&[u8; 32]) -> bool)>,
) -> ([u8; 32], KeySource) {
    let from_file = in_file.and_then(parse_key);
    let Some((get, set)) = keychain else {
        return (from_file.unwrap_or_else(|| iroh::SecretKey::generate().to_bytes()), KeySource::File);
    };
    if let Some(key) = get() {
        return (key, KeySource::Keychain);
    }
    // Moving from the file keeps the same key, so paired phones still find this Mac.
    let key = from_file.unwrap_or_else(|| iroh::SecretKey::generate().to_bytes());
    if set(&key) { (key, KeySource::Keychain) } else { (key, KeySource::File) }
}

/// The public id for a secret key: what a phone dials.
pub fn public_id(key: &[u8; 32]) -> String {
    iroh::SecretKey::from_bytes(key).public().to_string()
}

/// A fresh one-time pairing secret, for the QR code.
pub fn new_secret() -> String {
    let bytes: [u8; 16] = rand::random();
    data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
}

/// The six digits both sides show while pairing. Both compute them from the
/// Mac's id, the phone's id and the secret, so a match means the phone in your
/// hand is the one asking, not someone else who saw the code.
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

/// Compare secrets without leaking how much of a guess was right.
pub fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The QR code's text: the Mac to dial, the secret, and the Mac's name to show.
pub fn pair_url(mac: &str, secret: &str, name: &str) -> String {
    format!("chat-remote://pair?mac={mac}&secret={secret}&name={}", percent(name))
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// This Mac's name as the user set it, for the phone to show.
pub fn mac_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

pub type ConnId = u64;

/// What the phone says first.
#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    secret: Option<String>,
}

/// What the transport hands up to the workspace.
#[derive(Debug)]
pub enum Incoming {
    /// The endpoint couldn't start.
    Failed(String),
    /// A phone connected and said who it is. Nothing else it sends is acted on
    /// until the workspace lets it in.
    Hello { conn: ConnId, device: String, name: String, secret: Option<String> },
    Line { conn: ConnId, line: String },
    /// The connection moved between a direct path and a relayed one.
    Path { conn: ConnId, direct: bool },
    Closed { conn: ConnId },
}

enum Out {
    Line(String),
    Close,
}

/// The running endpoint. Dropping it closes every connection.
pub struct Server {
    id: String,
    outs: Arc<Mutex<HashMap<ConnId, tokio::sync::mpsc::UnboundedSender<Out>>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Server {
    pub fn start(key: [u8; 32]) -> (Self, UnboundedReceiver<Incoming>) {
        let (tx, rx) = unbounded();
        let outs: Arc<Mutex<HashMap<ConnId, tokio::sync::mpsc::UnboundedSender<Out>>>> = Arc::default();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let id = public_id(&key);
        let thread_outs = outs.clone();
        std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.unbounded_send(Incoming::Failed(e.to_string()));
                        return;
                    }
                };
                runtime.block_on(serve(key, tx, thread_outs, stopped));
            })
            .expect("could not start the remote thread");
        (Self { id, outs, shutdown: Some(stop) }, rx)
    }
    /// What a phone dials: the endpoint's public key.
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn send(&self, conn: ConnId, line: String) {
        if let Some(out) = self.outs.lock().unwrap().get(&conn) {
            let _ = out.send(Out::Line(line));
        }
    }
    pub fn close(&self, conn: ConnId) {
        if let Some(out) = self.outs.lock().unwrap().remove(&conn) {
            let _ = out.send(Out::Close);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
    }
}

async fn serve(
    key: [u8; 32],
    tx: UnboundedSender<Incoming>,
    outs: Arc<Mutex<HashMap<ConnId, tokio::sync::mpsc::UnboundedSender<Out>>>>,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
) {
    let endpoint = match iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(&key))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
    {
        Ok(e) => e,
        Err(e) => {
            let _ = tx.unbounded_send(Incoming::Failed(e.to_string()));
            return;
        }
    };
    let next = Arc::new(AtomicU64::new(1));
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let (tx, outs, next) = (tx.clone(), outs.clone(), next.clone());
                tokio::spawn(async move {
                    let Ok(connection) = incoming.await else { return };
                    let conn = next.fetch_add(1, Ordering::Relaxed);
                    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
                    outs.lock().unwrap().insert(conn, out_tx);
                    handle(conn, connection, &tx, out_rx).await;
                    outs.lock().unwrap().remove(&conn);
                    let _ = tx.unbounded_send(Incoming::Closed { conn });
                });
            }
        }
    }
    endpoint.close().await;
}

/// Whether the connection's chosen path goes straight to the phone.
fn is_direct(connection: &iroh::endpoint::Connection) -> bool {
    connection.paths().iter().any(|p| p.is_selected() && p.is_ip())
}

async fn handle(
    conn: ConnId,
    connection: iroh::endpoint::Connection,
    tx: &UnboundedSender<Incoming>,
    mut out: tokio::sync::mpsc::UnboundedReceiver<Out>,
) {
    let device = connection.remote_id().to_string();
    let Ok((mut send, recv)) = connection.accept_bi().await else { return };
    let mut reader = BufReader::new(recv);
    let mut line = String::new();
    // The phone has a moment to say who it is, in one bounded line.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, (&mut reader).take(64 * 1024).read_line(&mut line)).await;
    let Ok(Ok(n)) = hello else { return };
    if n == 0 {
        return;
    }
    let Ok(hello) = serde_json::from_str::<Hello>(&line) else { return };
    if hello.kind != "hello" {
        return;
    }
    let name: String = hello.name.chars().filter(|c| !c.is_control()).take(64).collect();
    let _ = tx.unbounded_send(Incoming::Hello { conn, device, name, secret: hello.secret });
    let mut direct = is_direct(&connection);
    let _ = tx.unbounded_send(Incoming::Path { conn, direct });
    // Reading runs on its own: a line half read must not be lost to a write.
    let lines = tx.clone();
    let mut reading = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match (&mut reader).take(MAX_LINE).read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) if !line.ends_with('\n') => break,
                Ok(_) => {
                    let _ = lines.unbounded_send(Incoming::Line { conn, line: line.trim_end().to_string() });
                }
            }
        }
    });
    let mut check = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = &mut reading => break,
            message = out.recv() => {
                match message {
                    Some(Out::Line(mut text)) => {
                        text.push('\n');
                        if send.write_all(text.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Some(Out::Close) | None => break,
                }
            }
            _ = check.tick() => {
                let now = is_direct(&connection);
                if now != direct {
                    direct = now;
                    let _ = tx.unbounded_send(Incoming::Path { conn, direct });
                }
            }
            _ = connection.closed() => break,
        }
    }
    reading.abort();
    // Let the last lines (a "rejected" and its reason, say) reach the phone
    // before the connection goes: closing at once can drop what's in flight.
    if send.finish().is_ok() {
        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    }
    connection.close(0u32.into(), b"bye");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_codes_are_six_digits_and_bind_every_part() {
        let code = pair_code("mac", "phone", "secret");
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(code, pair_code("mac", "phone", "secret"));
        assert_ne!(code, pair_code("mac", "other", "secret"));
        assert_ne!(code, pair_code("mac", "phone", "other"));
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
    }

    #[test]
    fn the_key_moves_from_the_file_to_the_keychain() {
        use std::cell::RefCell;
        let file = hex::encode([7u8; 32]);
        let stored: RefCell<Option<[u8; 32]>> = RefCell::new(None);
        let get = || *stored.borrow();
        let set = |k: &[u8; 32]| {
            *stored.borrow_mut() = Some(*k);
            true
        };
        // First launch with the Keychain: the file's key moves, unchanged.
        let (key, source) = load_key_from(Some(&file), Some((&get, &set)));
        assert_eq!(key, [7u8; 32]);
        assert!(matches!(source, KeySource::Keychain));
        assert_eq!(*stored.borrow(), Some([7u8; 32]));
        // After: the Keychain's wins, whatever the file says.
        let (key, _) = load_key_from(Some(&hex::encode([9u8; 32])), Some((&get, &set)));
        assert_eq!(key, [7u8; 32]);
    }

    #[test]
    fn the_file_keeps_the_key_without_the_keychain() {
        let file = hex::encode([3u8; 32]);
        let (key, source) = load_key_from(Some(&file), None);
        assert_eq!(key, [3u8; 32]);
        assert!(matches!(source, KeySource::File));
        // A Keychain that refuses leaves it in the file too.
        let (key, source) = load_key_from(Some(&file), Some((&|| None, &|_: &[u8; 32]| false)));
        assert_eq!(key, [3u8; 32]);
        assert!(matches!(source, KeySource::File));
        // Nothing anywhere: a fresh one.
        let (fresh, _) = load_key_from(None, None);
        assert_ne!(fresh, [0u8; 32]);
    }

    /// Writes to the real login Keychain under a throwaway account, then
    /// removes it. Run by hand: `cargo test keychain_round_trip -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        use security_framework::passwords::*;
        let account = "test-round-trip";
        let _ = delete_generic_password(KEYCHAIN_SERVICE, account);
        set_generic_password(KEYCHAIN_SERVICE, account, b"abc").unwrap();
        assert_eq!(get_generic_password(KEYCHAIN_SERVICE, account).unwrap(), b"abc");
        delete_generic_password(KEYCHAIN_SERVICE, account).unwrap();
        assert!(get_generic_password(KEYCHAIN_SERVICE, account).is_err());
    }

    #[test]
    fn pair_url_escapes_the_name() {
        assert_eq!(pair_url("m", "s", "De's Mac"), "chat-remote://pair?mac=m&secret=s&name=De%27s%20Mac");
    }
}
