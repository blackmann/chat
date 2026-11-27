import Foundation
import Observation
import UIKit

/// A paired Mac, as kept in the Keychain.
struct PairedMac: Codable, Equatable, Identifiable, Hashable {
    var id: String
    var name: String
}

/// Everything the app shows, fed by the connection to the Mac.
@MainActor
@Observable
final class Store {
    enum Phase: Equatable {
        /// No Mac yet: scan one.
        case unpaired
        /// Dialing the Mac with the scanned secret, then waiting for it to accept.
        case pairing(mac: String, code: String?, expected: String)
        case connecting
        case connected
        /// Can't reach it right now; retrying.
        case unreachable(String)
        /// The Mac said no: not paired, or removed.
        case rejected(String)
    }

    var phase: Phase = .unpaired
    /// Every Mac this phone is paired with, and the one it's talking to.
    var macs: [PairedMac] = []
    var mac: PairedMac?
    /// Why adding another Mac didn't work, shown over the current one.
    var pairingProblem: String?
    var direct = false
    var projects: [ProjectInfo] = []
    var threads: [String: [ThreadInfo]] = [:]
    var thread: ThreadDetail?
    /// Git's changes for the thread showing.
    var changes: Changes?
    var lastConnected: Date?
    /// A thread the Mac just made at our request, to open.
    var created: Int?
    var error: String?
    /// Why the last answers to a set of questions weren't taken, by its id.
    var askErrors: [UInt64: String] = [:]

    @ObservationIgnored private let bridge = Bridge()
    @ObservationIgnored private var retry: Task<Void, Never>?
    @ObservationIgnored private var attempts = 0
    /// What's showing, re-sent after a reconnect.
    @ObservationIgnored private var viewing: (project: String?, thread: Int?) = (nil, nil)
    @ObservationIgnored private var pairing: PairingLink?

    init() {
        bridge.onEvent = { [weak self] event in
            MainActor.assumeIsolated { self?.handle(event) }
        }
        macs = Store.loadMacs()
        Store.debugLog("phone=\(bridge.id) launch macs=\(macs.map { String($0.id.prefix(8)) }) current=\(UserDefaults.standard.string(forKey: Store.currentKey)?.prefix(8) ?? "-")")
        #if DEBUG
        let env = ProcessInfo.processInfo.environment
        // Checking it without tapping: start over, or start on another Mac.
        if env["CHAT_RESET_MACS"] != nil {
            macs = []
            Store.saveMacs([])
        }
        if let pick = env["CHAT_SWITCH_TO"], let found = macs.first(where: { $0.id.hasPrefix(pick) }) {
            UserDefaults.standard.set(found.id, forKey: Store.currentKey)
        }
        #endif
        let current = UserDefaults.standard.string(forKey: Store.currentKey)
        if let mac = macs.first(where: { $0.id == current }) ?? macs.first {
            self.mac = mac
            connect()
        }
        #if DEBUG
        // For trying it without a camera: pair from a link given at launch.
        if let link = ProcessInfo.processInfo.environment["CHAT_PAIR_URL"].flatMap(PairingLink.init),
           !macs.contains(where: { $0.id == link.mac }) {
            _ = pair(ProcessInfo.processInfo.environment["CHAT_PAIR_URL"]!)
        }
        #endif
    }

    var phoneName: String { UIDevice.current.name }

    /// Debug builds note what happened in Documents/debug.log, to check flows without a debugger.
    static func debugLog(_ text: String) {
        #if DEBUG
        guard let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let url = dir.appendingPathComponent("debug.log")
        let line = "\(Date().formatted(date: .omitted, time: .standard)) \(text)\n"
        if let handle = try? FileHandle(forWritingTo: url) {
            handle.seekToEndOfFile()
            handle.write(Data(line.utf8))
            try? handle.close()
        } else {
            try? Data(line.utf8).write(to: url)
        }
        #endif
    }

    // MARK: Paired Macs

    private static let currentKey = "current-mac"

    /// The paired Macs from the Keychain; a phone that paired one before
    /// there could be several has it moved into the list.
    private static func loadMacs() -> [PairedMac] {
        if let saved = Keychain.read(Keychain.macs), let data = saved.data(using: .utf8),
           let list = try? JSONDecoder().decode([PairedMac].self, from: data) {
            return list
        }
        guard let saved = Keychain.read(Keychain.mac), let data = saved.data(using: .utf8),
              let one = try? JSONDecoder().decode(PairedMac.self, from: data) else { return [] }
        saveMacs([one])
        Keychain.delete(Keychain.mac)
        return [one]
    }

    private static func saveMacs(_ list: [PairedMac]) {
        if let data = try? JSONEncoder().encode(list), let text = String(data: data, encoding: .utf8) {
            Keychain.write(Keychain.macs, text)
        }
    }

    /// Talk to another paired Mac instead.
    func switchTo(_ next: PairedMac) {
        guard next != mac || phase != .connected else { return }
        retry?.cancel()
        bridge.disconnect()
        projects = []
        threads = [:]
        thread = nil
        askErrors = [:]
        viewing = (nil, nil)
        direct = false
        lastConnected = nil
        mac = next
        UserDefaults.standard.set(next.id, forKey: Store.currentKey)
        attempts = 0
        phase = .connecting
        connect()
    }

    /// Forget a Mac. It keeps this phone listed until removed there.
    func remove(_ gone: PairedMac) {
        macs.removeAll { $0.id == gone.id }
        Store.saveMacs(macs)
        guard gone.id == mac?.id else { return }
        if let next = macs.first {
            switchTo(next)
        } else {
            retry?.cancel()
            bridge.disconnect()
            UserDefaults.standard.removeObject(forKey: Store.currentKey)
            mac = nil
            projects = []
            threads = [:]
            thread = nil
            phase = .unpaired
        }
    }

    // MARK: Pairing

    /// Start pairing from a scanned (or pasted) link: the first Mac, another
    /// one, or one paired before that turned this phone away.
    func pair(_ text: String) -> Bool {
        guard let link = PairingLink(text) else { return false }
        Store.debugLog("pair \(link.mac.prefix(8)) macs=\(macs.map { String($0.id.prefix(8)) })")
        retry?.cancel()
        pairingProblem = nil
        pairing = link
        let expected = Bridge.pairCode(mac: link.mac, phone: bridge.id, secret: link.secret)
        phase = .pairing(mac: link.name, code: nil, expected: expected)
        bridge.connect(mac: link.mac, hello: ["type": "hello", "name": phoneName, "secret": link.secret])
        return true
    }

    func cancelPairing() {
        pairing = nil
        bridge.disconnect()
        phase = mac == nil ? .unpaired : .connecting
        if mac != nil { connect() }
    }

    /// Pairing didn't work: back to the Mac we had, saying why, or to the start.
    private func pairingFailed(_ reason: String) {
        Store.debugLog("pairing failed: \(reason)")
        pairing = nil
        if mac != nil {
            pairingProblem = reason
            phase = .connecting
            connect()
        } else {
            phase = .rejected(reason)
        }
    }

    // MARK: Connection

    func connect() {
        guard let mac, pairing == nil else { return }
        retry?.cancel()
        if phase != .connected { phase = .connecting }
        bridge.connect(mac: mac.id, hello: ["type": "hello", "name": phoneName])
    }

    /// Back in the foreground: make sure we're connected.
    func resume() {
        if case .connected = phase { return }
        if case .rejected = phase { return }
        attempts = 0
        connect()
    }

    private func scheduleRetry() {
        retry?.cancel()
        attempts += 1
        let delay = min(30.0, pow(2.0, Double(min(attempts, 5))))
        retry = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            self?.connect()
        }
    }

    private func handle(_ event: Bridge.Event) {
        switch event {
        case .connected(let direct), .path(let direct):
            self.direct = direct
        case .line(let line):
            receive(line)
        case .disconnected(let reason):
            if pairing != nil {
                pairingFailed(Store.friendly(reason))
                return
            }
            if case .rejected = phase { return }
            guard mac != nil else { return }
            phase = .unreachable(Store.friendly(reason))
            scheduleRetry()
        }
    }

    private static func friendly(_ reason: String) -> String {
        if reason == "closed" { return "The Mac closed the connection." }
        if reason.contains("No addressing information") || reason.contains("Timed out") {
            return "It may be asleep, offline, or Chat isn't open."
        }
        return reason
    }

    private func receive(_ line: String) {
        guard let data = line.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = object["type"] as? String else { return }
        switch type {
        case "pairing":
            if case .pairing(let name, _, let expected) = phase {
                phase = .pairing(mac: name, code: object["code"] as? String, expected: expected)
            }
        case "paired":
            savePairing(macName: object["mac"] as? String)
            didConnect()
        case "welcome" where pairing != nil:
            // A Mac that still knows this phone lets it straight back in.
            savePairing(macName: object["mac"] as? String)
            didConnect()
        case "welcome":
            if let name = object["mac"] as? String, var mac, mac.name != name {
                mac.name = name
                self.mac = mac
                if let i = macs.firstIndex(where: { $0.id == mac.id }) {
                    macs[i].name = name
                    Store.saveMacs(macs)
                }
            }
            didConnect()
        case "rejected":
            let reason = (object["reason"] as? String) ?? "The Mac turned this phone away."
            if pairing != nil {
                pairingFailed(reason)
                return
            }
            phase = .rejected(reason)
            retry?.cancel()
        case "path":
            direct = (object["direct"] as? Bool) ?? false
        case "projects":
            if let list = object["projects"], let decoded = decode([ProjectInfo].self, list) { projects = decoded }
        case "threads":
            if let key = object["project"] as? String, let list = object["threads"],
               let decoded = decode([ThreadInfo].self, list) { threads[key] = decoded }
        case "thread":
            if let detail = object["thread"], let decoded = decode(ThreadDetail.self, detail),
               decoded.id == viewing.thread { thread = decoded }
        case "changes":
            if let decoded = decode(Changes.self, object), decoded.thread == viewing.thread { changes = decoded }
        case "created":
            created = object["thread"] as? Int
        case "ask_error":
            if let ask = (object["ask"] as? NSNumber)?.uint64Value {
                askErrors[ask] = object["message"] as? String
            }
        case "error":
            error = object["message"] as? String
        default:
            break
        }
    }

    /// The Mac being paired let us in: keep it, and talk to it from now on.
    private func savePairing(macName: String?) {
        if let link = pairing {
            let paired = PairedMac(id: link.mac, name: macName ?? link.name)
            macs.removeAll { $0.id == paired.id }
            macs.append(paired)
            Store.saveMacs(macs)
            UserDefaults.standard.set(paired.id, forKey: Store.currentKey)
            if mac?.id != paired.id {
                projects = []
                threads = [:]
                thread = nil
                viewing = (nil, nil)
            }
            mac = paired
            pairing = nil
        }
    }

    private func didConnect() {
        #if DEBUG
        Store.debugLog("connected=\(mac?.id.prefix(8) ?? "-") macs=\(macs.map { String($0.id.prefix(8)) })")
        #endif
        phase = .connected
        attempts = 0
        lastConnected = .now
        sendView()
    }

    private func decode<T: Decodable>(_ type: T.Type, _ value: Any) -> T? {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return nil }
        return try? decoder.decode(type, from: data)
    }

    // MARK: Requests

    /// Follow a project's thread list. Kept after leaving it: harmless, and
    /// screens appear and disappear in no reliable order.
    func watch(project: String?) {
        viewing.project = project
        sendView()
    }

    /// Follow a thread's transcript while its screen shows.
    func watch(thread: Int, in project: String) {
        viewing = (project, thread)
        if self.thread?.id != thread {
            self.thread = nil
            self.changes = nil
        }
        sendView()
    }

    func unwatch(thread: Int) {
        guard viewing.thread == thread else { return }
        viewing.thread = nil
        sendView()
    }

    private func sendView() {
        guard phase == .connected else { return }
        var request: [String: Any] = ["type": "view"]
        if let p = viewing.project { request["project"] = p }
        if let t = viewing.thread { request["thread"] = t }
        bridge.send(request)
    }

    func send(_ text: String, to thread: Int) {
        bridge.send(["type": "send", "thread": thread, "text": text])
    }

    func newThread(in project: String, text: String) {
        created = nil
        bridge.send(["type": "new_thread", "project": project, "text": text])
    }

    func stop(_ thread: Int) {
        bridge.send(["type": "stop", "thread": thread])
    }

    /// Answer the agent's questions: choices by field, typed text by field; or skip them.
    func reply(thread: Int, ask: UInt64, picks: [String: [String]], typed: [String: String], skip: Bool) {
        askErrors[ask] = nil
        bridge.send(["type": "reply", "thread": thread, "ask": ask, "picks": picks, "typed": typed, "skip": skip])
    }

    func answer(thread: Int, permission: UInt64, answer: String?) {
        var request: [String: Any] = ["type": "answer", "thread": thread, "permission": permission]
        request["answer"] = answer ?? NSNull()
        bridge.send(request)
    }
}
