import SwiftUI

/// What sits under a thread's page, revealed by swiping it left: the Mac's
/// Context page (the context window, plan, files, instructions) and git's
/// changes. Lists only for now; nothing opens a detail view yet.
struct ContextPanel: View {
    enum Tab: String, CaseIterable {
        case context = "Context"
        case changes = "Changes"
    }

    let thread: ThreadDetail?
    let changes: Changes?
    @State private var tab: Tab = {
        #if DEBUG
        // Checking a tab without tapping.
        if ProcessInfo.processInfo.environment["CHAT_PANEL_TAB"] == "changes" { return .changes }
        #endif
        return .context
    }()

    var body: some View {
        VStack(spacing: 0) {
            Picker("Panel", selection: $tab) {
                ForEach(Tab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.top, 8)
            .padding(.bottom, 10)
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    switch tab {
                    case .context: context
                    case .changes: changesList
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 6)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .background(Color(.systemGroupedBackground))
    }

    // MARK: Context

    @ViewBuilder
    private var context: some View {
        if let c = thread?.context {
            if let used = c.used, let size = c.size, size > 0 {
                meter(used: used, size: size, cost: c.cost, currency: c.currency)
            }
            if !c.plan.isEmpty {
                let done = c.plan.filter { $0.step == "done" }.count
                section("Plan", detail: "\(done) of \(c.plan.count) done") {
                    ForEach(Array(c.plan.enumerated()), id: \.offset) { _, step in
                        row {
                            switch step.step {
                            case "done": Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.green)
                            case "doing" where thread?.busy == true: ProgressView().controlSize(.mini)
                            default: Image(systemName: "circle.dashed").foregroundStyle(Theme.quiet)
                            }
                        } label: {
                            Text(step.text).foregroundStyle(step.step == "done" ? Theme.muted : Theme.text)
                        }
                    }
                }
            }
            if !c.files.isEmpty {
                section("Files", detail: "\(c.files.count)") {
                    ForEach(c.files, id: \.path) { file in
                        row {
                            Image(systemName: "doc.text")
                                .foregroundStyle(file.touch == "read" ? Theme.muted : Theme.amber)
                        } label: {
                            PathLabel(path: file.path)
                        } trailing: {
                            touchNote(file)
                        }
                    }
                }
            }
            if !c.instructions.isEmpty {
                section("Instructions", detail: "\(c.instructions.count)") {
                    ForEach(c.instructions, id: \.path) { file in
                        row {
                            Image(systemName: "text.document").foregroundStyle(Theme.muted)
                        } label: {
                            PathLabel(path: file.path)
                        } trailing: {
                            Text(file.scope).font(.caption).foregroundStyle(Theme.muted)
                        }
                    }
                }
            }
            if c.plan.isEmpty && c.files.isEmpty && c.instructions.isEmpty && c.used == nil {
                empty("Nothing yet", "The plan and the files the agent reads or changes show here.")
            }
        } else {
            ProgressView().frame(maxWidth: .infinity).padding(.top, 40)
        }
    }

    /// How full the context window is, warming from blue as it fills, as on the Mac.
    private func meter(used: Int, size: Int, cost: Double?, currency: String?) -> some View {
        let share = min(1, Double(used) / Double(size))
        return VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .lastTextBaseline, spacing: 6) {
                Text(tokens(used)).font(.title2.weight(.medium))
                Text("of \(tokens(size)) tokens").font(.footnote).foregroundStyle(Theme.muted)
                Spacer()
                Text("\(Int((share * 100).rounded()))%").font(.footnote).foregroundStyle(Theme.muted)
            }
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Theme.border)
                    Capsule()
                        .fill(LinearGradient(colors: [Theme.blue, share > 0.7 ? Theme.red : Theme.blue], startPoint: .leading, endPoint: .trailing))
                        .frame(width: max(6, geo.size.width * share))
                }
            }
            .frame(height: 6)
            HStack {
                Text("\(tokens(max(0, size - used))) left")
                Spacer()
                if let cost { Text(money(cost, currency)) }
            }
            .font(.caption)
            .foregroundStyle(Theme.muted)
        }
        .padding(14)
        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 14))
    }

    @ViewBuilder
    private func touchNote(_ file: ThreadContext.File) -> some View {
        switch file.touch {
        case "read": Text("read").font(.caption).foregroundStyle(Theme.muted)
        case "created": Text("new").font(.caption).foregroundStyle(Theme.green)
        default:
            if let adds = file.adds, let dels = file.dels {
                Stat(adds: adds, dels: dels)
            } else {
                Text("edited").font(.caption).foregroundStyle(Theme.amber)
            }
        }
    }

    // MARK: Changes

    @ViewBuilder
    private var changesList: some View {
        if let changes, !changes.repos.isEmpty {
            ForEach(changes.repos, id: \.name) { repo in
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.triangle.branch").foregroundStyle(Theme.muted)
                        Text(repo.branch ?? repo.name).font(.subheadline.weight(.semibold)).lineLimit(1)
                        if changes.repos.count > 1 {
                            Text(repo.name).font(.caption).foregroundStyle(Theme.muted)
                        }
                        Spacer()
                        if let ahead = repo.ahead, ahead > 0 {
                            Label("\(ahead)", systemImage: "arrow.up").font(.caption).foregroundStyle(Theme.muted)
                        }
                    }
                    if let error = repo.error {
                        Text(error).font(.footnote).foregroundStyle(Theme.muted)
                    } else {
                        let staged = repo.staged ?? []
                        let unstaged = repo.unstaged ?? []
                        if staged.isEmpty && unstaged.isEmpty {
                            Text("No changes").font(.footnote).foregroundStyle(Theme.muted)
                        }
                        if !staged.isEmpty {
                            section("Staged", detail: "\(staged.count)") { files(staged) }
                        }
                        if !unstaged.isEmpty {
                            section("Changes", detail: "\(unstaged.count)") { files(unstaged) }
                        }
                    }
                }
            }
        } else if changes == nil || changes?.loading == true {
            ProgressView().frame(maxWidth: .infinity).padding(.top, 40)
        } else {
            empty("No folder", "Chats aren't tied to a project, so there's nothing in git to show.")
        }
    }

    private func files(_ list: [Changes.File]) -> some View {
        ForEach(list, id: \.path) { file in
            row {
                Text(file.status == "?" ? "U" : file.status)
                    .font(.system(size: 12, weight: .bold, design: .monospaced))
                    .foregroundStyle(statusColor(file.status))
            } label: {
                PathLabel(path: file.path)
            } trailing: {
                if let adds = file.adds, let dels = file.dels { Stat(adds: adds, dels: dels) }
            }
        }
    }

    private func statusColor(_ s: String) -> Color {
        switch s {
        case "A", "?": return Theme.green
        case "D", "U": return Theme.red
        case "R": return Theme.blue
        default: return Theme.amber
        }
    }

    // MARK: Pieces

    private func section<Content: View>(_ title: String, detail: String?, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Text(title).font(.footnote.weight(.semibold)).foregroundStyle(Theme.muted)
                if let detail { Text(detail).font(.footnote).foregroundStyle(Theme.quiet) }
            }
            .padding(.bottom, 6)
            .padding(.leading, 4)
            VStack(alignment: .leading, spacing: 0) { content() }
                .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 14))
        }
    }

    private func row<Icon: View, Label: View, Trailing: View>(
        @ViewBuilder icon: () -> Icon,
        @ViewBuilder label: () -> Label,
        @ViewBuilder trailing: () -> Trailing = { EmptyView() }
    ) -> some View {
        HStack(spacing: 10) {
            icon().font(.footnote).frame(width: 18)
            label().font(.subheadline).lineLimit(1)
            Spacer(minLength: 4)
            trailing()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    private func empty(_ title: String, _ detail: String) -> some View {
        VStack(spacing: 6) {
            Text(title).font(.subheadline.weight(.semibold))
            Text(detail).font(.footnote).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 40)
        .padding(.horizontal, 20)
    }

    private func tokens(_ n: Int) -> String {
        n >= 1000 ? "\(n / 1000)k" : "\(n)"
    }

    private func money(_ amount: Double, _ currency: String?) -> String {
        amount.formatted(.currency(code: currency ?? "USD"))
    }
}

/// A file's name, with the folder it's in after it, quieter.
struct PathLabel: View {
    let path: String

    var body: some View {
        let parts = path.split(separator: "/")
        let name = parts.last.map(String.init) ?? path
        let dir = parts.dropLast().joined(separator: "/")
        HStack(spacing: 6) {
            Text(name).lineLimit(1).layoutPriority(1)
            if !dir.isEmpty {
                Text(dir).foregroundStyle(Theme.quiet).lineLimit(1).truncationMode(.head)
            }
        }
    }
}

/// Lines added and removed.
struct Stat: View {
    let adds: Int
    let dels: Int

    var body: some View {
        HStack(spacing: 4) {
            if adds > 0 { Text("+\(adds)").foregroundStyle(Theme.green) }
            if dels > 0 { Text("−\(dels)").foregroundStyle(Theme.red) }
        }
        .font(.system(size: 12, design: .monospaced))
    }
}
