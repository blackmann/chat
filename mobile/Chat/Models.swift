import Foundation

/// What the Mac sends, as it sends it (see src/workspace/remote.rs).
struct ProjectInfo: Decodable, Identifiable, Hashable {
    let key: String
    let name: String
    let lane: Int
    let waiting: Int
    let running: Int
    let unread: Int
    let latest: String?
    let activeAt: Int64?
    var id: String { key }

    var initials: String {
        let letters = name.filter { $0.isLetter || $0.isNumber }
        return String(letters.prefix(2)).uppercased()
    }
}

struct ThreadInfo: Decodable, Identifiable, Hashable {
    let id: Int
    let title: String
    let state: String
    let unread: Bool
    let waiting: Bool
    let problem: String?
    let activeAt: Int64?
}

struct Answer: Decodable, Hashable {
    let id: String
    let name: String
    let allow: Bool
}

struct Choice: Decodable, Hashable {
    let value: String
    let label: String
    let description: String?
}

/// One of the agent's questions.
struct Question: Decodable, Hashable {
    let field: String
    let title: String?
    let prompt: String?
    /// single, multi, text, yes, number.
    let kind: String
    let integer: Bool
    let required: Bool
    let choices: [Choice]?

    var label: String { title ?? prompt ?? field }
}

struct Block: Decodable, Identifiable, Hashable {
    let kind: String
    let index: Int
    let text: String?
    let at: Int64?
    // Tools
    let title: String?
    let tool: String?
    let status: String?
    let output: String?
    let files: [String]?
    let adds: Int?
    let dels: Int?
    // Permissions
    let permissionId: UInt64?
    let detail: String?
    let answers: [Answer]?
    let chosen: String?
    // Questions; `permissionId` is their id too.
    let open: Bool?
    let questions: [Question]?
    let answered: [[String]]?
    let skipped: Bool?
    // Turn ends
    let workedMs: Int64?

    var id: Int { index }

    enum CodingKeys: String, CodingKey {
        case kind, index, text, at, title, tool, status, output, files, adds, dels
        case permissionId = "id"
        case detail, answers, chosen, open, questions, answered, skipped
        case workedMs
    }
}

/// What the Mac's Context page shows for a thread.
struct ThreadContext: Decodable, Hashable {
    struct Step: Decodable, Hashable {
        /// done, doing, todo.
        let step: String
        let text: String
    }
    struct File: Decodable, Hashable {
        let path: String
        /// read, edited, created.
        let touch: String
        let adds: Int?
        let dels: Int?
    }
    struct Instruction: Decodable, Hashable {
        let path: String
        let scope: String
    }
    let used: Int?
    let size: Int?
    let cost: Double?
    let currency: String?
    let plan: [Step]
    let files: [File]
    let instructions: [Instruction]
}

/// Git's changes in the folders a thread works in.
struct Changes: Decodable, Hashable {
    struct File: Decodable, Hashable {
        let path: String
        /// Git's letter: M, A, D, R, ?, U.
        let status: String
        let adds: Int?
        let dels: Int?
    }
    struct Repo: Decodable, Hashable {
        let name: String
        let branch: String?
        let upstream: String?
        let ahead: Int?
        let staged: [File]?
        let unstaged: [File]?
        let error: String?
    }
    let thread: Int
    let repos: [Repo]
    let loading: Bool
}

struct ThreadDetail: Decodable, Identifiable, Hashable {
    let id: Int
    let project: String
    let projectName: String
    let cwd: String?
    let title: String
    let state: String
    let agent: String?
    let settings: [String]
    let model: String?
    let effort: String?
    let mode: String?
    let context: ThreadContext?
    let queued: Int
    let problem: String?
    let truncated: Bool
    let blocks: [Block]

    var busy: Bool { state == "running" || state == "waiting" }

    /// The questions the agent is waiting on, if any.
    var openAsk: Block? {
        blocks.last { $0.kind == "ask" && $0.open == true }
    }

    /// The permission the agent is waiting on, if any.
    var openPermission: Block? {
        blocks.last { $0.kind == "permission" && $0.chosen == nil }
    }
}

/// The parts of the pairing QR code: `chat-remote://pair?mac=…&secret=…&name=…`.
struct PairingLink {
    let mac: String
    let secret: String
    let name: String

    init?(_ text: String) {
        guard let url = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              url.scheme == "chat-remote", url.host == "pair" else { return nil }
        let items = Dictionary(uniqueKeysWithValues: (url.queryItems ?? []).map { ($0.name, $0.value ?? "") })
        guard let mac = items["mac"], !mac.isEmpty, let secret = items["secret"], !secret.isEmpty else { return nil }
        self.mac = mac
        self.secret = secret
        self.name = items["name"].flatMap { $0.isEmpty ? nil : $0 } ?? "Mac"
    }
}

let decoder: JSONDecoder = {
    let d = JSONDecoder()
    d.keyDecodingStrategy = .convertFromSnakeCase
    return d
}()
