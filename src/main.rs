mod agent;
mod editors;
mod icons;
mod git;
mod git_watch;
mod lsp;
mod menu;
mod missions;
mod model;
mod notify;
mod pasteboard;
mod settings;
mod stats;
mod store;
mod theme;
mod workspace;
mod worktree_tool;
use gpui::*;
use gpui_component::{Root, TitleBar};
use workspace::*;

/// The app's keys, shared with the tests that drive it.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-b", ToggleThreads, None),
        KeyBinding::new("cmd-alt-b", ToggleSide, None),
        KeyBinding::new("cmd-n", NewThread, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PreviousTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("cmd-1", ShowContext, None),
        KeyBinding::new("cmd-2", ShowChanges, None),
        KeyBinding::new("cmd-3", ShowFiles, None),
        KeyBinding::new("cmd-4", ShowMissions, None),
        KeyBinding::new("ctrl-`", OpenTerminal, None),
        KeyBinding::new("enter", Submit, Some("Composer > Input")),
        KeyBinding::new(
            "shift-enter",
            gpui_component::input::Enter { secondary: false },
            Some("Composer > Input"),
        ),
        // A waiting question card: choose, move between questions, send or skip.
        KeyBinding::new("up", AskUp, Some("Ask")),
        KeyBinding::new("down", AskDown, Some("Ask")),
        KeyBinding::new("space", AskPick, Some("Ask")),
        KeyBinding::new("enter", AskPick, Some("Ask")),
        KeyBinding::new("tab", AskNext, Some("Ask")),
        KeyBinding::new("shift-tab", AskPrevious, Some("Ask")),
        KeyBinding::new("cmd-enter", AskSubmit, Some("Ask")),
        KeyBinding::new("escape", AskSkip, Some("Ask")),
        KeyBinding::new("cmd-,", OpenPreferences, None),
        KeyBinding::new("cmd-l", AddToChat, None),
        KeyBinding::new("f12", GoToDefinition, None),
        KeyBinding::new("shift-f12", FindReferences, None),
        KeyBinding::new("ctrl--", GoBack, None),
        KeyBinding::new("ctrl-shift--", GoForward, None),
        KeyBinding::new("cmd-q", Quit, None),
        // Find in the thread: over the composer too, where the input would take ⌘ F.
        KeyBinding::new("cmd-f", FindInThread, None),
        KeyBinding::new("cmd-f", FindInThread, Some("Composer > Input")),
        KeyBinding::new("cmd-f", FindInThread, Some("ThreadFind > Input")),
        KeyBinding::new("enter", FindNext, Some("ThreadFind > Input")),
        KeyBinding::new("shift-enter", FindPrevious, Some("ThreadFind > Input")),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrevious, None),
        KeyBinding::new("escape", CloseFind, Some("ThreadFind > Input")),
        // Find a file by name, on the Files page.
        KeyBinding::new("cmd-shift-o", FindInFiles, None),
        KeyBinding::new("enter", OpenFound, Some("FileFind > Input")),
        KeyBinding::new("up", FoundUp, Some("FileFind > Input")),
        KeyBinding::new("down", FoundDown, Some("FileFind > Input")),
        KeyBinding::new("escape", CloseFileFind, Some("FileFind > Input")),
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(worktree_tool::FLAG) {
        worktree_tool::serve(&args[1..]);
        return;
    }
    Application::new().with_assets(icons::Assets).run(move |cx| {
        gpui_component::init(cx);
        theme::init(cx);
        workspace::init_terminal_keys(cx);
        cx.bind_keys(key_bindings());
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_app_quit(|_| async { worktree_tool::cleanup() }).detach();
        cx.set_menus(vec![
            Menu {
                name: "Chat".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenPreferences),
                    MenuItem::separator(),
                    MenuItem::action("Quit Chat", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Thread", NewThread),
                    MenuItem::action("Add Project…", AddProject),
                    MenuItem::action("Open Terminal", OpenTerminal),
                    MenuItem::separator(),
                    MenuItem::action("Close Tab", CloseTab),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Context", ShowContext),
                    MenuItem::action("Changes", ShowChanges),
                    MenuItem::action("Files", ShowFiles),
                    MenuItem::action("Missions", ShowMissions),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Threads", ToggleThreads),
                    MenuItem::action("Toggle Side Panel", ToggleSide),
                    MenuItem::separator(),
                    MenuItem::action("Next Tab", NextTab),
                    MenuItem::action("Previous Tab", PreviousTab),
                ],
            },
        ]);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitleBar::title_bar_options()),
                window_min_size: Some(size(px(980.), px(600.))),
                ..Default::default()
            },
            move |window, cx| {
                window.set_window_title("Chat");
                theme::sync(window, cx);
                window.observe_window_appearance(theme::sync).detach();
                let view = cx.new(|cx| Workspace::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("Could not open Chat window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
