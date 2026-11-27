import ChatBridge
import Foundation

/// The Rust iroh client, wrapped. One lives for the app's lifetime: its
/// address is the context the Rust side calls back with.
final class Bridge {
    enum Event {
        case connected(direct: Bool)
        case line(String)
        case path(direct: Bool)
        case disconnected(String)
    }

    private let handle: UnsafeRawPointer
    /// The connection events are wanted for; anything from an older one is dropped.
    private var current: UInt64 = 0
    /// Events, delivered on the main thread.
    var onEvent: ((Event) -> Void)?

    /// With the saved key, or a fresh one (saved for next time).
    init() {
        if let saved = Keychain.read(Keychain.phoneKey), let h = saved.withCString({ chat_bridge_new($0) }) {
            handle = h
        } else {
            handle = chat_bridge_new(nil)!
            if let key = Bridge.take(chat_bridge_secret(handle)) {
                Keychain.write(Keychain.phoneKey, key)
            }
        }
    }

    deinit { chat_bridge_free(handle) }

    /// This phone's public id: what the Mac knows it by.
    var id: String { Bridge.take(chat_bridge_id(handle)) ?? "" }

    func connect(mac: String, hello: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: hello),
              let text = String(data: data, encoding: .utf8) else { return }
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        current = mac.withCString { m in
            text.withCString { h in
                chat_bridge_connect(handle, m, h, ctx) { ctx, generation, kind, text in
                    guard let ctx, let text else { return }
                    let message = String(cString: text)
                    let bridge = Unmanaged<Bridge>.fromOpaque(ctx).takeUnretainedValue()
                    let event: Event
                    switch kind {
                    case CHAT_EVENT_CONNECTED: event = .connected(direct: message == "direct")
                    case CHAT_EVENT_LINE: event = .line(message)
                    case CHAT_EVENT_PATH: event = .path(direct: message == "direct")
                    default: event = .disconnected(message)
                    }
                    DispatchQueue.main.async {
                        guard generation == bridge.current else { return }
                        bridge.onEvent?(event)
                    }
                }
            }
        }
    }

    @discardableResult
    func send(_ object: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: object),
              let text = String(data: data, encoding: .utf8) else { return false }
        return text.withCString { chat_bridge_send(handle, $0) }
    }

    func disconnect() {
        current = 0
        chat_bridge_disconnect(handle)
    }

    static func pairCode(mac: String, phone: String, secret: String) -> String {
        mac.withCString { m in phone.withCString { p in secret.withCString { s in
            take(chat_bridge_pair_code(m, p, s)) ?? ""
        } } }
    }

    /// A string the Rust side allocated, copied and freed.
    private static func take(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p else { return nil }
        defer { chat_bridge_free_string(p) }
        return String(cString: p)
    }
}
