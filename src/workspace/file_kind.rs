//! What a file looks like in lists: a glyph by kind, and a tint by language.
use super::syntax;
use crate::{icons::Glyph, theme::Palette};
use gpui::SharedString;
use gpui_component::{IconName, IconNamed};
use std::path::Path;

/// A tint, resolved against the palette so it follows light and dark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tint {
    Muted,
    Blue,
    Purple,
    Green,
    Yellow,
    Red,
    Cyan,
    Orange,
    Indigo,
}

impl Tint {
    pub(super) fn color(self, colors: Palette) -> u32 {
        match self {
            Self::Muted => colors.muted,
            Self::Blue => colors.lane(0),
            Self::Purple => colors.lane(1),
            Self::Green => colors.lane(2),
            Self::Yellow => colors.lane(3),
            Self::Red => colors.lane(4),
            Self::Cyan => colors.lane(5),
            Self::Orange => colors.lane(6),
            Self::Indigo => colors.lane(7),
        }
    }
}

/// The glyph and tint for a file, by its name first and then its extension.
pub(super) fn file_kind(path: &Path) -> (SharedString, Tint) {
    use Tint::*;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lower = name.to_ascii_lowercase();
    let g = |g: Glyph| g.path();

    // Well-known names.
    let by_name = match lower.as_str() {
        "cargo.toml" => Some((g(Glyph::Package), Orange)),
        "package.json" | "deno.json" | "go.mod" | "pyproject.toml" | "gemfile" | "pubspec.yaml" => {
            Some((g(Glyph::Package), Red))
        }
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "go.sum"
        | "poetry.lock" | "gemfile.lock" | "flake.lock" => Some((g(Glyph::FileLock), Muted)),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => Some((g(Glyph::Branch), Orange)),
        "dockerfile" | "containerfile" | ".dockerignore" | "docker-compose.yml" | "compose.yaml" => {
            Some((g(Glyph::Package), Blue))
        }
        "makefile" | "justfile" | "rakefile" | "procfile" => Some((g(Glyph::FileTerminal), Yellow)),
        "license" | "license.md" | "license.txt" | "copying" => Some((g(Glyph::FileKey), Yellow)),
        _ if lower.starts_with("readme") => Some((g(Glyph::FileText), Blue)),
        _ if lower == ".env" || lower.starts_with(".env.") => Some((g(Glyph::FileKey), Yellow)),
        _ => None,
    };
    if let Some(kind) = by_name {
        return kind;
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // Code, tinted by language.
        "rs" => (g(Glyph::FileCode), Orange),
        "ts" | "tsx" | "mts" | "cts" => (g(Glyph::FileCode), Blue),
        "js" | "jsx" | "mjs" | "cjs" => (g(Glyph::FileCode), Yellow),
        "py" | "pyi" => (g(Glyph::FileCode), Blue),
        "go" => (g(Glyph::FileCode), Cyan),
        "swift" => (g(Glyph::FileCode), Orange),
        "kt" | "kts" => (g(Glyph::FileCode), Purple),
        "java" | "scala" => (g(Glyph::FileCode), Red),
        "c" | "h" => (g(Glyph::FileCode), Indigo),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "m" | "mm" => (g(Glyph::FileCode), Indigo),
        "cs" | "fs" => (g(Glyph::FileCode), Purple),
        "rb" | "erb" => (g(Glyph::FileCode), Red),
        "php" => (g(Glyph::FileCode), Indigo),
        "lua" | "zig" => (g(Glyph::FileCode), Orange),
        "ex" | "exs" | "erl" | "hs" | "ml" | "clj" | "elm" => (g(Glyph::FileCode), Purple),
        "dart" => (g(Glyph::FileCode), Cyan),
        "html" | "htm" => (g(Glyph::FileCode), Orange),
        "css" => (g(Glyph::FileCode), Blue),
        "scss" | "sass" | "less" => (g(Glyph::FileCode), Purple),
        "vue" => (g(Glyph::FileCode), Green),
        "svelte" | "astro" => (g(Glyph::FileCode), Orange),
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => (g(Glyph::FileTerminal), Green),
        "sql" | "db" | "sqlite" | "sqlite3" | "prisma" => (g(Glyph::Database), Cyan),
        "graphql" | "gql" | "proto" => (g(Glyph::Braces), Purple),

        // Data and config.
        "json" | "jsonc" | "json5" => (g(Glyph::FileJson), Yellow),
        "toml" | "ini" | "cfg" | "conf" | "properties" | "editorconfig" => (g(Glyph::FileSliders), Muted),
        "yaml" | "yml" => (g(Glyph::FileSliders), Purple),
        "xml" | "plist" => (g(Glyph::FileCode), Orange),
        "lock" => (g(Glyph::FileLock), Muted),
        "env" => (g(Glyph::FileKey), Yellow),
        "pem" | "key" | "crt" | "cer" | "pub" | "gpg" | "asc" => (g(Glyph::FileKey), Yellow),
        "csv" | "tsv" | "xls" | "xlsx" | "numbers" | "parquet" => (g(Glyph::FileSpreadsheet), Green),

        // Prose.
        "md" | "mdx" | "markdown" => (g(Glyph::FileText), Blue),
        "txt" | "rst" | "adoc" | "org" | "log" => (g(Glyph::FileText), Muted),
        "pdf" => (g(Glyph::FileText), Red),

        // Media.
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "avif" | "heic" | "tiff" => {
            (g(Glyph::FileImage), Purple)
        }
        "svg" => (g(Glyph::FileImage), Yellow),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => (g(Glyph::FileAudio), Red),
        "mp4" | "mov" | "mkv" | "webm" | "avi" => (g(Glyph::FileVideo), Red),
        "ttf" | "otf" | "woff" | "woff2" => (g(Glyph::FileType), Red),
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "dmg" | "jar" => {
            (g(Glyph::FileArchive), Yellow)
        }

        "" => (IconName::File.path(), Muted),
        _ if syntax::language(&path.to_string_lossy()).is_some() => (g(Glyph::FileCode), Muted),
        _ => (IconName::File.path(), Muted),
    }
}
