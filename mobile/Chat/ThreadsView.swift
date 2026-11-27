import SwiftUI

/// A project's threads, grouped by day as on the Mac.
struct ThreadsView: View {
    @Environment(Store.self) private var store
    let project: ProjectInfo
    @State private var search = ""
    @State private var composing = false
    @State private var path: [Int] = []

    var body: some View {
        let threads = (store.threads[project.key] ?? []).filter {
            search.isEmpty || $0.title.localizedCaseInsensitiveContains(search)
        }
        List {
            ForEach(groups(threads), id: \.0) { day, items in
                Section(day) {
                    ForEach(items) { t in
                        NavigationLink(value: t.id) { row(t) }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .overlay {
            if store.threads[project.key] == nil {
                ProgressView()
            } else if threads.isEmpty && search.isEmpty {
                ContentUnavailableView("No threads yet", systemImage: "bubble.left", description: Text("Start one with the pencil."))
            }
        }
        .searchable(text: $search, prompt: "Search threads")
        .navigationTitle(project.name)
        .navigationDestination(for: Int.self) { ThreadView(threadId: $0, projectKey: project.key) }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { composing = true } label: { Image(systemName: "square.and.pencil") }
            }
        }
        .sheet(isPresented: $composing) {
            NewThreadSheet(project: project)
        }
        .onAppear { store.watch(project: project.key) }
    }

    private func row(_ t: ThreadInfo) -> some View {
        HStack(spacing: 12) {
            StateMark(state: t.state, unread: t.unread).frame(width: 14)
            VStack(alignment: .leading, spacing: 1) {
                Text(t.title).lineLimit(1)
                if t.state == "failed" {
                    Text(t.problem.map { "Failed · \($0)" } ?? "Failed")
                        .font(.caption).foregroundStyle(Theme.red).lineLimit(1)
                }
            }
            Spacer()
            if t.waiting {
                Pill(text: "Needs you", fg: Theme.amber, bg: Theme.amberTint)
            }
        }
    }

    private func groups(_ threads: [ThreadInfo]) -> [(String, [ThreadInfo])] {
        let calendar = Calendar.current
        var out: [(String, [ThreadInfo])] = []
        for t in threads {
            let date = t.activeAt.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) }
            let day: String
            if let date, calendar.isDateInToday(date) { day = "Today" }
            else if let date, calendar.isDateInYesterday(date) { day = "Yesterday" }
            else { day = "Older" }
            if out.last?.0 == day { out[out.count - 1].1.append(t) } else { out.append((day, [t])) }
        }
        return out
    }
}

/// A new thread's first message.
struct NewThreadSheet: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let project: ProjectInfo
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            TextField("Ask for a change, or how something works…", text: $text, axis: .vertical)
                .lineLimit(4...12)
                .focused($focused)
                .padding()
                .frame(maxHeight: .infinity, alignment: .top)
                .navigationTitle("New thread in \(project.name)")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Send") {
                            store.newThread(in: project.key, text: text)
                            dismiss()
                        }
                        .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
        }
        .presentationDetents([.medium, .large])
        .onAppear { focused = true }
    }
}
