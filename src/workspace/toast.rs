//! Short notices that float above the status bar and go away on their own,
//! with an optional action, and the Git output tab their failures open.

use super::ui::*;
use super::*;
use crate::icons::Glyph;
use gpui_component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};

/// How long a toast stays when the pointer isn't on it.
const SHOW_FOR: Duration = Duration::from_secs(8);

pub(super) struct Toast {
    id: usize,
    title: SharedString,
    detail: Option<SharedString>,
    action: Option<ToastAction>,
    hovered: bool,
    /// Its time ran out while the pointer was on it: it goes when the pointer leaves.
    expired: bool,
    _dismiss: Task<()>,
}

#[derive(Clone)]
pub(super) enum ToastAction {
    /// Open the Git output tab for this folder.
    GitOutput(PathBuf),
}

impl ToastAction {
    fn label(&self) -> &'static str {
        match self {
            ToastAction::GitOutput(_) => "View output",
        }
    }
}

/// What a Git command printed when it failed, kept for the output tab.
pub(super) struct GitFailure {
    pub what: &'static str,
    pub output: String,
    pub at: chrono::DateTime<chrono::Local>,
}

impl GitFailure {
    pub fn new(what: &'static str, output: String) -> Self {
        Self { what, output, at: chrono::Local::now() }
    }

    /// The line that says what went wrong, for the toast: Git's own `error:`
    /// or `fatal:` when it gave one, else the first thing it said.
    pub fn summary(&self) -> Option<SharedString> {
        let lines = || self.output.lines().map(str::trim).filter(|l| !l.is_empty());
        let line = lines()
            .find_map(|l| l.strip_prefix("error:").or_else(|| l.strip_prefix("fatal:")))
            .map(str::trim)
            .or_else(|| lines().next())?;
        Some(line.to_string().into())
    }
}

impl Workspace {
    pub(super) fn show_toast(
        &mut self,
        title: impl Into<SharedString>,
        detail: Option<SharedString>,
        action: Option<ToastAction>,
        cx: &mut Context<Self>,
    ) {
        self.toast_count += 1;
        let id = self.toast_count;
        let dismiss = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHOW_FOR).await;
            let _ = this.update(cx, |s, cx| s.expire_toast(id, cx));
        });
        self.toast = Some(Toast { id, title: title.into(), detail, action, hovered: false, expired: false, _dismiss: dismiss });
        cx.notify();
    }

    /// The timer ran out: go, unless the pointer is resting on it, in which
    /// case it goes once the pointer leaves.
    fn expire_toast(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_mut().filter(|t| t.id == id) else { return };
        if toast.hovered {
            toast.expired = true;
            return;
        }
        self.toast = None;
        cx.notify();
    }

    pub(super) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        if self.toast.take().is_some() {
            cx.notify();
        }
    }

    fn run_toast_action(&mut self, action: ToastAction, cx: &mut Context<Self>) {
        self.toast = None;
        match action {
            ToastAction::GitOutput(folder) => self.open(TabKind::GitOutput(folder), cx),
        }
        cx.notify();
    }

    pub(super) fn toast_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let toast = self.toast.as_ref()?;
        let colors = palette(cx);
        let action = toast.action.clone();
        let body = card(colors)
            .id("toast")
            .debug_selector(|| "toast".into())
            .w(px(340.))
            .shadow_lg()
            .occlude()
            .on_hover(cx.listener(|s, hovered: &bool, _, cx| {
                let Some(toast) = s.toast.as_mut() else { return };
                toast.hovered = *hovered;
                if !*hovered && toast.expired {
                    s.dismiss_toast(cx);
                }
            }))
            .child(
                row()
                    .items_start()
                    .gap_2()
                    .pl_3()
                    .pr_1()
                    .py_2()
                    .child(icon(IconName::CircleX).size(px(14.)).mt(px(2.)).flex_shrink_0().text_color(rgb(colors.red)))
                    .child(
                        col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().text_color(rgb(colors.text)).child(toast.title.clone()))
                            .when_some(toast.detail.clone(), |s, detail| {
                                s.child(
                                    div()
                                        .text_size(px(crate::theme::scaled_ui(12.)))
                                        .text_color(rgb(colors.muted))
                                        .line_clamp(2)
                                        .child(detail),
                                )
                            })
                            .when_some(action, |s, action| {
                                s.child(
                                    row().pt_1().child(
                                        Button::new("toast-action")
                                            .outline()
                                            .xsmall()
                                            .label(action.label())
                                            .on_click(cx.listener(move |s, _, _, cx| s.run_toast_action(action.clone(), cx))),
                                    ),
                                )
                            }),
                    )
                    .child(
                        Button::new("toast-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|s, _, _, cx| s.dismiss_toast(cx))),
                    ),
            );
        Some(deferred(div().absolute().right(px(12.)).bottom(px(40.)).child(body)).with_priority(1))
    }

    /// The Git output tab: what the last failed Git command in a folder printed.
    pub(super) fn git_output_view(&self, folder: &Path, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let failure = self.git_errors.get(folder);
        let output = failure.map(|f| f.output.clone());
        let header = super::panes::pane_header(
            colors,
            Glyph::Branch,
            &format!("Git output — {}", crate::model::folder_name(folder)),
            row()
                .gap_2()
                .font_family(crate::theme::ui_font())
                .when_some(failure, |s, f| s.child(label(colors, format!("{} · {}", f.what, f.at.format("%H:%M:%S")))))
                .child(
                    Button::new("git-output-copy")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .tooltip("Copy output")
                        .disabled(output.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(text) = output.clone() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }),
                ),
        );
        let body = match failure {
            None => div().p_4().text_color(rgb(colors.muted)).child("Nothing has gone wrong here.").into_any_element(),
            Some(f) => div()
                .id("git-output")
                .size_full()
                .overflow_y_scroll()
                .px_3()
                .py_2()
                .font_family(mono())
                .text_size(px(crate::theme::mono_size()))
                .text_color(rgb(colors.code))
                .children(f.output.lines().map(|line| div().min_h(px(20.)).child(line.to_string())))
                .into_any_element(),
        };
        col().size_full().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }
}
