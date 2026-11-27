import SwiftUI
import UIKit

/// One thread's transcript, its composer, and any permission it waits on.
struct ThreadView: View {
    @Environment(Store.self) private var store
    let threadId: Int
    let projectKey: String
    @State private var draft = ""
    @State private var openTools: Set<Int> = []
    /// The waiting permission or questions put away, by block, until "Review" or "Answer" brings them back.
    @State private var dismissedPrompt: Int?

    private var thread: ThreadDetail? { store.thread?.id == threadId ? store.thread : nil }

    @Environment(\.dismiss) private var dismiss
    /// The context panel is showing: the page is pushed aside.
    @State private var panelOpen = false
    /// How far the page is being dragged right now.
    @State private var drag: CGFloat = 0
    /// Which way the current drag turned out to go; only sideways moves the page.
    @State private var sideways: Bool?
    /// How much of the page stays in view while the panel shows.
    private let peek: CGFloat = 64
    /// The keyboard is up: the composer sits on it, not above the home indicator.
    @State private var keyboard = false

    var body: some View {
        GeometryReader { geo in
            let travel = geo.size.width - peek
            let offset = min(0, max(-travel, (panelOpen ? -travel : 0) + drag))
            let progress = travel > 0 ? -offset / travel : 0
            ZStack(alignment: .topLeading) {
                // Underneath: revealed as the page slides away, easing in from the right.
                ContextPanel(thread: thread, changes: store.changes)
                    .padding(.leading, peek)
                    .padding(.top, geo.safeAreaInsets.top)
                    .offset(x: (1 - progress) * 40)
                    .opacity(0.3 + 0.7 * progress)
                page(top: geo.safeAreaInsets.top, bottom: keyboard ? 6 : geo.safeAreaInsets.bottom)
                    .background(Theme.background)
                    .clipShape(RoundedRectangle(cornerRadius: progress > 0.01 ? 38 : 0, style: .continuous))
                    .shadow(color: .black.opacity(0.18 * progress), radius: 24, x: 8)
                    .overlay {
                        // While pushed aside, a tap on what shows of the page brings it back.
                        if panelOpen {
                            Color.clear.contentShape(Rectangle()).onTapGesture { setPanel(false) }
                        }
                    }
                    .offset(x: offset)
                    .simultaneousGesture(slide(travel: travel))
            }
            // Edge to edge, as the page is a card that slides; the keyboard still
            // pushes it up, since only the container's insets are ignored.
            .ignoresSafeArea(.container, edges: .vertical)
            .background(Color(.systemGroupedBackground))
        }
        .toolbar(.hidden, for: .navigationBar)
        .sheet(item: Binding(
            get: { (thread?.openPermission ?? thread?.openAsk).flatMap { $0.index == dismissedPrompt ? nil : $0 } },
            set: { if $0 == nil { dismissedPrompt = (thread?.openPermission ?? thread?.openAsk)?.index } }
        )) { prompt in
            if prompt.kind == "permission" {
                PermissionSheet(block: prompt, cwd: thread?.cwd) { answer in
                    store.answer(thread: threadId, permission: prompt.permissionId ?? 0, answer: answer)
                    dismissedPrompt = prompt.index
                }
            } else {
                AskSheet(block: prompt, threadId: threadId)
            }
        }
        .onAppear {
            store.watch(thread: threadId, in: projectKey)
            #if DEBUG
            if ProcessInfo.processInfo.environment["CHAT_OPEN_PANEL"] != nil { panelOpen = true }
            #endif
        }
        .onDisappear { store.unwatch(thread: threadId) }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in keyboard = true }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in keyboard = false }
    }

    /// The thread itself: header, transcript, and the composer floating over it.
    private func page(top: CGFloat, bottom: CGFloat) -> some View {
        VStack(spacing: 0) {
            header(top: top)
            if let thread {
                transcript(thread)
                    .safeAreaInset(edge: .bottom) { composer(thread).padding(.bottom, bottom) }
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    /// Drawn on the page rather than in the navigation bar, so it slides with it.
    private func header(top: CGFloat) -> some View {
        HStack(spacing: 10) {
            Button { dismiss() } label: {
                Image(systemName: "chevron.left").font(.system(size: 17, weight: .semibold))
                    .frame(width: 44, height: 44)
                    .glass(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Back")
            VStack(spacing: 1) {
                Text(thread?.title ?? "Thread").font(.subheadline.weight(.semibold)).lineLimit(1)
                Text([thread?.projectName, thread?.agent].compactMap { $0 }.joined(separator: " · "))
                    .font(.caption2).foregroundStyle(Theme.muted).lineLimit(1)
            }
            .frame(maxWidth: .infinity)
            Button { setPanel(!panelOpen) } label: {
                Image(systemName: "sidebar.right").font(.system(size: 16, weight: .medium))
                    .frame(width: 44, height: 44)
                    .glass(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Context")
        }
        .foregroundStyle(Theme.text)
        .padding(.horizontal, 14)
        .padding(.top, top + 2)
        .padding(.bottom, 6)
    }

    /// Sideways drags slide the page: left reveals the panel, right puts it back,
    /// or goes back a screen when it's already closed. Upright drags are left to scrolling.
    private func slide(travel: CGFloat) -> some Gesture {
        DragGesture(minimumDistance: 12)
            .onChanged { value in
                if sideways == nil {
                    sideways = abs(value.translation.width) > abs(value.translation.height) * 1.4
                }
                guard sideways == true else { return }
                let dx = value.translation.width
                // Closed, a rightward drag is for going back: the page doesn't move for it.
                drag = panelOpen ? max(0, dx) : min(0, dx)
            }
            .onEnded { value in
                defer { sideways = nil }
                guard sideways == true else { return }
                let dx = value.predictedEndTranslation.width
                if panelOpen {
                    setPanel(!(dx > travel * 0.3))
                } else if value.translation.width > 80 && dx > 160 {
                    drag = 0
                    dismiss()
                } else {
                    setPanel(dx < -travel * 0.3)
                }
            }
    }

    private func setPanel(_ open: Bool) {
        withAnimation(.spring(response: 0.38, dampingFraction: 0.86)) {
            panelOpen = open
            drag = 0
        }
    }

    private func transcript(_ thread: ThreadDetail) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 16) {
                    if thread.truncated {
                        Text("Earlier messages are on the Mac.").font(.caption).foregroundStyle(Theme.quiet)
                            .frame(maxWidth: .infinity)
                    }
                    ForEach(thread.blocks) { block in
                        BlockView(block: block, open: openTools.contains(block.index)) {
                            if openTools.contains(block.index) { openTools.remove(block.index) } else { openTools.insert(block.index) }
                        } reopen: {
                            dismissedPrompt = nil
                        }
                    }
                    if thread.busy {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.mini).tint(Theme.orange)
                            Text(thread.state == "waiting" ? "Waiting on you" : "Working")
                                .font(.footnote).foregroundStyle(Theme.muted)
                        }
                    }
                    if let problem = thread.problem {
                        Text(problem).font(.footnote).foregroundStyle(Theme.red)
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            .onChange(of: thread.blocks.count) { proxy.scrollTo("end", anchor: .bottom) }
        }
    }

    /// A glass card floating over the transcript: what's typed on top, and below
    /// it the thread's mode, model and effort, and send (or stop while it works).
    private func composer(_ thread: ThreadDetail) -> some View {
        let empty = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return VStack(alignment: .leading, spacing: 12) {
            TextField(thread.busy ? "Queue a message" : "Reply", text: $draft, axis: .vertical)
                .lineLimit(1...6)
                .font(.body)
            HStack(spacing: 10) {
                if let mode = thread.mode {
                    Text(mode).font(.footnote).foregroundStyle(Theme.muted).lineLimit(1)
                }
                if thread.queued > 0 {
                    Text("\(thread.queued) queued").font(.footnote).foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 8)
                (Text(thread.model ?? "").fontWeight(.semibold).foregroundColor(Theme.text)
                    + Text(thread.effort.map { " \($0)" } ?? "").foregroundColor(Theme.muted))
                    .font(.subheadline)
                    .lineLimit(1)
                if thread.busy && empty {
                    Button { store.stop(threadId) } label: {
                        Image(systemName: "stop.fill").font(.system(size: 13))
                            .frame(width: 36, height: 36)
                            .foregroundStyle(Theme.background)
                            .background(Theme.text, in: Circle())
                    }
                    .accessibilityLabel("Stop")
                } else {
                    Button {
                        store.send(draft, to: threadId)
                        draft = ""
                    } label: {
                        Image(systemName: "arrow.up").font(.system(size: 15, weight: .bold))
                            .frame(width: 36, height: 36)
                            .foregroundStyle(.white)
                            .background(empty ? Theme.quiet.opacity(0.6) : Theme.blue, in: Circle())
                    }
                    .disabled(empty)
                    .accessibilityLabel("Send")
                }
            }
        }
        .padding(.leading, 18)
        .padding(.trailing, 10)
        .padding(.top, 16)
        .padding(.bottom, 10)
        .glass(RoundedRectangle(cornerRadius: 28, style: .continuous))
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
    }
}

/// One part of a transcript.
struct BlockView: View {
    let block: Block
    let open: Bool
    let toggle: () -> Void
    let reopen: () -> Void

    var body: some View {
        switch block.kind {
        case "user":
            VStack(alignment: .trailing, spacing: 3) {
                Text(block.text ?? "")
                    .lineSpacing(3)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Theme.panel, in: RoundedRectangle(cornerRadius: 14))
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.border))
                if let at = block.at {
                    Text(Date(timeIntervalSince1970: TimeInterval(at) / 1000).formatted(date: .omitted, time: .shortened))
                        .font(.caption2).foregroundStyle(Theme.quiet)
                }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 40)
        case "text":
            Markdown(text: block.text ?? "")
        case "tool":
            tool
        case "permission":
            permission
        case "ask":
            ask
        case "notice":
            Text(block.text ?? "").font(.footnote).foregroundStyle(Theme.muted)
        case "ended":
            Text(ended).font(.caption2).foregroundStyle(Theme.quiet)
        default:
            EmptyView()
        }
    }

    private var tool: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    Text(glyph).font(Theme.mono).foregroundStyle(Theme.quiet)
                    Text(block.title ?? "Tool").font(Theme.mono).foregroundStyle(Theme.text).lineLimit(open ? nil : 1)
                    if let adds = block.adds, let dels = block.dels, adds + dels > 0 {
                        Pill(text: "+\(adds) −\(dels)", fg: Theme.green, bg: Theme.greenTint)
                    }
                    if block.status == "Failed" {
                        Image(systemName: "xmark.circle").font(.caption).foregroundStyle(Theme.red)
                    } else if block.status == "Running" || block.status == "Pending" {
                        ProgressView().controlSize(.mini)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: open ? "chevron.down" : "chevron.right").font(.caption2).foregroundStyle(Theme.quiet)
                }
            }
            .buttonStyle(.plain)
            if open {
                ForEach(block.files ?? [], id: \.self) { Text($0).font(.caption).foregroundStyle(Theme.muted) }
                if let output = block.output, !output.isEmpty {
                    ScrollView(.horizontal) {
                        Text(output).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted)
                            .textSelection(.enabled)
                    }
                    .padding(8)
                    .background(Theme.chrome, in: RoundedRectangle(cornerRadius: 8))
                }
            }
        }
    }

    private var glyph: String {
        switch block.tool {
        case "Execute": return ">_"
        case "Edit", "Delete", "Move": return "✎"
        case "Read": return "◱"
        case "Search": return "⌕"
        case "Fetch": return "↓"
        default: return "•"
        }
    }

    private var permission: some View {
        HStack(spacing: 8) {
            Image(systemName: "checkmark.shield").foregroundStyle(block.chosen == nil ? Theme.blue : Theme.quiet)
            VStack(alignment: .leading, spacing: 1) {
                Text(block.title ?? "Permission").font(.footnote).lineLimit(2)
                Text(block.chosen ?? "Waiting for your answer").font(.caption2)
                    .foregroundStyle(block.chosen == nil ? Theme.amber : Theme.muted)
            }
            Spacer()
            if block.chosen == nil {
                Button("Review", action: reopen).font(.footnote.weight(.semibold))
            }
        }
        .padding(10)
        .background(block.chosen == nil ? Theme.blueTint : Theme.panel, in: RoundedRectangle(cornerRadius: 10))
    }

    private var ask: some View {
        let waiting = block.open == true
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: "questionmark.bubble").foregroundStyle(waiting ? Theme.blue : Theme.quiet)
                Text(block.text.flatMap { $0.isEmpty ? nil : $0 } ?? block.questions?.first?.label ?? "Questions")
                    .font(.footnote).lineLimit(3)
                Spacer()
                if waiting {
                    Button("Answer", action: reopen).font(.footnote.weight(.semibold))
                }
            }
            if let answered = block.answered {
                ForEach(Array(answered.enumerated()), id: \.offset) { _, pair in
                    Text("\(pair.first ?? ""): \(pair.last ?? "")").font(.caption).foregroundStyle(Theme.muted)
                }
            } else if block.skipped == true {
                Text("Skipped").font(.caption).foregroundStyle(Theme.muted)
            }
        }
        .padding(10)
        .background(waiting ? Theme.blueTint : Theme.panel, in: RoundedRectangle(cornerRadius: 10))
    }

    private var ended: String {
        let secs = (block.workedMs ?? 0) / 1000
        let time = block.at.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000).formatted(date: .omitted, time: .shortened) } ?? ""
        return "Worked for \(secs / 60)m \(String(format: "%02d", secs % 60))s · \(time)"
    }
}

/// Agent prose: paragraphs and lists with inline Markdown, code blocks in mono.
struct Markdown: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(parts.enumerated()), id: \.offset) { _, part in
                if part.code {
                    ScrollView(.horizontal) {
                        Text(part.text).font(.system(size: 12, design: .monospaced)).lineSpacing(2).textSelection(.enabled)
                    }
                    .padding(10)
                    .background(Theme.chrome, in: RoundedRectangle(cornerRadius: 8))
                } else {
                    paragraph(part.text.components(separatedBy: "\n"))
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var parts: [(code: Bool, text: String)] {
        var out: [(Bool, String)] = []
        var code = false
        var buffer: [String] = []
        func flush() {
            let joined = buffer.joined(separator: "\n").trimmingCharacters(in: .newlines)
            if !joined.isEmpty { out.append((code, joined)) }
            buffer = []
        }
        for line in text.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                flush()
                code.toggle()
            } else if !code && line.trimmingCharacters(in: .whitespaces).isEmpty {
                flush()
            } else {
                buffer.append(line)
            }
        }
        flush()
        return out
    }

    /// One block of prose: a heading (and what follows it), a table, a quote, a list, or text.
    @ViewBuilder
    private func paragraph(_ lines: [String]) -> some View {
        let trimmed = lines.map { $0.trimmingCharacters(in: .whitespaces) }
        if let first = trimmed.first, let r = first.range(of: #"^#{1,6} "#, options: .regularExpression) {
            VStack(alignment: .leading, spacing: 8) {
                Text(inline(String(first[r.upperBound...]))).font(.headline).padding(.top, 4)
                if lines.count > 1 { AnyView(paragraph(Array(lines.dropFirst()))) }
            }
        } else if trimmed.allSatisfy({ $0.hasPrefix("|") }) {
            table(trimmed)
        } else if trimmed.allSatisfy({ $0.hasPrefix(">") }) {
            let text = trimmed.map { String($0.dropFirst()).trimmingCharacters(in: .whitespaces) }.joined(separator: "\n")
            Text(inline(text))
                .font(.callout).lineSpacing(4).foregroundStyle(Theme.muted)
                .padding(.leading, 12)
                .overlay(alignment: .leading) { Capsule().fill(Theme.border).frame(width: 3) }
                .textSelection(.enabled)
        } else if lines.contains(where: Markdown.isItem) {
            // A list: each item on its own, its wrapped lines hanging under the text.
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    if let (mark, rest) = Markdown.item(line) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(mark).foregroundStyle(Theme.muted).frame(minWidth: 12, alignment: .leading)
                            Text(inline(rest)).lineSpacing(4)
                        }
                    } else {
                        Text(inline(line)).lineSpacing(4)
                    }
                }
            }
            .font(.callout)
            .textSelection(.enabled)
        } else {
            Text(inline(lines.joined(separator: "\n"))).font(.callout).lineSpacing(4).textSelection(.enabled)
        }
    }

    /// A Markdown table as a grid: header row in bold, the `---` row dropped.
    private func table(_ lines: [String]) -> some View {
        let rows = lines
            .filter { !$0.replacingOccurrences(of: "|", with: "").allSatisfy { "-: ".contains($0) } }
            .map { line in
                line.trimmingCharacters(in: CharacterSet(charactersIn: "| "))
                    .components(separatedBy: "|")
                    .map { $0.trimmingCharacters(in: .whitespaces) }
            }
        return ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 8) {
                ForEach(Array(rows.enumerated()), id: \.offset) { n, cells in
                    GridRow {
                        ForEach(Array(cells.enumerated()), id: \.offset) { _, cell in
                            Text(inline(cell)).fontWeight(n == 0 ? .semibold : .regular)
                        }
                    }
                    if n == 0 { Divider() }
                }
            }
            .font(.footnote)
            .padding(10)
        }
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.border))
    }

    static func isItem(_ line: String) -> Bool { item(line) != nil }

    /// A list item's mark (• or "1.") and its text.
    static func item(_ line: String) -> (String, String)? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") { return ("•", String(trimmed.dropFirst(2))) }
        if let r = trimmed.range(of: #"^\d+[.)] "#, options: .regularExpression) {
            return (String(trimmed[r]).trimmingCharacters(in: .whitespaces), String(trimmed[r.upperBound...]))
        }
        return nil
    }

    private func inline(_ s: String) -> AttributedString {
        // Bullets and headings read plainly; the rest is inline Markdown.
        let lines = s.components(separatedBy: "\n").map { line -> String in
            var l = line
            if let r = l.range(of: #"^\s*[-*] "#, options: .regularExpression) { l.replaceSubrange(r, with: "•  ") }
            if let r = l.range(of: #"^#{1,6} "#, options: .regularExpression) { l.replaceSubrange(r, with: "") }
            return l
        }.joined(separator: "\n")
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: lines, options: options)) ?? AttributedString(lines)
    }
}

/// The agent asks before it acts: the full command, never truncated.
struct PermissionSheet: View {
    let block: Block
    let cwd: String?
    let answer: (String?) -> Void

    private var heading: String {
        switch block.tool {
        case "Execute": return "Run a command?"
        case "Edit", "Delete", "Move": return "Allow this change?"
        case "Fetch": return "Fetch from the web?"
        default: return "Allow this?"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(heading).font(.title3.bold())
            Text(block.title ?? "").font(.subheadline).foregroundStyle(Theme.muted).padding(.top, 4)
            if let detail = block.detail, detail != block.title {
                ScrollView {
                    Text(detail).font(Theme.mono).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                }
                .frame(maxHeight: 160)
                .padding(10)
                .background(Theme.background, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.border))
                .padding(.top, 12)
            }
            if let cwd {
                HStack {
                    Text("Directory").foregroundStyle(Theme.muted)
                    Spacer()
                    Text(cwd).font(Theme.mono).lineLimit(1).truncationMode(.head)
                }
                .font(.caption)
                .padding(.top, 10)
            }
            VStack(spacing: 8) {
                ForEach(Array((block.answers ?? []).enumerated()), id: \.offset) { i, a in
                    Button { answer(a.id) } label: {
                        Text(a.name).font(.body.weight(.semibold)).frame(maxWidth: .infinity).padding(.vertical, 4)
                    }
                    .modifier(AnswerStyle(allow: a.allow, first: i == 0))
                }
            }
            .padding(.top, 16)
            Text("Bypass-permissions mode can only be turned on from the Mac.")
                .font(.caption2).foregroundStyle(Theme.quiet).frame(maxWidth: .infinity).padding(.top, 12)
        }
        .padding(20)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

private struct AnswerStyle: ViewModifier {
    let allow: Bool
    let first: Bool

    func body(content: Content) -> some View {
        if allow && first {
            content.buttonStyle(.borderedProminent).controlSize(.large)
        } else if allow {
            content.buttonStyle(.bordered).controlSize(.large)
        } else {
            content.buttonStyle(.bordered).controlSize(.large).tint(Theme.red)
        }
    }
}
