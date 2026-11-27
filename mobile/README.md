# Chat for iPhone

A SwiftUI companion that reaches Chat on your Mac over [iroh](https://iroh.computer):
phone and Mac dial each other by public key, end-to-end encrypted, directly when
they can see each other and through a relay when they can't. Relays only see
ciphertext.

The phone can browse projects and threads, read transcripts, send messages, start
threads, stop turns, answer permission requests and answer the agent's questions.
Pictures and videos a message points at (screenshots, screen recordings) show
under it and open full screen; the Mac sends only files a transcript names, each
on its own stream, and a download that breaks off resumes. It can't change a thread's mode,
model or effort; raising what an agent may do stays on the Mac.

## Build

1. `./scripts/build-bridge.sh` builds the Rust iroh client (`bridge/`) into
   `ChatBridge.xcframework`. Run it again whenever `bridge/` changes.
2. Open `Chat.xcodeproj`, pick your team under Signing (and a bundle id of your own),
   then run on a device or simulator.

## Pair

On the Mac, click the phone icon above Settings on the projects rail, turn remote
access on and choose **Pair device…**. Scan the QR code from the app (or with the
Camera app), check both screens show the same six digits, and accept on the Mac.
Paired phones are listed in the same popover, where they can be removed.

A phone can pair with several Macs: tap the Mac's name at the top of Projects to
switch, or choose **Add Mac…** from it. One Mac is connected at a time;
the app reopens on the last one used.

The Mac keeps its secret key in the login Keychain ("Chat Remote Access") and the
paired devices, which are only public keys, in `remote.json` in Chat's settings
folder. With `CHAT_CONFIG_DIR` set (tests, throwaway settings) the key stays in that
folder's `remote.json` instead. The phone keeps its key and the Mac in its Keychain.

## Layout

- `Chat/`: the SwiftUI app.
- `bridge/`: the iroh client behind a small C ABI (`bridge/include/chat_bridge.h`).
  `cargo run --example probe -- <mac-id> [secret]` dials a Mac from the terminal.
- `design.html`: the screens as designed.
- The Mac side is `src/remote.rs` (transport) and `src/workspace/remote.rs`
  (who's let in, what they see, the devices popover).

## Trying it without a camera

Debug builds read these at launch: `CHAT_PAIR_URL` pairs from a link (adding a Mac
if one is paired), `CHAT_SWITCH_TO` starts on the paired Mac whose id begins with it,
`CHAT_RESET_MACS` forgets every Mac, `CHAT_OPEN_PROJECT` / `CHAT_OPEN_THREAD`
open a screen, and `CHAT_OPEN_PANEL` (with `CHAT_PANEL_TAB=changes`) opens a thread's
context panel. They also note what happened in the app's `Documents/debug.log`. With the simulator:
`SIMCTL_CHILD_CHAT_PAIR_URL='chat-remote://pair?…' xcrun simctl launch booted dev.chat.remote`.
