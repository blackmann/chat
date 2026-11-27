//! Settings: sections listed down the left under a search box, and the
//! section picked as a titled page of rows, each a name, a description and a
//! control. Searching lists the matching rows from every section.
use super::ui::*;
use super::*;
use crate::{
    menu::{Entry as MenuEntry, Item},
    theme::{DEFAULT_MONO, SYSTEM_UI},
};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    kbd::Kbd,
    radio::Radio,
    switch::Switch,
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

const SAMPLE: &str = "fn open(&mut self, kind: TabKind) -> usize {\n\t// Focus the tab if it's already open.\n\tself.tabs.iter().position(|t| t.kind == kind).unwrap_or(0)\n}";

/// Tab widths offered; anything 1–8 in the settings file is honoured.
const TAB_WIDTHS: &[usize] = &[2, 4, 8];

/// The choices for how thinking and tool calls show, as the select lists them.
const ACTIVITY_MODES: &[(ActivityMode, &str)] =
    &[(ActivityMode::Summary, "Summary"), (ActivityMode::Visible, "Visible"), (ActivityMode::Hidden, "Hidden")];

/// The sidebar beside the page.
const SIDEBAR: f32 = 210.;

/// The pages of settings, in the sidebar's order.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Appearance,
    Editor,
    Chat,
}
impl Section {
    const ALL: [Section; 3] = [Section::Appearance, Section::Editor, Section::Chat];
    fn title(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Editor => "Editor",
            Section::Chat => "Chat",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Section::Appearance => IconName::Palette,
            Section::Editor => IconName::File,
            Section::Chat => IconName::Bot,
        }
    }
}

/// One setting: where it lives, what it's called and does, and its control.
struct Row {
    section: Section,
    group: &'static str,
    title: &'static str,
    description: &'static str,
    control: AnyElement,
}
impl Row {
    fn matches(&self, query: &str) -> bool {
        [self.title, self.description, self.group, self.section.title()].iter().any(|t| t.to_lowercase().contains(query))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    Ui,
    Mono,
}

fn setting(colors: Palette, title: &str, description: &str, control: impl IntoElement) -> Div {
    let name = title.to_string();
    row()
        .debug_selector(move || format!("pref-row:{name}"))
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
                .child(div().text_size(px(crate::theme::scaled_ui(14.))).child(title.to_string()))
                .child(
                    div()
                        .text_size(px(crate::theme::scaled_ui(13.)))
                        .line_height(relative(1.45))
                        .text_color(rgb(colors.muted))
                        .child(description.to_string()),
                ),
        )
        // Controls sit against the right edge, whatever their width.
        .child(row().flex_1().min_w_0().justify_end().child(control))
}

fn group(colors: Palette, title: &str) -> Div {
    div()
        .pt_6()
        .pb_1()
        .font_family(mono())
        .text_size(px(crate::theme::mono_size()))
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
        self.prefs_search.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }
    /// Show `section`'s page, leaving any search.
    fn show_prefs_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs_section = section;
        self.prefs_search.update(cx, |s, cx| s.set_value("", window, cx));
        self.prefs_page_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }
    /// Open the settings file itself in a tab, for what has no control here.
    fn edit_settings_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = crate::settings::dir().map(|d| d.join("settings.json")) else { return };
        if !path.exists() {
            let _ = self.settings.save();
        }
        self.close_prefs(window, cx);
        self.open(TabKind::File(path), cx);
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
        self.apply_typography(cx);
        cx.notify();
    }

    fn apply_typography(&self, cx: &mut Context<Self>) {
        crate::theme::set_fonts(
            self.settings.ui_font.as_deref(),
            self.settings.mono_font.as_deref(),
            self.settings.ui_font_size,
            self.settings.mono_font_size,
            cx,
        );
    }

    fn font_size(&self, role: FontRole) -> f32 {
        match role {
            FontRole::Ui => self.settings.ui_font_size.unwrap_or(crate::theme::UI_SIZE),
            FontRole::Mono => self.settings.mono_font_size.unwrap_or(crate::theme::MONO_SIZE),
        }
    }

    fn set_font_size(&mut self, role: FontRole, size: Option<f32>, cx: &mut Context<Self>) {
        match role {
            FontRole::Ui => self.settings.ui_font_size = size,
            FontRole::Mono => self.settings.mono_font_size = size,
        }
        let _ = self.settings.save();
        self.apply_typography(cx);
        cx.notify();
    }

    fn font_size_picker(&self, role: FontRole, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let size = self.font_size(role);
        let (minus, plus, min, max) = match role {
            FontRole::Ui => ("pref-ui-size-minus", "pref-ui-size-plus", 11., 16.),
            FontRole::Mono => ("pref-mono-size-minus", "pref-mono-size-plus", 10., 16.),
        };
        row()
            .w(px(CONTROL))
            .justify_end()
            .gap_1()
            .child(
                Button::new(minus)
                    .icon(IconName::Minus)
                    .ghost()
                    .xsmall()
                    .disabled(size <= min)
                    .on_click(cx.listener(move |s, _, _, cx| s.set_font_size(role, Some((size - 1.).max(min)), cx))),
            )
            .child(
                div()
                    .w(px(62.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .font_family(mono())
                    .text_size(px(crate::theme::scaled_mono(12.)))
                    .child(format!("{size:.0} px")),
            )
            .child(
                Button::new(plus)
                    .icon(IconName::Plus)
                    .ghost()
                    .xsmall()
                    .disabled(size >= max)
                    .on_click(cx.listener(move |s, _, _, cx| s.set_font_size(role, Some((size + 1.).min(max)), cx))),
            )
    }

    fn reset_typography(&mut self, cx: &mut Context<Self>) {
        self.settings.ui_font = None;
        self.settings.mono_font = None;
        self.settings.ui_font_size = None;
        self.settings.mono_font_size = None;
        let _ = self.settings.save();
        self.apply_typography(cx);
        cx.notify();
    }

    fn apply_indentation(&mut self) {
        syntax::set_indentation(
            self.settings.tab_width.unwrap_or(syntax::DEFAULT_TAB_WIDTH),
            self.settings.indent_guides.unwrap_or(true),
        );
        // Open files hold their lines tab-expanded; read them again.
        for (path, view) in self.files.iter_mut() {
            *view = read_lines(path);
        }
    }

    fn set_tab_width(&mut self, width: usize, cx: &mut Context<Self>) {
        self.settings.tab_width = (width != syntax::DEFAULT_TAB_WIDTH).then_some(width);
        let _ = self.settings.save();
        self.apply_indentation();
        cx.notify();
    }

    fn set_indent_guides(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.indent_guides = (!on).then_some(false);
        let _ = self.settings.save();
        self.apply_indentation();
        cx.notify();
    }

    fn tab_width_picker(&self, cx: &mut Context<Self>) -> Div {
        let current = syntax::tab_width();
        TAB_WIDTHS.iter().fold(row().gap_4(), |row, &width| {
            row.child(
                Radio::new(("tab-width", width))
                    .label(width.to_string())
                    .checked(current == width)
                    .on_click(cx.listener(move |s, _, _, cx| s.set_tab_width(width, cx))),
            )
        })
    }

    fn indent_guides_switch(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.weak_entity();
        row().child(Switch::new("indent-guides").checked(syntax::show_guides()).on_click(move |on, _, cx| {
            let on = *on;
            let _ = this.update(cx, |s, cx| s.set_indent_guides(on, cx));
        }))
    }

    fn set_notifications(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.notifications = (!on).then_some(false);
        let _ = self.settings.save();
        cx.notify();
    }

    fn notifications_switch(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.weak_entity();
        let on = self.settings.notifications != Some(false);
        row().child(Switch::new("notifications").checked(on).on_click(move |on, _, cx| {
            let on = *on;
            let _ = this.update(cx, |s, cx| s.set_notifications(on, cx));
        }))
    }

    pub(super) fn set_missions(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.missions = (!on).then_some(false);
        let _ = self.settings.save();
        if !on {
            self.close_mission(cx);
        }
        cx.notify();
    }

    fn missions_switch(&self, cx: &mut Context<Self>) -> Div {
        let this = cx.weak_entity();
        row().child(Switch::new("missions").checked(self.missions_on()).on_click(move |on, _, cx| {
            let on = *on;
            let _ = this.update(cx, |s, cx| s.set_missions(on, cx));
        }))
    }

    fn set_follow_up_mode(&mut self, mode: FollowUpMode, cx: &mut Context<Self>) {
        self.settings.follow_up_mode = mode;
        let _ = self.settings.save();
        cx.notify();
    }

    fn follow_up_picker(&self, cx: &mut Context<Self>) -> Div {
        let mode = self.settings.follow_up_mode;
        row()
            .gap_4()
            .child(
                Radio::new("follow-up-queue")
                    .label("Queue")
                    .checked(mode == FollowUpMode::Queue)
                    .on_click(cx.listener(|s, _, _, cx| s.set_follow_up_mode(FollowUpMode::Queue, cx))),
            )
            .child(
                Radio::new("follow-up-steer")
                    .label("Steer")
                    .checked(mode == FollowUpMode::Steer)
                    .on_click(cx.listener(|s, _, _, cx| s.set_follow_up_mode(FollowUpMode::Steer, cx))),
            )
    }

    fn set_activity(&mut self, mode: ActivityMode, cx: &mut Context<Self>) {
        self.settings.activity = mode;
        let _ = self.settings.save();
        cx.notify();
    }

    fn set_project_palette(&mut self, palette: ProjectPalette, cx: &mut Context<Self>) {
        self.settings.project_palette = palette;
        let _ = self.settings.save();
        cx.notify();
    }

    /// A swatch per palette: a few of its colours as it'd look on the rail.
    fn project_palette_picker(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let current = self.settings.project_palette;
        row().gap(px(6.)).children(ProjectPalette::ALL.into_iter().map(|choice| {
            let hues = crate::theme::project_colors(choice, colors.dark);
            let picked = choice == current;
            col()
                .id(SharedString::from(format!("project-palette-{}", choice.name())))
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .child(
                    div()
                        .grid()
                        .grid_cols(4)
                        .gap(px(3.))
                        .p(px(5.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgb(if picked { colors.blue } else { colors.border }))
                        .when(picked, |s| s.bg(rgb(colors.blue_tint)))
                        .children(hues.iter().map(|h| div().size(px(7.)).rounded(px(2.)).bg(rgb(*h)))),
                )
                .child(
                    div()
                        .text_size(px(crate::theme::scaled_ui(11.)))
                        .text_color(rgb(if picked { colors.text } else { colors.muted }))
                        .child(choice.name()),
                )
                .on_click(cx.listener(move |s, _, _, cx| s.set_project_palette(choice, cx)))
        }))
    }

    fn activity_picker(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let label = ACTIVITY_MODES.iter().find(|(m, _)| *m == self.settings.activity).map_or("", |(_, n)| *n);
        self.popover_trigger(
            "pref-activity",
            picker(colors, "pref-activity-button", crate::theme::ui_font(), label.into()),
            Corner::TopRight,
            CONTROL,
            None,
            |s, weak, _| {
                ACTIVITY_MODES
                    .iter()
                    .map(|&(mode, name)| {
                        let weak = weak.clone();
                        MenuEntry::Item(Item::new(name).checked(s.settings.activity == mode).on_click(move |_, cx| {
                            let _ = weak.update(cx, |s, cx| s.set_activity(mode, cx));
                        }))
                    })
                    .collect()
            },
            cx,
        )
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
                    .text_size(px(crate::theme::mono_size()))
                    .line_height(relative(1.5))
                    .text_color(rgb(colors.code))
                    .children(code),
            )
    }

    /// Every setting, in the order the pages list them.
    fn pref_rows(&self, cx: &mut Context<Self>) -> Vec<Row> {
        use Section::*;
        let row = |section, group, title, description, control: AnyElement| Row { section, group, title, description, control };
        vec![
            row(Appearance, "Fonts", "Interface font", "Everything outside code: the transcript, menus, tabs and panels.", self.font_picker(FontRole::Ui, cx).into_any_element()),
            row(Appearance, "Fonts", "Code font", "Code blocks in replies, open files, diffs and paths.", self.font_picker(FontRole::Mono, cx).into_any_element()),
            row(Appearance, "Fonts", "Interface font size", "The base size for chat text, controls, menus, tabs and panels.", self.font_size_picker(FontRole::Ui, cx).into_any_element()),
            row(Appearance, "Fonts", "Code font size", "The base size for code, paths and chip-style inlays.", self.font_size_picker(FontRole::Mono, cx).into_any_element()),
            row(
                Appearance,
                "Projects",
                "Project colours",
                "The colours projects take on the rail. Each project keeps its colour, and no two share one until there are more than twelve.",
                self.project_palette_picker(cx).into_any_element(),
            ),
            row(Editor, "Code", "Tab width", "How many columns a tab character spans in open files, diffs and code blocks.", self.tab_width_picker(cx).into_any_element()),
            row(Editor, "Code", "Indent guides", "Faint vertical lines at each indent level in open files and diffs.", self.indent_guides_switch(cx).into_any_element()),
            row(
                Chat,
                "Messages",
                "While the agent is working",
                "Queue waits for the current turn to finish. Steer sends the message to the running turn when the agent supports it.",
                self.follow_up_picker(cx).into_any_element(),
            ),
            row(
                Chat,
                "Messages",
                "Thinking and tool calls",
                "Summary shows them while the agent works, then folds them into one line at the end of the turn. Edits stay listed either way.",
                self.activity_picker(cx).into_any_element(),
            ),
            row(
                Chat,
                "Notifications",
                "Notify when a thread needs you",
                "A system notification when a turn finishes or the agent asks a question while its thread isn't on screen or Chat isn't in front. Click one to open the thread.",
                self.notifications_switch(cx).into_any_element(),
            ),
            row(
                Chat,
                "Missions",
                "Missions",
                "Tiny memory games that arrive in the status bar's mail icon now and then while agents work, such as remembering a PIN until a few turns later.",
                self.missions_switch(cx).into_any_element(),
            ),
        ]
    }

    /// The sidebar: search, then a row per section.
    fn prefs_sidebar(&self, searching: bool, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let mut nav = col().px_2().gap(px(2.));
        for section in Section::ALL {
            let on = !searching && self.prefs_section == section;
            nav = nav.child(
                row()
                    .id(SharedString::from(format!("prefs-section-{}", section.title())))
                    .h(px(30.))
                    .px_2()
                    .gap_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(rgb(if on { colors.text } else { colors.muted }))
                    .when(on, |s| s.bg(rgb(colors.selected)))
                    .when(!on, |s| s.hover(|s| s.bg(rgb(colors.hover)).text_color(rgb(colors.text))))
                    .child(icon(section.icon()).size(px(14.)).text_color(rgb(if on { colors.blue } else { colors.muted })))
                    .child(section.title())
                    .on_click(cx.listener(move |s, _, window, cx| s.show_prefs_section(section, window, cx))),
            );
        }
        col()
            .w(px(SIDEBAR))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(colors.chrome))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div().p_3().child(
                    gpui_component::input::Input::new(&self.prefs_search)
                        .small()
                        .prefix(icon(IconName::Search).text_color(rgb(colors.muted)))
                        .cleanable(true),
                ),
            )
            .child(nav)
            .child(div().flex_1())
            .child(
                row()
                    .mx_3()
                    .py_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .text_size(px(crate::theme::scaled_ui(11.5)))
                    .text_color(rgb(colors.quiet))
                    .child(Kbd::new(Keystroke::parse("escape").expect("valid keystroke")))
                    .child("to close"),
            )
    }

    pub(super) fn preferences_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let query = self.prefs_search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();
        let rows = self.pref_rows(cx);
        let title = |text: String| div().pt_1().text_size(px(crate::theme::scaled_ui(20.))).font_weight(FontWeight::SEMIBOLD).child(text);
        let mut page = col().w(px(PAGE));
        if searching {
            let found: Vec<Row> = rows.into_iter().filter(|r| r.matches(&query)).collect();
            page = page.child(title("Search results".into()));
            if found.is_empty() {
                page = page.child(label(colors, format!("No settings match “{query}”.")).pt_4());
            }
            let mut last = None;
            for r in found {
                if last != Some(r.section) {
                    last = Some(r.section);
                    page = page.child(group(colors, &format!("{} · {}", r.section.title(), r.group)));
                }
                page = page.child(setting(colors, r.title, r.description, r.control));
            }
        } else {
            let section = self.prefs_section;
            page = page.child(title(section.title().into()));
            let mut last = None;
            for r in rows.into_iter().filter(|r| r.section == section) {
                if last != Some(r.group) {
                    last = Some(r.group);
                    page = page.child(group(colors, r.group));
                }
                page = page.child(setting(colors, r.title, r.description, r.control));
            }
            if section == Section::Appearance {
                let customised = self.settings.ui_font.is_some()
                    || self.settings.mono_font.is_some()
                    || self.settings.ui_font_size.is_some()
                    || self.settings.mono_font_size.is_some();
                page = page.child(self.font_preview(colors)).child(row().pt_4().child(div().flex_1()).when(customised, |s| {
                    s.child(
                        Button::new("reset-fonts")
                            .label("Restore defaults")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|s, _, _, cx| s.reset_typography(cx))),
                    )
                }));
            }
        }
        let main = col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                row()
                    .h(px(48.))
                    .flex_shrink_0()
                    .pl(px(40.))
                    .pr_3()
                    .gap_2()
                    .child(div().flex_1())
                    .child(
                        Button::new("edit-settings-file")
                            .label("Edit in settings.json")
                            .outline()
                            .small()
                            .on_click(cx.listener(|s, _, w, cx| s.edit_settings_file(w, cx))),
                    )
                    .child(
                        Button::new("close-prefs")
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip("Close · esc")
                            .on_click(cx.listener(|s, _, w, cx| s.close_prefs(w, cx))),
                    ),
            )
            .child(
                div()
                    .id("prefs-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.prefs_page_scroll)
                    .px(px(40.))
                    .pb(px(40.))
                    .child(page),
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
                row()
                    .w(px(SIDEBAR + PAGE + 80.))
                    .max_w(relative(0.94))
                    .h(px(640.))
                    .max_h(relative(0.92))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .rounded(px(12.))
                    .shadow_xl()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.prefs_sidebar(searching, cx))
                    .child(main),
            )
            .into_any_element()
    }
}
