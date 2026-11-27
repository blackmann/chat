//! Adding a project: a popover beside the rail's plus button, with a name
//! (the first folder's, until one is typed) and the folders it spans.
use super::ui::*;
use super::*;
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
};

/// The rail's plus button, which the popover opens beside.
pub(super) const ANCHOR: &str = "add-project";

impl Workspace {
    /// Open the popover, or close it if it is already open.
    pub(super) fn add_project(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_project.is_some() {
            return self.close_new_project(window, cx);
        }
        self.new_project = Some(Vec::new());
        self.new_project_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Pick more folders for the project being added.
    fn pick_new_project_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = this.update_in(cx, |s, window, cx| s.add_new_project_folders(paths, window, cx));
        })
        .detach();
    }

    /// Take in folders picked for the project being added.
    pub(super) fn add_new_project_folders(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folders) = &mut self.new_project else { return };
        let first = folders.is_empty();
        for path in paths {
            if !folders.contains(&path) {
                folders.push(path);
            }
        }
        // Named after its first folder, unless a name is already typed.
        if first && let Some(path) = folders.first().cloned() {
            self.new_project_input.update(cx, |input, cx| {
                if input.value().trim().is_empty() {
                    input.set_value(crate::model::folder_name(&path), window, cx);
                }
                input.focus(window, cx);
            });
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        }
        cx.notify();
    }

    fn remove_new_project_folder(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folders) = &mut self.new_project else { return };
        if index >= folders.len() {
            return;
        }
        let was = crate::model::folder_name(&folders.remove(index));
        // A name that only came from the first folder follows the new first one.
        if index == 0 {
            let next = folders.first().map(|p| crate::model::folder_name(p)).unwrap_or_default();
            self.new_project_input.update(cx, |input, cx| {
                if input.value().trim() == was {
                    input.set_value(next, window, cx);
                }
            });
        }
        cx.notify();
    }

    /// The project already on the rail with these folders: the same main one,
    /// and the same others in any order.
    fn same_project(&self, folders: &[PathBuf]) -> Option<usize> {
        let sorted = |others: &[PathBuf]| {
            let mut others = others.to_vec();
            others.sort();
            others
        };
        let (main, others) = folders.split_first()?;
        let others = sorted(others);
        self.projects.iter().position(|p| p.path.as_ref() == Some(main) && sorted(&p.folders) == others)
    }

    /// Put the project on the rail, based in its first folder, leaving the
    /// projects already there as they are. One with exactly these folders opens instead.
    pub(super) fn create_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut folders) = self.new_project.take_if(|f| !f.is_empty()) else { return };
        if let Some(existing) = self.same_project(&folders) {
            self.select_project(existing, cx);
        } else {
            let name = self.new_project_input.read(cx).value().trim().to_string();
            let path = folders.remove(0);
            let name = (!name.is_empty() && name != crate::model::folder_name(&path)).then_some(name);
            // Threads are filed under the main folder: one another project is
            // based in too gets its own place, so the two don't mix.
            let shared = self.projects.iter().any(|p| p.path.as_ref() == Some(&path));
            let key = shared.then(|| format!("{}#{}", path.display(), crate::model::now_ms()));
            let mut project = Project::open(crate::settings::ProjectEntry { path, name, folders, key });
            // A folder added back keeps the threads it had.
            if let Some(store) = &self.store {
                project.threads = store.load(&project, &self.settings.agents, self.settings.agent, &mut self.next_thread);
            }
            self.projects.push(project);
            self.select_project(self.projects.len() - 1, cx);
            self.save_projects();
            self.refresh_git(cx);
        }
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    pub(super) fn close_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_project.take().is_some() {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
            cx.notify();
        }
    }

    pub(super) fn new_project_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let folders = self.new_project.as_ref()?;
        let colors = palette(cx);
        let small = px(crate::theme::scaled_ui(11.));
        let mut list = col().gap(px(2.));
        for (i, folder) in folders.iter().enumerate() {
            list = list.child(
                row()
                    .id(("new-project-folder", i))
                    .group("new-project-folder")
                    .h(px(30.))
                    .px_1()
                    .gap_2()
                    .rounded(px(6.))
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .child(icon(IconName::Folder).text_color(rgb(colors.muted)))
                    .child(
                        col()
                            .flex_1()
                            .min_w_0()
                            .child(one_line(crate::model::folder_name(folder), 16.))
                            .child(one_line(crate::model::tilde(folder), 14.).text_size(small).text_color(rgb(colors.quiet))),
                    )
                    .when(i == 0 && folders.len() > 1, |r| r.child(pill("main", colors.muted, colors.hover)))
                    .child(
                        icon_button(("new-project-remove", i), IconName::Close, "Remove")
                            .xsmall()
                            .on_click(cx.listener(move |s, _, window, cx| s.remove_new_project_folder(i, window, cx))),
                    ),
            );
        }
        let ready = !folders.is_empty();
        let view = col()
            .id("new-project")
            .occlude()
            .w(px(264.))
            .p_2()
            .gap_2()
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(10.))
            .shadow_lg()
            .debug_selector(|| "new-project".into())
            .on_mouse_down_out(cx.listener(|s, e: &MouseDownEvent, window, cx| {
                // A press on the plus button is its own: it toggles the popover.
                if s.anchors.borrow().get(ANCHOR).is_some_and(|b| b.contains(&e.position)) {
                    return;
                }
                s.close_new_project(window, cx);
            }))
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    s.close_new_project(window, cx);
                }
            }))
            .child(label(colors, "New project").px_1().font_weight(FontWeight::MEDIUM))
            .child(gpui_component::input::Input::new(&self.new_project_input).small())
            .child(
                col()
                    .gap_1()
                    .child(label(colors, "Folders").px_1().text_size(small).text_color(rgb(colors.muted)))
                    .when(!ready, |c| {
                        c.child(label(colors, "Add a folder to start the project in").px_1().text_size(small).text_color(rgb(colors.quiet)))
                    })
                    .child(list),
            )
            .child(
                row()
                    .gap_1()
                    .child(
                        Button::new("new-project-add-folder")
                            .icon(icon(IconName::FolderOpen))
                            .label("Add folder…")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|s, _, window, cx| s.pick_new_project_folders(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("new-project-create")
                            .label("Add project")
                            .primary()
                            .small()
                            .disabled(!ready)
                            .on_click(cx.listener(|s, _, window, cx| s.create_new_project(window, cx))),
                    ),
            );
        let at = self.anchors.borrow().get(ANCHOR).map(|b| b.top_right() + point(px(8.), px(0.)));
        Some(
            deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(at.unwrap_or_default())
                    .snap_to_window_with_margin(px(8.))
                    .child(view),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}
