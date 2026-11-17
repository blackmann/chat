//! Settings, laid out like Gitflow's: a titled page of rows, each a name, a
//! description and a control. Only appearance for now; a section list joins
//! once there is a second page.
use super::ui::*;
use super::*;
use crate::{
    menu::{Entry as MenuEntry, Item},
    theme::{DEFAULT_MONO, SYSTEM_UI},
};
use gpui_component::{
    IconName, Sizable,
    button::{Button, ButtonVariants},
    kbd::Kbd,
};

/// Monospace families worth offering first, where installed.
const SUGGESTED_MONO: &[&str] = &[
    "SF Mono",
    "Menlo",
    "Monaco",
    "JetBrains Mono",
    "Fira Code",
    "Berkeley Mono",
    "IBM Plex Mono",
    "Cascadia Code",
    "Source Code Pro",
    "Geist Mono",
    "Commit Mono",
    "Iosevka",
    "Hack",
];
/// The page's width, and the control column inside it. Descriptions get what
/// is left as a fixed width: wrapped text in a flex column is measured before
/// the column has its width, and runs into the control.
const PAGE: f32 = 640.;
const CONTROL: f32 = 220.;
const GAP: f32 = 24.;

/// Interface families worth offering first, where installed.
const SUGGESTED_UI: &[&str] = &["SF Pro", "Inter", "Helvetica Neue", "IBM Plex Sans", "Avenir Next", "Geist"];

const SAMPLE: &str = "fn open(&mut self, kind: TabKind) -> usize {\n    // Focus the tab if it's already open.\n    self.tabs.iter().position(|t| t.kind == kind).unwrap_or(0)\n}";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    Ui,
    Mono,
}

fn setting(colors: Palette, title: &str, description: &str, control: impl IntoElement) -> Div {
    row()
        .w(px(PAGE))
        .gap(px(GAP))
        .py(px(14.))
        .border_b_1()
        .border_color(rgb(colors.row_border))
        .child(
            col()
                .w(px(PAGE - CONTROL - GAP))
                .flex_shrink_0()
                .gap(px(3.))
                .child(div().text_size(px(14.)).child(title.to_string()))
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(relative(1.45))
                        .text_color(rgb(colors.muted))
                        .child(description.to_string()),
                ),
        )
        .child(div().flex_shrink_0().child(control))
}

fn group(colors: Palette, title: &str) -> Div {
    div()
        .pt_6()
        .pb_1()
        .font_family(mono())
        .text_size(px(12.))
        .text_color(rgb(colors.muted))
        .border_b_1()
        .border_color(rgb(colors.row_border))
        .child(title.to_string())
}

/// A font picker's face: the family's name, set in that family.
fn picker(colors: Palette, id: &'static str, family: SharedString, label: SharedString) -> Stateful<Div> {
    row()
        .id(id)
        .h(px(28.))
        .w(px(CONTROL))
        .pl(px(10.))
        .pr(px(8.))
        .gap_2()
        .rounded(px(6.))
        .border_1()
        .border_color(rgb(colors.border))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(colors.hover)))
        .child(one_line(label, 18.).flex_1().min_w_0().font_family(family))
        .child(icon(IconName::ChevronsUpDown).size(px(13.)).text_color(rgb(colors.muted)))
}

impl Workspace {
    pub(super) fn open_prefs(&mut self, _: &OpenPreferences, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs_open = true;
        self.popup = None;
        self.prefs_focus.focus(window);
        cx.notify();
    }
    fn close_prefs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs_open = false;
        self.popup = None;
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }
    /// Pick a font for `role`; `None` goes back to the default. Saved at once.
    fn set_font(&mut self, role: FontRole, family: Option<String>, cx: &mut Context<Self>) {
        match role {
            FontRole::Ui => self.settings.ui_font = family,
            FontRole::Mono => self.settings.mono_font = family,
        }
        let _ = self.settings.save();
        crate::theme::set_fonts(self.settings.ui_font.as_deref(), self.settings.mono_font.as_deref(), cx);
        cx.notify();
    }

    /// The menu for one font: a default, suggestions that are installed, then everything.
    fn font_menu(&self, role: FontRole, weak: WeakEntity<Workspace>, cx: &App) -> Vec<MenuEntry> {
        let names = self.font_names.get_or_init(|| {
            let mut names: Vec<String> = cx.text_system().all_font_names().into_iter().filter(|n| !n.starts_with('.')).collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            names
        });
        let current = match role {
            FontRole::Ui => self.settings.ui_font.clone(),
            FontRole::Mono => self.settings.mono_font.clone(),
        };
        let item = |label: String, family: Option<String>, preview: Option<String>| {
            let weak = weak.clone();
            let checked = current == family;
            let item = Item::new(label).checked(checked).on_click(move |_, cx| {
                let family = family.clone();
                let _ = weak.update(cx, |s, cx| s.set_font(role, family, cx));
            });
            // Suggestions preview themselves; the full list doesn't, as loading
            // every installed family just to open a menu is slow.
            MenuEntry::Item(match preview {
                Some(f) => item.font(f),
                None => item,
            })
        };
        let (default, default_family, suggested) = match role {
            FontRole::Ui => ("System default", SYSTEM_UI, SUGGESTED_UI),
            FontRole::Mono => ("Menlo (default)", DEFAULT_MONO, SUGGESTED_MONO),
        };
        let mut out = vec![item(default.into(), None, Some(default_family.into()))];
        let installed: Vec<&str> = suggested.iter().copied().filter(|f| *f != default_family && names.iter().any(|n| n == f)).collect();
        if !installed.is_empty() {
            out.push(MenuEntry::Label("Suggested".into()));
            out.extend(installed.into_iter().map(|f| item(f.into(), Some(f.into()), Some(f.into()))));
        }
        out.push(MenuEntry::Separator);
        out.push(MenuEntry::Label("All installed fonts".into()));
        out.extend(names.iter().map(|n| item(n.clone(), Some(n.clone()), None)));
        out
    }

    fn font_picker(&self, role: FontRole, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let (anchor, id, family, label) = match role {
            FontRole::Ui => (
                "pref-ui-font",
                "pref-ui-font-button",
                crate::theme::ui_font(),
                self.settings.ui_font.clone().unwrap_or_else(|| "System default".into()),
            ),
            FontRole::Mono => (
                "pref-mono-font",
                "pref-mono-font-button",
                mono(),
                self.settings.mono_font.clone().unwrap_or_else(|| DEFAULT_MONO.into()),
            ),
        };
        self.popover_trigger(
            anchor,
            picker(colors, id, family, label.into()),
            Corner::TopRight,
            300.,
            Some("Search fonts…"),
            move |s, weak, cx| s.font_menu(role, weak, cx),
            cx,
        )
    }

    /// Prose in the interface font and highlighted code in the code font.
    fn font_preview(&self, colors: Palette) -> Div {
        let styles = syntax::cached("rust", SAMPLE);
        let code = syntax::lines(SAMPLE).into_iter().enumerate().map(|(n, line)| {
            let marks = styles.as_ref().and_then(|s| s.line(colors.dark, n)).cloned().unwrap_or_default();
            div().whitespace_nowrap().child(StyledText::new(line).with_highlights(marks))
        });
        col()
            .mt_5()
            .w(px(PAGE))
            .p_4()
            .gap_3()
            .rounded(px(8.))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.row_border))
            .child(label(colors, "Preview"))
            .child(
                div()
                    .w(px(PAGE - 34.))
                    .line_height(relative(1.55))
                    .child("The chat tab stays pinned; files and diffs open beside it. Opening one that is already open focuses its tab."),
            )
            .child(
                div()
                    .p_3()
                    .rounded(px(6.))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.row_border))
                    .font_family(mono())
                    .text_size(px(crate::theme::MONO_SIZE))
                    .line_height(relative(1.5))
                    .text_color(rgb(colors.code))
                    .children(code),
            )
    }

    pub(super) fn preferences_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let customised = self.settings.ui_font.is_some() || self.settings.mono_font.is_some();
        let page = col()
            .w(px(PAGE))
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("Appearance"))
            .child(group(colors, "Fonts"))
            .child(setting(
                colors,
                "Interface font",
                "Everything outside code: the transcript, menus, tabs and panels.",
                self.font_picker(FontRole::Ui, cx),
            ))
            .child(setting(
                colors,
                "Code font",
                "Code blocks in replies, open files, diffs and paths.",
                self.font_picker(FontRole::Mono, cx),
            ))
            .child(self.font_preview(colors))
            .child(
                row().pt_4().child(div().flex_1()).when(customised, |s| {
                    s.child(
                        Button::new("reset-fonts").label("Restore defaults").ghost().small().on_click(cx.listener(|s, _, _, cx| {
                            s.set_font(FontRole::Ui, None, cx);
                            s.set_font(FontRole::Mono, None, cx);
                        })),
                    )
                }),
            );
        div()
            .id("prefs-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(colors.scrim))
            .track_focus(&self.prefs_focus)
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, w, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    s.close_prefs(w, cx);
                }
            }))
            // A click outside the dialog closes it.
            .on_mouse_down(MouseButton::Left, cx.listener(|s, _, w, cx| s.close_prefs(w, cx)))
            .child(
                col()
                    .w(px(PAGE + 80.))
                    .max_w(relative(0.94))
                    .h(px(600.))
                    .max_h(relative(0.92))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .rounded(px(12.))
                    .shadow_xl()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        row()
                            .h(px(44.))
                            .flex_shrink_0()
                            .pl(px(40.))
                            .pr_3()
                            .gap_2()
                            .text_size(px(12.))
                            .text_color(rgb(colors.muted))
                            .child("Settings")
                            .child(div().flex_1())
                            .child(Kbd::new(Keystroke::parse("escape").expect("valid keystroke")))
                            .child(
                                Button::new("close-prefs")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .small()
                                    .tooltip("Close · esc")
                                    .on_click(cx.listener(|s, _, w, cx| s.close_prefs(w, cx))),
                            ),
                    )
                    .child(div().id("prefs-content").flex_1().min_h_0().overflow_y_scroll().px(px(40.)).pb(px(40.)).child(page)),
            )
            .into_any_element()
    }
}
