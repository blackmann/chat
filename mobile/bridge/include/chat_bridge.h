// The iPhone app's iroh client, implemented in Rust (mobile/bridge/src/lib.rs).
#pragma once
#include <stdbool.h>
#include <stdint.h>

#define CHAT_EVENT_CONNECTED 1
#define CHAT_EVENT_LINE 2
#define CHAT_EVENT_DISCONNECTED 3
#define CHAT_EVENT_PATH 4
#define CHAT_EVENT_FETCH_PROGRESS 5
#define CHAT_EVENT_FETCH_DONE 6
#define CHAT_EVENT_FETCH_FAILED 7

/// Called on a background thread. `text` is valid only for the call. `generation`
/// is what chat_bridge_connect returned for the connection the event is about.
typedef void (*chat_bridge_callback)(void *ctx, uint64_t generation, int32_t kind, const char *text);

/// A bridge with the given hex secret key, or a new key when null. Null if the key is malformed.
const void *chat_bridge_new(const char *secret_hex);
/// The secret key, hex, to keep in the Keychain. Free with chat_bridge_free_string.
char *chat_bridge_secret(const void *bridge);
/// This phone's public id. Free with chat_bridge_free_string.
char *chat_bridge_id(const void *bridge);
/// Dial the Mac and send `hello` as the first line. Replaces any open connection.
/// Returns the new connection's generation; events from older ones may still
/// arrive afterwards and should be dropped.
uint64_t chat_bridge_connect(const void *bridge, const char *mac_id, const char *hello, void *ctx, chat_bridge_callback callback);
/// Queue a line for the Mac; false when not connected.
bool chat_bridge_send(const void *bridge, const char *line);
/// Close the connection; no events follow for it.
void chat_bridge_disconnect(const void *bridge);
/// Fetch a file the Mac shared, by its token, into `dest` (a file path); with
/// `thumb`, a small picture of it instead. Bytes go to `dest.part` until done,
/// and a later fetch picks up from it. Events come to `callback` with the
/// returned id in place of a generation: FETCH_PROGRESS "received total",
/// FETCH_DONE the Mac's header (JSON), FETCH_FAILED why. 0 if arguments are bad.
uint64_t chat_bridge_fetch(const void *bridge, const char *token, bool thumb, const char *dest, void *ctx, chat_bridge_callback callback);
/// Call a download off; no more events come for it. What it got is kept.
void chat_bridge_cancel_fetch(const void *bridge, uint64_t id);
/// The six-digit pairing code. Free with chat_bridge_free_string.
char *chat_bridge_pair_code(const char *mac_id, const char *phone_id, const char *secret);
void chat_bridge_free_string(char *s);
void chat_bridge_free(const void *bridge);
