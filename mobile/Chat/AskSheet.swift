import SwiftUI

/// The agent's questions, answered from the phone: a list for one or many
/// choices, a toggle for yes or no, a field for text and numbers. A choice
/// list's free-text companion (`<field>_custom`) shows under it as "Other".
struct AskSheet: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let block: Block
    let threadId: Int
    @State private var picks: [String: [String]] = [:]
    @State private var typed: [String: String] = [:]
    @State private var sent = false

    private var questions: [Question] { block.questions ?? [] }
    private var askId: UInt64 { block.permissionId ?? 0 }

    /// Questions shown on their own: companions sit under their list.
    private var shown: [Question] {
        questions.filter { q in !(q.kind == "text" && companionOwner(q) != nil) }
    }

    private func companionOwner(_ q: Question) -> Question? {
        guard q.field.hasSuffix("_custom") else { return nil }
        let base = String(q.field.dropLast("_custom".count))
        return questions.first { $0.field == base && $0.choices != nil }
    }

    private func companion(of q: Question) -> Question? {
        questions.first { $0.field == q.field + "_custom" && $0.kind == "text" }
    }

    var body: some View {
        NavigationStack {
            Form {
                if let message = block.text, !message.isEmpty {
                    Section { Text(message) }
                }
                ForEach(shown, id: \.field) { q in
                    Section {
                        control(q)
                        if let other = companion(of: q) {
                            TextField(other.title ?? "Other", text: binding(other.field))
                        }
                    } header: {
                        Text(q.title ?? "")
                    } footer: {
                        if let prompt = q.prompt, q.title != nil { Text(prompt) }
                    }
                }
                if let error = store.askErrors[askId] {
                    Section { Text(error).foregroundStyle(Theme.red) }
                }
            }
            .navigationTitle("Questions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Skip") {
                        store.reply(thread: threadId, ask: askId, picks: [:], typed: [:], skip: true)
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") {
                        sent = true
                        store.reply(thread: threadId, ask: askId, picks: picks, typed: typed, skip: false)
                    }
                    .fontWeight(.semibold)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        // Taken: the Mac closes the questions and the sheet goes with them.
        // Turned back: the error shows and the sheet stays.
        .onChange(of: store.askErrors[askId]) { _, error in if error != nil { sent = false } }
    }

    @ViewBuilder
    private func control(_ q: Question) -> some View {
        switch q.kind {
        case "single", "multi":
            ForEach(q.choices ?? [], id: \.value) { choice in
                let on = picks[q.field, default: []].contains(choice.value)
                Button {
                    toggle(q, choice.value)
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(choice.label).foregroundStyle(Theme.text)
                            if let d = choice.description { Text(d).font(.caption).foregroundStyle(Theme.muted) }
                        }
                        Spacer()
                        Image(systemName: on ? (q.kind == "multi" ? "checkmark.square.fill" : "checkmark.circle.fill")
                                             : (q.kind == "multi" ? "square" : "circle"))
                            .foregroundStyle(on ? Theme.blue : Theme.quiet)
                    }
                }
            }
        case "yes":
            Toggle(q.prompt ?? q.label, isOn: Binding(
                get: { picks[q.field]?.first == "true" },
                set: { picks[q.field] = [$0 ? "true" : "false"] }
            ))
        case "number":
            TextField(q.prompt ?? q.label, text: binding(q.field))
                .keyboardType(q.integer ? .numberPad : .decimalPad)
        default:
            TextField(q.prompt ?? q.label, text: binding(q.field), axis: .vertical)
                .lineLimit(1...6)
        }
    }

    /// As on the Mac: one-of picks replace, tapping again clears; many-of toggle.
    private func toggle(_ q: Question, _ value: String) {
        var list = picks[q.field, default: []]
        if let at = list.firstIndex(of: value) {
            list.remove(at: at)
        } else if q.kind == "multi" {
            list.append(value)
        } else {
            list = [value]
        }
        picks[q.field] = list
    }

    private func binding(_ field: String) -> Binding<String> {
        Binding(get: { typed[field, default: ""] }, set: { typed[field] = $0 })
    }
}
