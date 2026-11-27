//! Dial a Mac running Chat and print what it says, for trying remote access
//! without the phone app.
//!
//!     cargo run --example probe -- <mac-id> [pairing-secret] [key-file]
//!
//! With a secret it pairs (accept on the Mac); after that, run it again
//! without one, with the same key file, to connect as the paired device.
use chat_bridge::{Bridge, EVENT_DISCONNECTED, pair_code};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--id-of <hex key>`: the public id for a secret key.
    if args.first().map(String::as_str) == Some("--id-of") {
        println!("{}", Bridge::new(Some(args[1].trim())).expect("bad key").id());
        return;
    }
    let mac = args.first().expect("usage: probe <mac-id> [secret] [key-file]").clone();
    let secret = args.get(1).filter(|s| !s.is_empty()).cloned();
    let key_file = args.get(2).cloned().unwrap_or_else(|| "/tmp/chat-probe.key".into());
    let saved = std::fs::read_to_string(&key_file).ok();
    let bridge = Bridge::new(saved.as_deref().map(str::trim)).expect("bad key");
    std::fs::write(&key_file, bridge.secret_hex()).expect("couldn't save the key");
    println!("phone id: {}", bridge.id());
    if let Some(s) = &secret {
        println!("expected code: {}", pair_code(&mac, &bridge.id(), s));
    }
    let hello = match &secret {
        Some(s) => format!(r#"{{"type":"hello","name":"Probe","secret":"{s}"}}"#),
        None => r#"{"type":"hello","name":"Probe"}"#.to_string(),
    };
    let (tx, rx) = std::sync::mpsc::channel::<(i32, String)>();
    bridge.connect(&mac, hello, move |_, kind, text| {
        let _ = tx.send((kind, text.to_string()));
    });
    let mut viewed = false;
    while let Ok((kind, text)) = rx.recv_timeout(Duration::from_secs(120)) {
        // `PROBE_FULL=1`: print whole lines.
        let shown: String = if std::env::var("PROBE_FULL").is_ok() { text.clone() } else { text.chars().take(400).collect() };
        println!("[{kind}] {shown}");
        if kind == EVENT_DISCONNECTED {
            break;
        }
        // `PROBE_ALLOW=1`: follow the thread PROBE_NEW made and allow what it asks.
        if std::env::var("PROBE_ALLOW").is_ok() {
            if let Some(id) = text.split(r#""type":"created","thread":"#).nth(1).and_then(|r| r.split('}').next()) {
                bridge.send(&format!(r#"{{"type":"view","thread":{id}}}"#));
            }
            if text.contains(r#""type":"thread""#)
                && let Some(rest) = text.split(r#""kind":"permission","#).nth(1)
                && rest.contains(r#""chosen":null"#)
                && let Some(perm) = rest.split(r#""id":"#).nth(1).and_then(|r| r.split(',').next())
                && let Some(thread) = text.split(r#""thread":{"id":"#).nth(1).and_then(|r| r.split(',').next())
            {
                bridge.send(&format!(r#"{{"type":"answer","thread":{thread},"permission":{perm},"answer":"yes"}}"#));
            }
        }
        // Once in, look at the first project's threads, then the first thread.
        if !viewed && text.contains(r#""type":"projects""#) {
            viewed = true;
            // `PROBE_VIEW=<thread id>`: look at that thread.
            if let Ok(id) = std::env::var("PROBE_VIEW") {
                bridge.send(&format!(r#"{{"type":"view","thread":{id}}}"#));
                continue;
            }
            // `PROBE_NEW=<text>`: start a thread in the last project with it.
            if let Ok(new) = std::env::var("PROBE_NEW")
                && let Some(key) = text.split(r#""key":""#).last().and_then(|r| r.split('"').next())
            {
                bridge.send(&format!(r#"{{"type":"new_thread","project":"{key}","text":"{new}"}}"#));
            }
            if let Some(key) = text.split(r#""key":""#).nth(1).and_then(|r| r.split('"').next()) {
                bridge.send(&format!(r#"{{"type":"view","project":"{key}"}}"#));
            }
        } else if text.contains(r#""type":"threads""#) && !text.contains(r#""threads":[]"#)
            && let Some(id) = text.split(r#""id":"#).nth(1).and_then(|r| r.split(',').next())
            && std::env::var("PROBE_THREAD").is_err()
        {
            unsafe { std::env::set_var("PROBE_THREAD", id) };
            bridge.send(&format!(r#"{{"type":"view","project":null,"thread":{id}}}"#));
        }
    }
}
