//! Finding things: text in the thread showing (⌘ F), marked where it's drawn
//! and stepped through with ↵ and ⇧ ↵; and files by name (⌘ ⇧ O), listed on
//! the Files page from the same index `@` mentions use.
use super::ui::*;
use super::*;
use gpui_component::{Icon, IconName, Sizable, input::Input, scroll::ScrollableElement};

/// Files listed for a query; past these, typing more narrows it.
const SHOWN_FILES: usize = 100;

impl Workspace {
    // ── In the thread ───────────────────────────────────────────────────────

    pub(super) fn open_thread_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The transcript is the chat tab's: bring it forward.
        if let Some(chat) = self.tabs.iter().position(|t| t.kind == TabKind::Chat) {
            self.active = chat;
        }
        self.thread_find_open = true;
        self.thread_find.update(cx, |s, cx| s.focus(window, cx));
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        self.sync_thread_find(true, cx);
    }

    pub(super) fn close_thread_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread_find_open = false;
        self.selection.borrow_mut().find = None;
        self.focus.focus(window);
        cx.notify();
    }

    /// Mark what the find bar holds, and when `pick`, make the first match
    /// from the top of the view current.
    pub(super) fn sync_thread_find(&mut self, pick: bool, cx: &mut Context<Self>) {
        let needle = self.thread_find.read(cx).value().to_lowercase();
        let colors = palette(cx);
        {
            let mut selection = self.selection.borrow_mut();
            if !self.thread_find_open || needle.trim().is_empty() {
                selection.find = None;
                cx.notify();
                return;
            }
            let current = selection.find.as_ref().filter(|f| f.needle == needle).and_then(|f| f.current);
            selection.find = Some(select::Find {
                needle,
                current,
                tint: Hsla::from(rgb(colors.amber)).opacity(0.28),
                current_tint: Hsla::from(rgb(colors.amber)).opacity(0.6),
            });
        }
        if pick {
            let top = self.transcript.bounds().top();
            let matches = self.selection.borrow().matches();
            let first = {
                let selection = self.selection.borrow();
                matches
                    .iter()
                    .find(|(key, r)| selection.position((*key, r.start)).is_some_and(|(p, _)| p.y >= top))
                    .or(matches.first())
                    .map(|(key, r)| (*key, r.start))
            };
            self.set_current_match(first);
        }
        cx.notify();
    }

    /// Step to the next match (or the previous one), wrapping at the ends.
    pub(super) fn thread_find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.thread_find_open {
            return;
        }
        let matches = self.selection.borrow().matches();
        if matches.is_empty() {
            return;
        }
        let current = self.selection.borrow().find.as_ref().and_then(|f| f.current);
        let at = current.and_then(|c| matches.iter().position(|(key, r)| (*key, r.start) == c));
        let n = matches.len();
        let next = match (at, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        let (key, range) = &matches[next];
        self.set_current_match(Some((*key, range.start)));
        cx.notify();
    }

    /// Make `current` the lit match and scroll it into view.
    fn set_current_match(&mut self, current: Option<select::Point2>) {
        let mut selection = self.selection.borrow_mut();
        let Some(find) = selection.find.as_mut() else { return };
        find.current = current;
        let Some((at, line)) = current.and_then(|c| selection.position(c)) else { return };
        let view = self.transcript.bounds();
        let margin = px(48.);
        if at.y >= view.top() + margin && at.y + line <= view.bottom() - margin {
            return;
        }
        scroll_to_third(&self.transcript, at.y);
        self.follow = false;
    }

    // ── From a search result ────────────────────────────────────────────────

    /// Open thread `index` at the paragraph a search result came from: the
    /// `nth` paragraph holding the query in block `block`. It's found once the
    /// transcript has drawn, then scrolled to and lit for a moment.
    pub(super) fn reveal_hit(&mut self, index: usize, block: usize, nth: usize, cx: &mut Context<Self>) {
        self.select_thread(index, cx);
        self.follow = false;
        let needle = self.thread_search.read(cx).value().trim().to_lowercase();
        let mut selection = self.selection.borrow_mut();
        // What's recorded is the last thread's; wait for this one to draw.
        selection.forget_layout();
        selection.flash = None;
        selection.reveal = Some(select::Reveal { block, nth, needle, frames: 4 });
        cx.notify();
    }

    /// Bring a pending search result into view and light it, once drawn.
    /// Run before the segments are forgotten for the next frame.
    pub(super) fn resolve_reveal(&self, window: &Window, colors: Palette) {
        let mut selection = self.selection.borrow_mut();
        let Some(reveal) = selection.reveal.take() else { return };
        let Some(key) = selection.revealed(&reveal) else {
            if reveal.frames > 0 {
                selection.reveal = Some(select::Reveal { frames: reveal.frames - 1, ..reveal });
                window.request_animation_frame();
            }
            return;
        };
        if let Some(bounds) = selection.bounds(key) {
            scroll_to_third(&self.transcript, bounds.top());
        }
        selection.flash = Some(select::Flash {
            key,
            needle: reveal.needle,
            tint: Hsla::from(rgb(colors.amber)).opacity(0.6),
            began: std::time::Instant::now(),
        });
        window.request_animation_frame();
    }

    /// Where the current match is among them all, as the bar shows it.
    pub(super) fn thread_find_count(&self) -> Option<(Option<usize>, usize)> {
        let selection = self.selection.borrow();
        let find = selection.find.as_ref()?;
        let matches = selection.matches();
        let at = find.current.and_then(|c| matches.iter().position(|(key, r)| (*key, r.start) == c));
        Some((at, matches.len()))
    }

    /// The bar floating over the transcript's top corner.
    pub(super) fn thread_find_bar(&self, colors: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let count = match self.thread_find_count() {
            None => String::new(),
            Some((_, 0)) => "No matches".into(),
            Some((Some(i), n)) => format!("{} of {n}", i + 1),
            Some((None, n)) => format!("{n} found"),
        };
        row()
            .id("thread-find")
            .key_context("ThreadFind")
            .absolute()
            .top_2()
            .right(px(18.))
            .w(px(340.))
            .h(px(34.))
            .pl(px(6.))
            .pr_1()
            .gap(px(2.))
            .bg(rgb(colors.popover))
            .border_1()
            .border_color(rgb(colors.popover_border))
            .rounded(px(8.))
            .shadow_md()
            .occlude()
            .child(
                Input::new(&self.thread_find)
                    .flex_1()
                    .small()
                    .appearance(false)
                    .prefix(icon(IconName::Search).text_color(rgb(colors.muted))),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_1()
                    .text_size(px(crate::theme::scaled_ui(11.5)))
                    .text_color(rgb(colors.quiet))
                    .child(count),
            )
            .child(
                icon_button("find-previous", IconName::ChevronUp, "Previous match · ⇧ ↵")
                    .on_click(cx.listener(|s, _, _, cx| s.thread_find_step(false, cx))),
            )
            .child(
                icon_button("find-next", IconName::ChevronDown, "Next match · ↵")
                    .on_click(cx.listener(|s, _, _, cx| s.thread_find_step(true, cx))),
            )
            .child(
                icon_button("find-close", IconName::Close, "Close · esc")
                    .on_click(cx.listener(|s, _, window, cx| s.close_thread_find(window, cx))),
            )
    }

}

/// Scroll the transcript so what's drawn at `y` sits a third of the way down,
/// where the eye looks first.
fn scroll_to_third(transcript: &ScrollHandle, y: Pixels) {
    let view = transcript.bounds();
    let offset = transcript.offset();
    let max = transcript.max_offset().height;
    let to = (offset.y - (y - (view.top() + view.size.height / 3.))).clamp(-max, px(0.));
    transcript.set_offset(point(offset.x, to));
}

impl Workspace {
    // ── Files by name ───────────────────────────────────────────────────────

    pub(super) fn open_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_open = true;
        self.side = Side::Files;
        self.file_find_open = true;
        self.file_find_selected = 0;
        self.file_find.update(cx, |s, cx| s.focus(window, cx));
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        self.ensure_mention_index(cx);
        cx.notify();
    }

    pub(super) fn close_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_find_open = false;
        self.focus.focus(window);
        cx.notify();
    }

    pub(super) fn file_query_changed(&mut self, cx: &mut Context<Self>) {
        self.file_find_selected = 0;
        self.file_find_scroll.scroll_to_item(0);
        self.ensure_mention_index(cx);
        cx.notify();
    }

    /// The files matching what the box holds, best first.
    pub(super) fn found_files(&self, cx: &App) -> Vec<&super::mentions::Candidate> {
        self.file_matches(&self.file_find.read(cx).value(), SHOWN_FILES)
    }

    /// Move the pick up or down the list, stopping at its ends.
    pub(super) fn file_find_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let n = self.found_files(cx).len();
        if n == 0 {
            return;
        }
        self.file_find_selected = (self.file_find_selected as isize + step).clamp(0, n as isize - 1) as usize;
        self.file_find_scroll.scroll_to_item(self.file_find_selected);
        cx.notify();
    }

    /// Open the picked file.
    pub(super) fn open_found(&mut self, cx: &mut Context<Self>) {
        let found = self.found_files(cx);
        let Some(path) = found.get(self.file_find_selected.min(found.len().saturating_sub(1))).map(|c| c.path.clone()) else {
            return;
        };
        self.open(TabKind::File(path), cx);
    }

    /// The search box pinned atop the Files page.
    pub(super) fn file_find_box(&self, colors: Palette, cx: &mut Context<Self>) -> Div {
        row()
            .key_context("FileFind")
            .flex_shrink_0()
            .h(px(36.))
            .pl(px(8.))
            .pr_1()
            .gap(px(2.))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                Input::new(&self.file_find)
                    .flex_1()
                    .small()
                    .appearance(false)
                    .prefix(icon(IconName::Search).text_color(rgb(colors.muted))),
            )
            .when(self.mention_indexing, |s| s.child(spinner(colors)))
            .child(
                icon_button("file-find-close", IconName::Close, "Close · esc")
                    .on_click(cx.listener(|s, _, window, cx| s.close_file_find(window, cx))),
            )
    }

    /// The files matching the box, the picked one lit; a click opens one.
    pub(super) fn file_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let found = self.found_files(cx);
        if found.is_empty() {
            let why = if self.mention_indexing { "Looking through the files…" } else { "No file matches." };
            return label(colors, why).p_3().into_any_element();
        }
        let selected = self.file_find_selected.min(found.len() - 1);
        let mut list = col().py_1();
        for (i, file) in found.into_iter().enumerate() {
            let (glyph, tint) = super::file_kind::file_kind(&file.path);
            let detail = (!file.detail.is_empty()).then(|| (SharedString::from(file.detail.clone()), Hsla::from(rgb(colors.quiet))));
            list = list.child(
                row()
                    .id(("found-file", i))
                    .h(px(26.))
                    .mx_1()
                    .px_2()
                    .gap(px(6.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(i == selected, |s| s.bg(rgb(colors.selected)))
                    .when(i != selected, |s| s.hover(|s| s.bg(rgb(colors.hover))))
                    .child(icon(Icon::empty().path(glyph)).text_color(rgb(tint.color(colors))))
                    .child(line_with_detail(file.label.clone(), detail, 18.).flex_1().min_w_0())
                    .on_click(cx.listener({
                        let path = file.path.clone();
                        move |s, _, _, cx| {
                            s.file_find_selected = i;
                            s.open(TabKind::File(path.clone()), cx)
                        }
                    })),
            );
        }
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(div().id("found-files").size_full().overflow_y_scroll().track_scroll(&self.file_find_scroll).child(list))
            .vertical_scrollbar(&self.file_find_scroll)
            .into_any_element()
    }
}
