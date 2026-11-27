import SwiftUI

/// Home: the Mac's projects, anything waiting on you first.
struct ProjectsView: View {
    @Environment(Store.self) private var store
    @State private var confirmRemove = false
    @State private var adding = false

    var body: some View {
        Group {
            switch store.phase {
            case .unreachable(let reason):
                Unreachable(reason: reason)
            case .rejected(let reason):
                Rejected(reason: reason)
            case .connecting where store.projects.isEmpty:
                ProgressView("Connecting…").frame(maxWidth: .infinity, maxHeight: .infinity)
            default:
                list
            }
        }
        // The list's own grouped background, behind the other states too.
        .background(Color(.systemGroupedBackground))
        .navigationTitle("Projects")
        .navigationDestination(for: ProjectInfo.self) { ThreadsView(project: $0) }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) { macSwitcher }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("Remove this Mac…", role: .destructive) { confirmRemove = true }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .confirmationDialog("Remove \(store.mac?.name ?? "this Mac")?", isPresented: $confirmRemove, titleVisibility: .visible) {
            Button("Remove", role: .destructive) { if let mac = store.mac { store.remove(mac) } }
        } message: {
            Text("To use it again you'll scan a new code. Remove this phone on the Mac too.")
        }
        .sheet(isPresented: $adding) { PairView(adding: true) }
        .alert("Couldn't add the Mac", isPresented: Binding(
            get: { store.pairingProblem != nil },
            set: { if !$0 { store.pairingProblem = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(store.pairingProblem ?? "")
        }
        .onAppear { store.watch(project: nil) }
    }

    /// The current Mac; tap to switch to another or add one.
    private var macSwitcher: some View {
        Menu {
            ForEach(store.macs) { mac in
                Button { store.switchTo(mac) } label: {
                    if mac.id == store.mac?.id {
                        Label(mac.name, systemImage: "checkmark")
                    } else {
                        Text(mac.name)
                    }
                }
            }
            Divider()
            Button { adding = true } label: { Label("Add Mac…", systemImage: "plus") }
        } label: {
            HStack(spacing: 4) {
                macLabel
                Image(systemName: "chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Theme.quiet)
            }
        }
    }

    private var macLabel: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(store.phase == .connected ? Theme.green : Theme.quiet)
                .frame(width: 7, height: 7)
            Text(store.mac?.name ?? "Mac").font(.footnote).foregroundStyle(Theme.muted).lineLimit(1)
            if store.phase == .connected {
                Pill(text: store.direct ? "Direct" : "Relayed",
                     fg: store.direct ? Theme.green : Theme.amber,
                     bg: store.direct ? Theme.greenTint : Theme.amberTint)
            }
        }
        .fixedSize()
    }

    private var list: some View {
        let waiting = store.projects.filter { $0.waiting > 0 }
        let chats = store.projects.first { $0.key == "chats" }
        let projects = store.projects.filter { $0.key != "chats" }
        return List {
            if !waiting.isEmpty {
                Section("Needs you") {
                    ForEach(waiting) { row($0, needs: true) }
                }
            }
            // Chats aren't a project: they get their own place, above the folders.
            if let chats {
                Section {
                    row(chats, needs: false)
                }
            }
            if !projects.isEmpty {
                Section("Projects") {
                    ForEach(projects) { row($0, needs: false) }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { store.watch(project: nil) }
    }

    private func row(_ p: ProjectInfo, needs: Bool) -> some View {
        NavigationLink(value: p) {
            HStack(spacing: 12) {
                if p.key == "chats" {
                    ChatsTile()
                } else {
                    ProjectTile(project: p)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text(p.name).lineLimit(1)
                    Text(subtitle(p, needs: needs))
                        .font(.caption)
                        .foregroundStyle(needs ? Theme.amber : Theme.muted)
                        .lineLimit(1)
                }
                Spacer()
                if p.waiting > 0 {
                    badge(p.waiting, Theme.orange)
                } else if p.running > 0 {
                    ProgressView().controlSize(.mini).tint(Theme.orange)
                } else if p.unread > 0 {
                    badge(p.unread, Theme.blue)
                }
            }
        }
    }

    private func subtitle(_ p: ProjectInfo, needs: Bool) -> String {
        if needs { return p.waiting == 1 ? "A thread needs you" : "\(p.waiting) threads need you" }
        // The spinner says it's working; the line says on what.
        if p.running > 0, let latest = p.latest { return latest }
        if let at = p.activeAt { return relative(at).capitalized }
        return "No threads yet"
    }

    private func badge(_ n: Int, _ color: Color) -> some View {
        Text("\(n)")
            .font(.caption2.weight(.semibold))
            .foregroundStyle(.white)
            .frame(minWidth: 18, minHeight: 18)
            .padding(.horizontal, 3)
            .background(color, in: Capsule())
    }
}

/// The Mac can't be reached; retrying meanwhile.
struct Unreachable: View {
    @Environment(Store.self) private var store
    let reason: String

    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            Image(systemName: "laptopcomputer.slash")
                .font(.system(size: 28))
                .foregroundStyle(Theme.muted)
                .frame(width: 64, height: 64)
                .background(Theme.panel, in: RoundedRectangle(cornerRadius: 18))
                .overlay(RoundedRectangle(cornerRadius: 18).stroke(Theme.border))
                .padding(.bottom, 18)
            Text("Can't reach your Mac").font(.title3.bold()).padding(.bottom, 6)
            Text("\(reason) Keep the Mac awake and Chat open, with remote access on.")
                .font(.subheadline)
                .foregroundStyle(Theme.muted)
                .multilineTextAlignment(.center)
                .padding(.bottom, 18)
            Button { store.resume() } label: {
                Text("Try again").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            if let last = store.lastConnected {
                Text("Last connected \(last.formatted(.relative(presentation: .named)))")
                    .font(.caption2)
                    .foregroundStyle(Theme.quiet)
                    .padding(.top, 12)
            }
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 34)
    }
}

/// The Mac turned this phone away.
struct Rejected: View {
    @Environment(Store.self) private var store
    let reason: String

    var body: some View {
        VStack(spacing: 14) {
            Spacer()
            Image(systemName: "hand.raised").font(.system(size: 32)).foregroundStyle(Theme.red)
            Text("Not allowed in").font(.title3.bold())
            Text(reason).font(.subheadline).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
            Text("Scan a new code from it to pair again, or remove it here.")
                .font(.footnote).foregroundStyle(Theme.quiet).multilineTextAlignment(.center)
            Button("Remove this Mac", role: .destructive) { if let mac = store.mac { store.remove(mac) } }
                .buttonStyle(.bordered)
            Spacer()
            Spacer()
        }
        .padding(.horizontal, 34)
    }
}
