import SwiftUI

@main
struct ChatApp: App {
    @State private var store = Store()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(store)
                .tint(Theme.blue)
                .onOpenURL { url in
                    // A pairing link, from the Camera app: the first Mac or another one.
                    if case .pairing = store.phase { return }
                    _ = store.pair(url.absoluteString)
                }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { store.resume() }
        }
    }
}

struct RootView: View {
    @Environment(Store.self) private var store
    @State private var path = NavigationPath()

    var body: some View {
        switch store.phase {
        case .unpaired:
            PairView()
        case .pairing(let mac, let code, let expected):
            PairingCodeView(mac: mac, code: code, expected: expected)
        case .rejected(let reason) where store.mac == nil:
            PairView(problem: reason)
        default:
            NavigationStack(path: $path) {
                ProjectsView()
            }
            // Another Mac: start at its projects.
            .onChange(of: store.mac?.id) { path = NavigationPath() }
            #if DEBUG
            // For checking screens without tapping: open a project, and a thread in it.
            .onChange(of: store.projects) { _, projects in
                let env = ProcessInfo.processInfo.environment
                guard path.isEmpty, let key = env["CHAT_OPEN_PROJECT"],
                      let project = projects.first(where: { $0.key == key || $0.name == key }) else { return }
                path.append(project)
                if let thread = env["CHAT_OPEN_THREAD"].flatMap(Int.init) { path.append(thread) }
            }
            #endif
        }
    }
}
