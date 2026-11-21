//! Finding text: in the thread showing (⌘ F), marked where it's drawn and
//! stepped through with ↵ and ⇧ ↵; and in the project's files (⌘ ⇧ O), listed
//! on the Files page.
use super::ui::*;
use super::*;
use gpui_component::{Icon, IconName, Sizable, input::Input};
use std::sync::atomic::Ordering;

/// How long typing pauses before the files are searched.
const SETTLE: Duration = Duration::from_millis(150);

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
        // A third of the way down, where the eye looks first.
        let offset = self.transcript.offset();
        let max = self.transcript.max_offset().height;
        let y = (offset.y - (at.y - (view.top() + view.size.height / 3.))).clamp(-max, px(0.));
        self.transcript.set_offset(point(offset.x, y));
        self.follow = false;
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

    // ── In the files ────────────────────────────────────────────────────────

    pub(super) fn open_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_open = true;
        self.side = Side::Files;
        self.file_find_open = true;
        self.file_find.update(cx, |s, cx| s.focus(window, cx));
        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        // Files change between searches; look again.
        self.search_files(cx);
    }

    pub(super) fn close_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_find_open = false;
        self.file_searching = None;
        self.file_search_generation.fetch_add(1, Ordering::Relaxed);
        self.focus.focus(window);
        cx.notify();
    }

    /// Search the project's files for what the box holds, once typing settles.
    pub(super) fn search_files(&mut self, cx: &mut Context<Self>) {
        let query = self.file_find.read(cx).value().to_string();
        let generation = self.file_search_generation.fetch_add(1, Ordering::Relaxed) + 1;
        if query.trim().is_empty() {
            self.file_hits = None;
            self.file_searching = None;
            cx.notify();
            return;
        }
        let roots = self.current_project().roots();
        let project = self.project;
        let counter = self.file_search_generation.clone();
        self.file_searching = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let q = query.clone();
            let (files, truncated) =
                cx.background_executor().spawn(async move { grep::run(&roots, &q, SKIPPED, &counter, generation) }).await;
            let _ = this.update(cx, |s, cx| {
                if s.file_search_generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                s.file_hits = Some(grep::Results { project, query, files, truncated });
                s.file_searching = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Open the first file found, at its first matching line.
    pub(super) fn open_first_found(&mut self, cx: &mut Context<Self>) {
        let first = self
            .file_hits
            .as_ref()
            .filter(|r| r.project == self.project)
            .and_then(|r| r.files.first())
            .and_then(|f| f.lines.first().map(|l| (f.path.clone(), l.number)));
        if let Some((path, line)) = first {
            self.open_at(path, Some((line, line)), cx);
        }
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
            .when(self.file_searching.is_some(), |s| s.child(spinner(colors)))
            .child(
                icon_button("file-find-close", IconName::Close, "Close · esc")
                    .on_click(cx.listener(|s, _, window, cx| s.close_file_find(window, cx))),
            )
    }

    /// What the search found, by file, each line a link to where it is.
    pub(super) fn file_results(&self, cx: &mut Context<Self>) -> Div {
        let colors = palette(cx);
        let Some(results) = self.file_hits.as_ref().filter(|r| r.project == self.project) else {
            return col();
        };
        if results.files.is_empty() {
            return label(colors, format!("Nothing matches “{}”.", results.query)).p_3();
        }
        let lines = results.lines();
        let summary = format!(
            "{}{lines} {} in {} {}",
            if results.truncated { "First " } else { "" },
            if lines == 1 { "line" } else { "lines" },
            results.files.len(),
            if results.files.len() == 1 { "file" } else { "files" },
        );
        let mut list = col().py_1().child(label(colors, summary).px_3().py(px(6.)));
        let mono_size = px(crate::theme::scaled_mono(11.5));
        for (f, file) in results.files.iter().enumerate() {
            let relative = self.relative(&file.path);
            let (name, dir) = match relative.rsplit_once('/') {
                Some((dir, name)) => (name.to_string(), Some(SharedString::from(dir.to_string()))),
                None => (relative.clone(), None),
            };
            let (glyph, tint) = super::file_kind::file_kind(&file.path);
            list = list.child(
                row()
                    .id(("found-file", f))
                    .h(px(26.))
                    .mx_1()
                    .px_2()
                    .gap(px(6.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(colors.hover)))
                    .child(icon(Icon::empty().path(glyph)).text_color(rgb(tint.color(colors))))
                    .child(
                        line_with_detail(name, dir.map(|d| (d, Hsla::from(rgb(colors.quiet)))), 18.).flex_1().min_w_0(),
                    )
                    .child(count_badge(colors, file.lines.len()))
                    .on_click(cx.listener({
                        let path = file.path.clone();
                        move |s, _, _, cx| s.open(TabKind::File(path.clone()), cx)
                    })),
            );
            for (l, line) in file.lines.iter().enumerate() {
                let highlights = line
                    .marks
                    .iter()
                    .map(|m| {
                        (m.clone(), HighlightStyle { background_color: Some(Hsla::from(rgb(colors.amber)).opacity(0.3)), ..Default::default() })
                    })
                    .collect::<Vec<_>>();
                list = list.child(
                    row()
                        .id(SharedString::from(format!("found-{f}-{l}")))
                        .items_start()
                        .mx_1()
                        .pl(px(28.))
                        .pr_2()
                        .py(px(3.))
                        .gap_2()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(colors.hover)))
                        .font_family(mono())
                        .text_size(mono_size)
                        .child(div().flex_shrink_0().min_w(px(24.)).text_right().text_color(rgb(colors.quiet)).child(line.number.to_string()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(rgb(colors.muted))
                                .child(StyledText::new(line.text.clone()).with_highlights(highlights)),
                        )
                        .on_click(cx.listener({
                            let (path, number) = (file.path.clone(), line.number);
                            move |s, _, _, cx| s.open_at(path.clone(), Some((number, number)), cx)
                        })),
                );
            }
        }
        list
    }
}
