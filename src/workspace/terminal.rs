//! PTY I/O and VT state live off the UI thread; GPUI paints only the visible grid.
use super::*;
use alacritty_terminal::{
    event::{Event as TerminalEvent, EventListener, Notify, WindowSize},
    event_loop::{EventLoop, Msg, Notifier},
    grid::{Dimensions, Scroll},
    index::{Column, Direction, Line, Point as CellPoint},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term, TermMode, cell::Flags},
    tty,
    vte::ansi::{Color, CursorShape, NamedColor},
};
use futures::channel::mpsc::{UnboundedSender, unbounded};
use std::sync::Arc;

const HEIGHT: f32 = 20.;
#[derive(Clone)]
struct Listener(UnboundedSender<TerminalEvent>);
impl EventListener for Listener {
    fn send_event(&self, event: TerminalEvent) {
        let _ = self.0.unbounded_send(event);
    }
}
#[derive(Clone, Copy, PartialEq)]
struct GridSize {
    rows: usize,
    cols: usize,
}
impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}
impl GridSize {
    fn window(self, width: f32) -> WindowSize {
        WindowSize {
            num_lines: self.rows as u16,
            num_cols: self.cols as u16,
            cell_width: width as u16,
            cell_height: HEIGHT as u16,
        }
    }
}

pub(super) struct TerminalView {
    focus: FocusHandle,
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    bounds: Bounds<Pixels>,
    cell_width: f32,
    size: GridSize,
    exited: bool,
    selecting: bool,
    marked: String,
    pub(super) request_focus: bool,
    _events: Task<()>,
}
impl Drop for TerminalView {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
impl TerminalView {
    pub fn new(cwd: PathBuf, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let (tx, mut rx) = unbounded();
        let listener = Listener(tx);
        let size = GridSize { rows: 24, cols: 80 };
        let term = Arc::new(FairMutex::new(Term::new(
            Config {
                scrolling_history: 10_000,
                ..Config::default()
            },
            &size,
            listener.clone(),
        )));
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let options = tty::Options {
            shell: Some(tty::Shell::new(shell, vec!["-l".into()])),
            working_directory: Some(cwd),
            drain_on_exit: true,
            env: [
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ]
            .into(),
            ..Default::default()
        };
        let pty = tty::new(&options, size.window(8.), 0)?;
        let io = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let notifier = Notifier(io.channel());
        io.spawn();
        Ok(cx.new(move |cx: &mut Context<Self>| {
            let events = cx.spawn(async move |this, cx| {
                while let Some(event) = rx.next().await {
                    if this
                        .update(cx, |s, cx| {
                            match event {
                                TerminalEvent::PtyWrite(text) => {
                                    s.notifier.notify(text.into_bytes())
                                }
                                TerminalEvent::TextAreaSizeRequest(format) => s
                                    .notifier
                                    .notify(format(s.size.window(s.cell_width)).into_bytes()),
                                TerminalEvent::ColorRequest(index, format) => {
                                    let value = ansi_color(
                                        match index {
                                            256 => Color::Named(NamedColor::Foreground),
                                            257 => Color::Named(NamedColor::Background),
                                            258 => Color::Named(NamedColor::Foreground),
                                            _ => Color::Indexed(index.min(255) as u8),
                                        },
                                        palette(cx),
                                    );
                                    s.notifier.notify(
                                        format(alacritty_terminal::vte::ansi::Rgb {
                                            r: (value >> 16) as u8,
                                            g: (value >> 8) as u8,
                                            b: value as u8,
                                        })
                                        .into_bytes(),
                                    );
                                }
                                TerminalEvent::Exit | TerminalEvent::ChildExit(_) => {
                                    s.exited = true
                                }
                                _ => {}
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Self {
                focus: cx.focus_handle(),
                term,
                notifier,
                bounds: Bounds::default(),
                cell_width: 8.,
                size,
                exited: false,
                selecting: false,
                marked: String::new(),
                request_focus: true,
                _events: events,
            }
        }))
    }
    fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if self.exited {
            return;
        }
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
        drop(term);
        self.notifier.notify(bytes);
        cx.notify();
    }
    fn cell(&self, position: Point<Pixels>) -> CellPoint {
        CellPoint::new(
            Line(
                ((f32::from(position.y - self.bounds.origin.y) / HEIGHT).floor() as i32)
                    .clamp(0, self.size.rows as i32 - 1)
                    - self.term.lock().grid().display_offset() as i32,
            ),
            Column(
                ((f32::from(position.x - self.bounds.origin.x) / self.cell_width)
                    .floor()
                    .max(0.) as usize)
                    .min(self.size.cols - 1),
            ),
        )
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.platform {
            match key.key.as_str() {
                "c" => {
                    if let Some(text) = self.term.lock().selection_to_string() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
                        self.input(paste(&text, bracketed).into_bytes(), cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        // Let the IME finish composition before interpreting terminal control keys.
        if !self.marked.is_empty() && !key.modifiers.control && !key.modifiers.alt {
            return;
        }
        let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = key_bytes(key, app_cursor) {
            self.input(bytes, cx);
            cx.stop_propagation();
        }
    }
}
fn paste(text: &str, bracketed: bool) -> String {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}
fn key_bytes(key: &Keystroke, app: bool) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let suffix = match key.key.as_str() {
        "up" => Some("A"),
        "down" => Some("B"),
        "right" => Some("C"),
        "left" => Some("D"),
        "home" => Some("H"),
        "end" => Some("F"),
        _ => None,
    };
    let mut text = if let Some(suffix) = suffix {
        let modifier = 1
            + u8::from(modifiers.shift)
            + 2 * u8::from(modifiers.alt)
            + 4 * u8::from(modifiers.control);
        if modifier > 1 {
            format!("\x1b[1;{modifier}{suffix}")
        } else {
            format!("\x1b{}{suffix}", if app { "O" } else { "[" })
        }
    } else {
        match key.key.as_str() {
            "enter" => "\r".into(),
            "backspace" => "\x7f".into(),
            "escape" => "\x1b".into(),
            "tab" if modifiers.shift => "\x1b[Z".into(),
            "tab" => "\t".into(),
            "delete" => "\x1b[3~".into(),
            "pageup" => "\x1b[5~".into(),
            "pagedown" => "\x1b[6~".into(),
            "f1" => "\x1bOP".into(),
            "f2" => "\x1bOQ".into(),
            "f3" => "\x1bOR".into(),
            "f4" => "\x1bOS".into(),
            "f5" => "\x1b[15~".into(),
            "f6" => "\x1b[17~".into(),
            "f7" => "\x1b[18~".into(),
            "f8" => "\x1b[19~".into(),
            "f9" => "\x1b[20~".into(),
            "f10" => "\x1b[21~".into(),
            "f11" => "\x1b[23~".into(),
            "f12" => "\x1b[24~".into(),
            _ if modifiers.control => {
                let c = if key.key == "space" {
                    b' '
                } else if key.key.len() == 1 {
                    key.key.as_bytes()[0]
                } else {
                    return None;
                };
                if c == b' ' {
                    "\0".into()
                } else if (b'@'..=b'_').contains(&c.to_ascii_uppercase()) {
                    ((c.to_ascii_uppercase() & 0x1f) as char).to_string()
                } else {
                    return None;
                }
            }
            _ if modifiers.alt => key.key_char.clone()?,
            // Ordinary text belongs to the platform input handler. macOS also supplies
            // key_char for Enter and Tab, so match those control keys above first.
            _ => return None,
        }
    };
    if modifiers.alt && suffix.is_none() {
        text.insert(0, '\x1b');
    }
    Some(text.into_bytes())
}
const ANSI_DARK: [u32; 16] = [
    0x1d2028, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x5c6370,
    0xff7a85, 0xb2df92, 0xffd68a, 0x80c7ff, 0xe29bfa, 0x76dbe8, 0xffffff,
];
const ANSI_LIGHT: [u32; 16] = [
    0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7, 0x696c77,
    0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xfafafa,
];
fn ansi_color(color: Color, colors: Palette) -> u32 {
    let ansi = if colors.dark { ANSI_DARK } else { ANSI_LIGHT };
    match color {
        Color::Spec(c) => ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32,
        Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => colors.code,
        Color::Named(NamedColor::Background) => colors.background,
        Color::Named(NamedColor::Cursor) => colors.blue,
        Color::Named(NamedColor::DimForeground) => colors.muted,
        Color::Named(c) => {
            let i = c as usize;
            ansi[if i >= NamedColor::DimBlack as usize {
                i - NamedColor::DimBlack as usize
            } else {
                i
            }
            .min(15)]
        }
        Color::Indexed(i @ 0..=15) => ansi[i as usize],
        Color::Indexed(i @ 16..=231) => {
            let n = i as u32 - 16;
            let v = |n| if n == 0 { 0 } else { 55 + n * 40 };
            (v(n / 36) << 16) | (v(n / 6 % 6) << 8) | v(n % 6)
        }
        Color::Indexed(i) => {
            let v = 8 + (i as u32 - 232) * 10;
            v * 0x010101
        }
    }
}
impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.request_focus {
            self.focus.focus(window);
            self.request_focus = false;
        }
        let colors = palette(cx);
        let entity = cx.entity();
        let painter = entity.clone();
        let focus = self.focus.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .text_color(rgb(colors.code))
            .when(self.exited, |view| {
                view.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(rgb(colors.muted))
                        .child("Shell exited · close this tab to start again"),
                )
            })
            .child(
                div()
                    .id("terminal-grid")
                    .key_context("Terminal")
                    .p_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::key))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, e: &MouseDownEvent, window, cx| {
                            s.focus.focus(window);
                            let point = s.cell(e.position);
                            let ty = if e.click_count >= 3 {
                                SelectionType::Lines
                            } else if e.click_count == 2 {
                                SelectionType::Semantic
                            } else {
                                SelectionType::Simple
                            };
                            s.term.lock().selection =
                                Some(Selection::new(ty, point, Direction::Left));
                            s.selecting = true;
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|s, e: &MouseMoveEvent, _, cx| {
                        if s.selecting && e.pressed_button == Some(MouseButton::Left) {
                            let point = s.cell(e.position);
                            if let Some(selection) = &mut s.term.lock().selection {
                                selection.update(point, Direction::Right);
                            }
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|s, _, _, _| s.selecting = false),
                    )
                    .on_scroll_wheel(cx.listener(|s, e: &ScrollWheelEvent, _, cx| {
                        let delta = match e.delta {
                            ScrollDelta::Lines(p) => p.y,
                            ScrollDelta::Pixels(p) => f32::from(p.y) / HEIGHT,
                        };
                        if s.term.lock().mode().contains(TermMode::ALT_SCREEN) {
                            let key = if delta > 0. { b"\x1bOA" } else { b"\x1bOB" };
                            s.input(key.repeat(delta.abs().ceil().min(20.) as usize), cx);
                        } else {
                            s.term
                                .lock()
                                .scroll_display(Scroll::Delta(delta.round() as i32));
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let font = font(mono());
                                let width = window
                                    .text_system()
                                    .advance(
                                        window.text_system().resolve_font(&font),
                                        px(crate::theme::MONO_SIZE),
                                        'm',
                                    )
                                    .map(|s| f32::from(s.width))
                                    .unwrap_or(8.);
                                entity.update(cx, |s, _| {
                                    s.bounds = bounds;
                                    s.cell_width = width;
                                    let size = GridSize {
                                        rows: (f32::from(bounds.size.height) / HEIGHT)
                                            .floor()
                                            .max(1.)
                                            as usize,
                                        cols: (f32::from(bounds.size.width) / width).floor().max(2.)
                                            as usize,
                                    };
                                    if size != s.size {
                                        s.size = size;
                                        s.term.lock().resize(size);
                                        let _ = s.notifier.0.send(Msg::Resize(size.window(width)));
                                    }
                                });
                            },
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, painter.clone()),
                                    cx,
                                );
                                painter.update(cx, |s, cx| {
                                    let term = s.term.lock();
                                    let content = term.renderable_content();
                                    for cell in content.display_iter {
                                        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                                            continue;
                                        }
                                        let row = cell.point.line.0 + content.display_offset as i32;
                                        if row < 0 || row >= s.size.rows as i32 {
                                            continue;
                                        }
                                        let origin = bounds.origin
                                            + point(
                                                px(cell.point.column.0 as f32 * s.cell_width),
                                                px(row as f32 * HEIGHT),
                                            );
                                        let selected = content
                                            .selection
                                            .is_some_and(|r| r.contains(cell.point));
                                        let cursor = content.cursor.point == cell.point
                                            && content.cursor.shape != CursorShape::Hidden
                                            && content.display_offset == 0
                                            && focus.is_focused(window)
                                            && !s.exited;
                                        let mut fg = ansi_color(cell.fg, colors);
                                        let mut bg = ansi_color(cell.bg, colors);
                                        if cell.flags.contains(Flags::INVERSE) {
                                            std::mem::swap(&mut fg, &mut bg);
                                        }
                                        if selected {
                                            bg = colors.selected;
                                        }
                                        if cursor {
                                            bg = colors.code;
                                            fg = colors.background;
                                        }
                                        let wide = cell.flags.contains(Flags::WIDE_CHAR);
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                origin,
                                                size(
                                                    px(s.cell_width * if wide { 2. } else { 1. }),
                                                    px(HEIGHT),
                                                ),
                                            ),
                                            rgb(bg),
                                        ));
                                        if cell
                                            .flags
                                            .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
                                        {
                                            continue;
                                        }
                                        let mut text = cell.c.to_string();
                                        if let Some(chars) = cell.zerowidth() {
                                            text.extend(chars);
                                        }
                                        let mut font = font(mono());
                                        if cell.flags.contains(Flags::BOLD) {
                                            font.weight = FontWeight::BOLD;
                                        }
                                        if cell.flags.contains(Flags::ITALIC) {
                                            font.style = FontStyle::Italic;
                                        }
                                        let run = TextRun {
                                            len: text.len(),
                                            font,
                                            color: rgb(fg).into(),
                                            background_color: None,
                                            underline: cell
                                                .flags
                                                .intersects(Flags::ALL_UNDERLINES)
                                                .then_some(UnderlineStyle {
                                                    thickness: px(1.),
                                                    color: Some(rgb(fg).into()),
                                                    wavy: false,
                                                }),
                                            strikethrough: None,
                                        };
                                        let line = window.text_system().shape_line(
                                            text.into(),
                                            px(crate::theme::MONO_SIZE),
                                            &[run],
                                            None,
                                        );
                                        let _ = line.paint(origin, px(HEIGHT), window, cx);
                                    }
                                    if !s.marked.is_empty() && focus.is_focused(window) {
                                        let cursor = content.cursor.point;
                                        let origin = bounds.origin
                                            + point(
                                                px(cursor.column.0 as f32 * s.cell_width),
                                                px(cursor.line.0 as f32 * HEIGHT),
                                            );
                                        let run = TextRun {
                                            len: s.marked.len(),
                                            font: font(mono()),
                                            color: rgb(colors.code).into(),
                                            background_color: Some(rgb(colors.selected).into()),
                                            underline: Some(UnderlineStyle {
                                                thickness: px(1.),
                                                color: None,
                                                wavy: false,
                                            }),
                                            strikethrough: None,
                                        };
                                        let line = window.text_system().shape_line(
                                            s.marked.clone().into(),
                                            px(crate::theme::MONO_SIZE),
                                            &[run],
                                            None,
                                        );
                                        let _ = line.paint(origin, px(HEIGHT), window, cx);
                                    }
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{event::VoidListener, vte::ansi::Processor};

    #[::core::prelude::v1::test]
    fn terminal_keys_follow_application_cursor_mode() {
        let key = |s| Keystroke::parse(s).unwrap();
        assert_eq!(key_bytes(&key("up"), false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(&key("up"), true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(&key("ctrl-c"), false).unwrap(), b"\x03");
        assert_eq!(key_bytes(&key("ctrl-space"), false).unwrap(), b"\0");
        assert_eq!(key_bytes(&key("ctrl-left"), true).unwrap(), b"\x1b[1;5D");
        assert_eq!(key_bytes(&key("shift-tab"), false).unwrap(), b"\x1b[Z");
    }

    #[::core::prelude::v1::test]
    fn macos_control_keys_with_text_still_reach_the_pty() {
        let mut enter = Keystroke::parse("enter").unwrap();
        enter.key_char = Some("\n".into());
        assert_eq!(key_bytes(&enter, false).unwrap(), b"\r");
        let mut tab = Keystroke::parse("tab").unwrap();
        tab.key_char = Some("\t".into());
        assert_eq!(key_bytes(&tab, false).unwrap(), b"\t");
        let mut text = Keystroke::parse("a").unwrap();
        text.key_char = Some("a".into());
        assert_eq!(key_bytes(&text, false), None);
    }

    #[::core::prelude::v1::test]
    fn paste_preserves_bracketed_content_and_normalizes_plain_newlines() {
        assert_eq!(paste("one\r\ntwo\n", false), "one\rtwo\r");
        assert_eq!(
            paste("one\ntwo\x1b[201~", true),
            "\x1b[200~one\ntwo[201~\x1b[201~"
        );
    }

    #[::core::prelude::v1::test]
    fn vt_grid_handles_color_unicode_alternate_screen_and_resize() {
        let mut term = Term::new(
            Config::default(),
            &GridSize { rows: 4, cols: 20 },
            VoidListener,
        );
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, "\x1b[31mred\x1b[0m 界e\u{301}".as_bytes());
        assert_eq!(
            term.grid()[Line(0)][Column(0)].fg,
            Color::Named(NamedColor::Red)
        );
        assert!(
            term.grid()[Line(0)][Column(4)]
                .flags
                .contains(Flags::WIDE_CHAR)
        );
        parser.advance(&mut term, b"\x1b[?1049hTUI");
        assert!(term.mode().contains(TermMode::ALT_SCREEN));
        parser.advance(&mut term, b"\x1b[?1049l");
        assert_eq!(term.grid()[Line(0)][Column(0)].c, 'r');
        term.resize(GridSize { rows: 6, cols: 40 });
        assert_eq!(term.columns(), 40);
        assert_eq!(term.screen_lines(), 6);
    }

    #[::core::prelude::v1::test]
    fn pty_runs_a_real_shell_in_the_requested_directory() {
        let (tx, _rx) = unbounded();
        let listener = Listener(tx);
        let size = GridSize {
            rows: 10,
            cols: 100,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
        )));
        let options = tty::Options {
            shell: Some(tty::Shell::new(
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    r#"read -r command; eval "$command"; test -t 0 && test -t 1 && printf 'PTY_OK '; pwd; stty size"#.into(),
                ],
            )),
            working_directory: Some(PathBuf::from("/private/tmp")),
            drain_on_exit: true,
            ..Default::default()
        };
        let pty = tty::new(&options, size.window(8.), 0).expect("allocate PTY");
        let io = EventLoop::new(term.clone(), listener, pty, true, false).unwrap();
        let sender = io.channel();
        let thread = io.spawn();
        let mut enter = Keystroke::parse("enter").unwrap();
        enter.key_char = Some("\n".into());
        let mut command = b"printf '%s%s' ENTER _OK".to_vec();
        command.extend(key_bytes(&enter, false).unwrap());
        sender.send(Msg::Input(command.into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output;
        loop {
            output = term
                .lock()
                .renderable_content()
                .display_iter
                .map(|c| c.c)
                .collect::<String>();
            if output.contains("ENTER_OK")
                && output.contains("PTY_OK /private/tmp")
                && output.contains("10 100")
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = sender.send(Msg::Shutdown);
                panic!("PTY output missing: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = sender.send(Msg::Shutdown);
        thread.join().unwrap();
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        actual: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<_> = self.marked.encode_utf16().collect();
        let range = range.start.min(utf16.len())..range.end.min(utf16.len());
        *actual = Some(range.clone());
        Some(String::from_utf16_lossy(&utf16[range]))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked.clear();
        self.input(text.as_bytes().to_vec(), cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = text.into();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.term.lock().grid().cursor.point;
        Some(Bounds::new(
            self.bounds.origin
                + point(
                    px(cursor.column.0 as f32 * self.cell_width),
                    px(cursor.line.0 as f32 * HEIGHT),
                ),
            size(px(self.cell_width), px(HEIGHT)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}
