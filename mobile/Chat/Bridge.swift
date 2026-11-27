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

    /// What a download says: how far it got, then that it's done or why not.
    enum FetchEvent {
        case progress(received: Int64, total: Int64)
        case done
        case failed(String)
    }
    /// Downloads under way, by id. Only touched on the main thread.
    private var fetches: [UInt64: (FetchEvent) -> Void] = [:]
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

    /// Fetch a file the Mac shared into `dest`, or with `thumb` a small picture
    /// of it. A download that broke off resumes from what it got. Call on the
    /// main thread; `onEvent` is called there too.
    @discardableResult
    func fetch(token: String, thumb: Bool, to dest: URL, onEvent: @escaping (FetchEvent) -> Void) -> UInt64 {
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let id = token.withCString { t in
            dest.path.withCString { d in
                chat_bridge_fetch(handle, t, thumb, d, ctx) { ctx, id, kind, text in
                    guard let ctx, let text else { return }
                    let message = String(cString: text)
                    let bridge = Unmanaged<Bridge>.fromOpaque(ctx).takeUnretainedValue()
                    let event: FetchEvent
                    switch kind {
                    case CHAT_EVENT_FETCH_PROGRESS:
                        let parts = message.split(separator: " ").compactMap { Int64($0) }
                        event = .progress(received: parts.first ?? 0, total: parts.last ?? 0)
                    case CHAT_EVENT_FETCH_DONE: event = .done
                    default: event = .failed(message)
                    }
                    DispatchQueue.main.async {
                        guard let handler = bridge.fetches[id] else { return }
                        if case .progress = event {} else { bridge.fetches[id] = nil }
                        handler(event)
                    }
                }
            }
        }
        if id == 0 {
            onEvent(.failed("That isn't a file the Mac shared."))
        } else {
            fetches[id] = onEvent
        }
        return id
    }

    /// Stop a download; what it got is kept to resume from.
    func cancelFetch(_ id: UInt64) {
        fetches[id] = nil
        chat_bridge_cancel_fetch(handle, id)
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
