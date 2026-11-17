//! App-specific glyphs (Lucide, ISC license) layered over GPUI Component's icon set.
use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;
use std::borrow::Cow;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Message,
    Compose,
    Branch,
    Terminal,
    FileDiff,
    FileCode,
    FileText,
    Files,
    Stop,
    ShieldCheck,
    Sparkle,
    Gauge,
    CircleDashed,
    CircleDot,
    Pin,
    Pencil,
    Plug,
    Brain,
}

const PREFIX: &str = "app-icons/";

impl Glyph {
    #[cfg(test)]
    pub const ALL: [Glyph; 18] = [
        Self::Message,
        Self::Compose,
        Self::Branch,
        Self::Terminal,
        Self::FileDiff,
        Self::FileCode,
        Self::FileText,
        Self::Files,
        Self::Stop,
        Self::ShieldCheck,
        Self::Sparkle,
        Self::Gauge,
        Self::CircleDashed,
        Self::CircleDot,
        Self::Pin,
        Self::Pencil,
        Self::Plug,
        Self::Brain,
    ];
    fn file(self) -> &'static str {
        match self {
            Self::Message => "message.svg",
            Self::Compose => "square-pen.svg",
            Self::Branch => "git-branch.svg",
            Self::Terminal => "terminal.svg",
            Self::FileDiff => "file-diff.svg",
            Self::FileCode => "file-code.svg",
            Self::FileText => "file-text.svg",
            Self::Files => "files.svg",
            Self::Stop => "stop.svg",
            Self::ShieldCheck => "shield-check.svg",
            Self::Sparkle => "sparkle.svg",
            Self::Gauge => "gauge.svg",
            Self::CircleDashed => "circle-dashed.svg",
            Self::CircleDot => "circle-dot.svg",
            Self::Pin => "pin.svg",
            Self::Pencil => "pencil.svg",
            Self::Plug => "plug.svg",
            Self::Brain => "brain.svg",
        }
    }
}

impl IconNamed for Glyph {
    fn path(self) -> SharedString {
        format!("{PREFIX}{}", self.file()).into()
    }
}

fn svg(file: &str) -> Option<&'static str> {
    Some(match file {
        "message.svg" => r#"<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>"#,
        "square-pen.svg" => {
            r#"<path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/>"#
        }
        "git-branch.svg" => {
            r#"<line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/>"#
        }
        "terminal.svg" => r#"<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>"#,
        "file-diff.svg" => {
            r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M9 10h6"/><path d="M12 13V7"/><path d="M9 17h6"/>"#
        }
        "file-code.svg" => {
            r#"<path d="M10 12.5 8 15l2 2.5"/><path d="m14 12.5 2 2.5-2 2.5"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z"/>"#
        }
        "file-text.svg" => {
            r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/>"#
        }
        "files.svg" => {
            r#"<path d="M20 7h-3a2 2 0 0 1-2-2V2"/><path d="M9 18a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v10a2 2 0 0 1-2 2Z"/><path d="M3 7.6v12.8A1.6 1.6 0 0 0 4.6 22h9.8"/>"#
        }
        "stop.svg" => r#"<rect width="14" height="14" x="5" y="5" rx="2"/>"#,
        "shield-check.svg" => {
            r#"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>"#
        }
        "sparkle.svg" => {
            r#"<path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/>"#
        }
        "gauge.svg" => r#"<path d="m12 14 4-4"/><path d="M3.34 19a10 10 0 1 1 17.32 0"/>"#,
        "circle-dashed.svg" => {
            r#"<path d="M10.1 2.182a10 10 0 0 1 3.8 0"/><path d="M13.9 21.818a10 10 0 0 1-3.8 0"/><path d="M17.609 3.721a10 10 0 0 1 2.69 2.7"/><path d="M2.182 13.9a10 10 0 0 1 0-3.8"/><path d="M20.279 17.609a10 10 0 0 1-2.7 2.69"/><path d="M21.818 10.1a10 10 0 0 1 0 3.8"/><path d="M3.721 6.391a10 10 0 0 1 2.7-2.69"/><path d="M6.391 20.279a10 10 0 0 1-2.69-2.7"/>"#
        }
        "circle-dot.svg" => r#"<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="1"/>"#,
        "pin.svg" => {
            r#"<path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/>"#
        }
        "pencil.svg" => {
            r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>"#
        }
        "plug.svg" => {
            r#"<path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"/>"#
        }
        "brain.svg" => {
            r#"<path d="M12 18V5"/><path d="M15 13a4.17 4.17 0 0 1-3-4 4.17 4.17 0 0 1-3 4"/><path d="M17.598 6.5A3 3 0 1 0 12 5a3 3 0 1 0-5.598 1.5"/><path d="M17.997 5.125a4 4 0 0 1 2.526 5.77"/><path d="M18 18a4 4 0 0 0 2-7.464"/><path d="M19.967 17.483A4 4 0 1 1 12 18a4 4 0 1 1-7.967-.517"/><path d="M6 18a4 4 0 0 1-2-7.464"/><path d="M6.003 5.125a4 4 0 0 0-2.526 5.77"/>"#
        }
        _ => return None,
    })
}

/// Serves app glyphs, then falls back to GPUI Component's embedded icons.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(body) = path.strip_prefix(PREFIX).and_then(svg) {
            return Ok(Some(Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
                )
                .into_bytes(),
            )));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_glyph_resolves() {
        for g in Glyph::ALL {
            assert!(Assets.load(&g.path()).unwrap().is_some(), "{g:?}");
        }
        assert!(Assets.load("icons/search.svg").unwrap().is_some());
    }
}
