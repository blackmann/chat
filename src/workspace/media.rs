//! Images and videos open in a tab as pictures rather than text. Images GPUI
//! can decode are drawn as they are; the rest, and videos, are drawn from a
//! still that Quick Look renders. Videos play in the system's player.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
use std::sync::Arc;
use gpui_component::{Sizable, button::{Button, ButtonVariants}};

/// What kind of picture a file makes, judged by its extension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    /// Drawn straight from the file.
    Image,
    /// An image GPUI can't decode, drawn from a Quick Look still.
    Still,
    /// Drawn from a Quick Look still of its first frame.
    Video,
}

pub(super) fn kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" => Kind::Image,
        "ico" | "icns" | "heic" | "heif" | "avif" => Kind::Still,
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" => Kind::Video,
        _ => return None,
    })
}

/// An open image or video, and the picture that stands for it.
pub struct Media {
    pub(super) kind: Kind,
    /// The file to draw: `None` while a still is made, `Err` if none could be.
    pub(super) shown: Option<Result<PathBuf, ()>>,
}

/// Have Quick Look render a still of `path`, at most `size` pixels on a side.
fn still(path: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    std::fs::metadata(path).and_then(|m| m.modified()).ok().hash(&mut hash);
    let dir = std::env::temp_dir().join("chat-stills").join(format!("{:016x}", hash.finish()));
    let out = dir.join(format!("{}.png", path.file_name()?.to_string_lossy()));
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(&dir).ok()?;
    std::process::Command::new("qlmanage")
        .args(["-t", "-s", "1600", "-o"])
        .arg(&dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    out.exists().then_some(out)
}

/// A picture scaled down to fit the space it's given, never spilling out of it:
/// sized from the box rather than the image, so a tall picture can't push past
/// the bottom.
fn fitted(picture: Img) -> Div {
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(picture.absolute().top_0().left_0().size_full().object_fit(ObjectFit::ScaleDown))
}

/// The bordered box a side of an image's change sits in.
fn frame(colors: Palette) -> Div {
    div().size_full().rounded(px(6.)).border_1().border_color(rgb(colors.border)).bg(rgb(colors.background))
}

/// An image's change: what it was and what it is, side by side. A side that
/// didn't exist says so.
pub(super) fn image_diff(colors: Palette, old: Option<&Arc<Image>>, new: Option<&Arc<Image>>) -> AnyElement {
    let side = |title: &'static str, tint: u32, image: Option<&Arc<Image>>, absent: &'static str| {
        let picture = match image {
            Some(image) => frame(colors).p_3().child(fitted(img(image.clone()))).into_any_element(),
            None => frame(colors)
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(colors.quiet))
                .text_size(px(crate::theme::scaled_ui(12.)))
                .child(absent)
                .into_any_element(),
        };
        col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .child(
                row()
                    .flex_shrink_0()
                    .gap(px(6.))
                    .child(div().size(px(7.)).rounded_full().bg(rgb(tint)))
                    .child(label(colors, title))
                    .when_some(image.and_then(|i| size_label(&i.bytes)), |s, size| {
                        s.child(div().text_color(rgb(colors.quiet)).text_size(px(crate::theme::scaled_ui(11.5))).child(size))
                    }),
            )
            .child(div().flex_1().min_h_0().child(picture))
    };
    row()
        .size_full()
        .min_h_0()
        .gap_4()
        .p_4()
        .bg(rgb(colors.panel))
        .map(|mut d| {
            d.style().align_items = Some(AlignItems::Stretch);
            d
        })
        .child(side("Before", colors.red, old, "Didn't exist"))
        .child(side("After", colors.green, new, "Deleted"))
        .into_any_element()
}

/// A file's size, briefly.
fn size_label(bytes: &[u8]) -> Option<String> {
    let n = bytes.len() as f64;
    Some(if n < 1024. {
        format!("{n} B")
    } else if n < 1024. * 1024. {
        format!("{:.0} KB", n / 1024.)
    } else {
        format!("{:.1} MB", n / 1024. / 1024.)
    })
}

impl Workspace {
    /// Start showing `path` as a picture, making its still in the background.
    pub(super) fn open_media(&mut self, path: PathBuf, kind: Kind, cx: &mut Context<Self>) {
        if kind == Kind::Image {
            self.media.insert(path.clone(), Media { kind, shown: Some(Ok(path)) });
            return;
        }
        self.media.insert(path.clone(), Media { kind, shown: None });
        cx.spawn(async move |this, cx| {
            let read = path.clone();
            let shown = cx.background_executor().spawn(async move { still(&read) }).await;
            let _ = this.update(cx, |s, cx| {
                // The tab may have closed meanwhile.
                if let Some(media) = s.media.get_mut(&path) {
                    media.shown = Some(shown.ok_or(()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn media_view(&self, path: &Path, media: &Media, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let relative = self.relative(path);
        let video = media.kind == Kind::Video;
        let glyph = if video { Glyph::FileVideo } else { Glyph::FileImage };
        let play = {
            let path = path.to_path_buf();
            Button::new("media-open")
                .label(if video { "Play in default app" } else { "Open in default app" })
                .small()
                .ghost()
                .on_click(move |_, _, cx| cx.open_with_system(&path))
        };
        let picture = match &media.shown {
            None => return col().size_full().items_center().justify_center().child(spinner(colors)).into_any_element(),
            Some(Err(())) => {
                let why = if video { "No preview could be made of this video." } else { "This image couldn't be read." };
                return empty(colors, glyph, relative, why).child(play).into_any_element();
            }
            Some(Ok(shown)) => shown.clone(),
        };
        col()
            .size_full()
            .min_h_0()
            .bg(rgb(colors.panel))
            .child(div().flex_1().min_h_0().p_4().child(fitted(img(picture))))
            .child(
                row()
                    .flex_shrink_0()
                    .bg(rgb(colors.panel))
                    .justify_center()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .child(ui::icon(glyph).size(px(14.)).text_color(rgb(colors.quiet)))
                    .child(label(colors, relative))
                    .child(play),
            )
            .into_any_element()
    }
}
