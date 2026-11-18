//! Images and videos open in a tab as pictures rather than text. Images GPUI
//! can decode are drawn as they are; the rest, and videos, are drawn from a
//! still that Quick Look renders. Videos play in the system's player.
use super::ui::*;
use super::*;
use crate::icons::Glyph;
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .child(img(picture).size_full().object_fit(ObjectFit::ScaleDown)),
            )
            .child(
                row()
                    .flex_shrink_0()
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
