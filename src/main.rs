mod agent;
mod icons;
mod menu;
mod model;
mod settings;
mod store;
mod theme;
mod workspace;
use gpui::*;
use gpui_component::{Root, TitleBar};
use workspace::*;

fn main() {
    Application::new().with_assets(icons::Assets).run(move |cx| {
        gpui_component::init(cx);
        theme::init(cx);
        cx.bind_keys([
            // Keep terminal completion keys out of Root's focus navigation.
            KeyBinding::new("tab", NoAction, Some("Terminal")),
            KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
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
            KeyBinding::new("ctrl-`", OpenTerminal, None),
            KeyBinding::new("cmd-enter", Submit, None),
            KeyBinding::new("cmd-,", OpenPreferences, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
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
