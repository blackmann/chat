//! Images on the system clipboard. GPUI hands back the text whenever there is
//! any, and a browser's "Copy Image" puts the image's address or markup there
//! alongside it, so on macOS the pasteboard is read directly for image data.

/// An image on the clipboard, as bytes in a format agents take, with its
/// extension. TIFF, what macOS apps often copy, comes back as PNG.
pub fn image(cx: &gpui::App) -> Option<(Vec<u8>, &'static str)> {
    #[cfg(all(target_os = "macos", not(test)))]
    if let Some(found) = mac::image() {
        return Some(found);
    }
    let item = cx.read_from_clipboard()?;
    item.entries().iter().find_map(|entry| match entry {
        gpui::ClipboardEntry::Image(image) => usable(image.bytes.clone(), image.format.mime_type()),
        _ => None,
    })
}

/// Files and folders on the clipboard, as Finder copies them. Finder puts
/// their names there as text too, and their icons as an image, so these come
/// before either.
pub fn files() -> Vec<std::path::PathBuf> {
    // Tests run against GPUI's own clipboard, not the machine's.
    #[cfg(all(target_os = "macos", not(test)))]
    return mac::files();
    #[cfg(any(not(target_os = "macos"), test))]
    Vec::new()
}

/// Text copied alongside an image that is worth pasting in its place: several
/// lines or words, as spreadsheets and slides copy, rather than a lone address.
pub fn prefers_text(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && !(text.lines().count() == 1 && !text.contains(char::is_whitespace) && text.contains("://"))
}

fn usable(bytes: Vec<u8>, mime: &str) -> Option<(Vec<u8>, &'static str)> {
    Some(match mime {
        "image/png" => (bytes, "png"),
        "image/jpeg" => (bytes, "jpg"),
        "image/gif" => (bytes, "gif"),
        "image/webp" => (bytes, "webp"),
        "image/tiff" | "image/bmp" => {
            let decoded = image::load_from_memory(&bytes).ok()?;
            let mut png = std::io::Cursor::new(Vec::new());
            decoded.write_to(&mut png, image::ImageFormat::Png).ok()?;
            (png.into_inner(), "png")
        }
        _ => return None,
    })
}

#[cfg(all(target_os = "macos", not(test)))]
#[allow(unexpected_cfgs)] // objc's macros test cfgs of their own.
mod mac {
    use cocoa::appkit::NSPasteboard;
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSData, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    /// Pasteboard types to look for, best first, with the MIME each holds.
    const TYPES: [(&str, &str); 5] = [
        ("public.png", "image/png"),
        ("public.jpeg", "image/jpeg"),
        ("com.compuserve.gif", "image/gif"),
        ("org.webmproject.webp", "image/webp"),
        ("public.tiff", "image/tiff"),
    ];

    pub fn files() -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        // SAFETY: as for `image`; strings are copied out before the pool drains.
        unsafe {
            let board: id = NSPasteboard::generalPasteboard(nil);
            if board == nil {
                return found;
            }
            let items: id = msg_send![board, pasteboardItems];
            if items == nil {
                return found;
            }
            let kind = NSString::alloc(nil).init_str("public.file-url");
            let count: usize = msg_send![items, count];
            for i in 0..count {
                let item: id = msg_send![items, objectAtIndex: i];
                let link: id = msg_send![item, stringForType: kind];
                if link == nil {
                    continue;
                }
                let url: id = msg_send![class!(NSURL), URLWithString: link];
                if url == nil {
                    continue;
                }
                let path: id = msg_send![url, path];
                if path == nil {
                    continue;
                }
                let utf8 = path.UTF8String();
                if !utf8.is_null() {
                    found.push(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned().into());
                }
            }
            let _: () = msg_send![kind, release];
        }
        found
    }

    pub fn image() -> Option<(Vec<u8>, &'static str)> {
        // SAFETY: AppKit's general pasteboard, read on the main thread as GPUI
        // does; the data is copied out before the autorelease pool drains.
        unsafe {
            let board: id = NSPasteboard::generalPasteboard(nil);
            if board == nil {
                return None;
            }
            for (kind, mime) in TYPES {
                let kind = NSString::alloc(nil).init_str(kind);
                let data: id = board.dataForType(kind);
                let _: () = msg_send![kind, release];
                if data == nil || data.length() == 0 || data.bytes().is_null() {
                    continue;
                }
                let bytes = std::slice::from_raw_parts(data.bytes() as *const u8, data.length() as usize).to_vec();
                if let Some(found) = super::usable(bytes, mime) {
                    return Some(found);
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_address_gives_way_to_the_image() {
        assert!(!prefers_text("https://example.com/cat.png"));
        assert!(!prefers_text(""));
        assert!(prefers_text("Name\tQty\nApples\t3"));
        assert!(prefers_text("a sentence copied from a slide"));
    }

    #[test]
    fn tiff_comes_back_as_png() {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut tiff = std::io::Cursor::new(Vec::new());
        img.write_to(&mut tiff, image::ImageFormat::Tiff).unwrap();
        let (png, ext) = usable(tiff.into_inner(), "image/tiff").unwrap();
        assert_eq!(ext, "png");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
