//! Which server speaks for a file, where its project starts, and where its
//! command lives.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// How to start a language server, and what it serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// LSP language ids: `rust`, `typescript`, `typescriptreact`, `json`…
    pub languages: Vec<String>,
    /// Files whose folder is a project root for this server. The outermost
    /// one inside the project wins, so a workspace gets one server, not one per member.
    #[serde(default)]
    pub roots: Vec<String>,
    /// Passed as `initializationOptions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
    /// Set in settings to turn a default server off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl ServerSpec {
    fn new(name: &str, command: &str, args: &[&str], languages: &[&str], roots: &[&str]) -> Self {
        let strings = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
        Self {
            name: name.into(),
            command: command.into(),
            args: strings(args),
            languages: strings(languages),
            roots: strings(roots),
            options: None,
            disabled: false,
        }
    }

    /// The servers known out of the box. Where two serve the same language, the
    /// first one installed is used.
    pub fn defaults() -> Vec<Self> {
        let js = ["typescript", "typescriptreact", "javascript", "javascriptreact"];
        vec![
            Self::new("rust-analyzer", "rust-analyzer", &[], &["rust"], &["Cargo.toml"]),
            Self::new("typescript-language-server", "typescript-language-server", &["--stdio"], &js, &["tsconfig.json", "jsconfig.json", "package.json"]),
            Self::new("vscode-json-language-server", "vscode-json-language-server", &["--stdio"], &["json", "jsonc"], &[]),
            Self::new("vscode-css-language-server", "vscode-css-language-server", &["--stdio"], &["css", "scss", "less"], &["package.json"]),
            Self::new("vscode-html-language-server", "vscode-html-language-server", &["--stdio"], &["html"], &["package.json"]),
            Self::new("gopls", "gopls", &[], &["go"], &["go.work", "go.mod"]),
            Self::new("basedpyright", "basedpyright-langserver", &["--stdio"], &["python"], &["pyproject.toml", "setup.py", "requirements.txt"]),
            Self::new("pyright", "pyright-langserver", &["--stdio"], &["python"], &["pyproject.toml", "setup.py", "requirements.txt"]),
            Self::new("clangd", "clangd", &[], &["c", "cpp", "objective-c"], &["compile_commands.json", "CMakeLists.txt", ".clangd"]),
            Self::new("yaml-language-server", "yaml-language-server", &["--stdio"], &["yaml"], &[]),
            Self::new("taplo", "taplo", &["lsp", "stdio"], &["toml"], &[]),
        ]
    }

    /// The defaults with the user's entries laid over them: one with a default's
    /// name replaces it (or, `disabled`, removes it); new names are added first,
    /// so they win over a default for the same language.
    pub fn merged(user: &[Self]) -> Vec<Self> {
        let mut all: Vec<Self> = user.iter().filter(|s| !s.disabled).cloned().collect();
        all.extend(Self::defaults().into_iter().filter(|d| !user.iter().any(|u| u.name == d.name)));
        all
    }
}

/// The LSP language id for a file, by its name.
pub fn language_id(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "tsconfig.json" | "jsconfig.json" | ".eslintrc" | ".babelrc" => return Some("jsonc"),
        _ => {}
    }
    let ext = name.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "json" => "json",
        "jsonc" | "json5" => "jsonc",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "html" | "htm" => "html",
        "go" => "go",
        "py" | "pyi" => "python",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "m" | "mm" => "objective-c",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        _ => return None,
    })
}

/// Where a server for `file` should be rooted: the outermost folder between
/// the file and its project folder that holds one of `markers`, else the
/// project folder. A file outside every project folder is rooted at the
/// nearest marker above it, else its own folder.
pub fn root_for(file: &Path, markers: &[String], projects: &[PathBuf]) -> PathBuf {
    let dir = file.parent().unwrap_or(file);
    let has_marker = |d: &Path| markers.iter().any(|m| d.join(m).exists());
    if let Some(project) = projects.iter().filter(|p| file.starts_with(p)).max_by_key(|p| p.as_os_str().len()) {
        let outermost = dir.ancestors().take_while(|d| d.starts_with(project)).filter(|d| has_marker(d)).last();
        return outermost.unwrap_or(project).to_path_buf();
    }
    dir.ancestors().find(|d| has_marker(d)).unwrap_or(dir).to_path_buf()
}

/// Where `command` is found on `path`, if anywhere. A path with a slash is
/// taken as it is.
pub fn which(command: &str, path: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if command.contains('/') {
        let p = PathBuf::from(command);
        return runnable(&p).then_some(p);
    }
    let path = path.map(str::to_string).or_else(|| std::env::var("PATH").ok())?;
    std::env::split_paths(&path).map(|dir| dir.join(command)).find(|p| runnable(p))
}

/// A rustup proxy stands in for components that may not be installed: it's on
/// PATH, but running it fails. Ask it before believing it.
pub fn proxy_missing(found: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(found) else { return false };
    if real.file_name().is_none_or(|n| n != "rustup") {
        return false;
    }
    let Some(name) = found.file_name() else { return false };
    let which = std::process::Command::new(&real).arg("which").arg(name).output();
    !which.is_ok_and(|o| o.status.success())
}

/// `file://` URI for an absolute path, percent-encoding what a URI can't hold.
pub fn uri(path: &Path) -> Option<lsp_types::Uri> {
    let mut out = String::from("file://");
    for b in path.to_str()?.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.parse().ok()
}

/// The path a `file://` URI names.
pub fn path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let rest = uri.as_str().strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// Places with the same file reached through different paths (a symlinked
/// folder, macOS's `/var` and `/private/var`) taken once, in the order given,
/// under the first path seen.
pub fn unique<R: PartialEq + Clone>(found: Vec<(PathBuf, R)>) -> Vec<(PathBuf, R)> {
    let mut seen: Vec<(PathBuf, R)> = Vec::new();
    let mut out = Vec::new();
    for (path, range) in found {
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.iter().any(|(p, r)| *p == real && *r == range) {
            seen.push((real, range.clone()));
            out.push((path, range));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_languages() {
        assert_eq!(language_id(Path::new("/a/main.rs")), Some("rust"));
        assert_eq!(language_id(Path::new("/a/App.TSX")), Some("typescriptreact"));
        assert_eq!(language_id(Path::new("/a/tsconfig.json")), Some("jsonc"));
        assert_eq!(language_id(Path::new("/a/package.json")), Some("json"));
        assert_eq!(language_id(Path::new("/a/README")), None);
    }

    #[test]
    fn uris_round_trip() {
        let p = Path::new("/Users/me/My Code/café #1/a+b.rs");
        let u = uri(p).unwrap();
        assert_eq!(u.as_str(), "file:///Users/me/My%20Code/caf%C3%A9%20%231/a%2Bb.rs");
        assert_eq!(path(&u).unwrap(), p);
    }

    #[test]
    fn user_entries_replace_add_and_disable() {
        let mut ra = ServerSpec::new("rust-analyzer", "/opt/ra", &[], &["rust"], &[]);
        let mut gone = ServerSpec::new("gopls", "", &[], &[], &[]);
        gone.disabled = true;
        let zls = ServerSpec::new("zls", "zls", &[], &["zig"], &[]);
        ra.options = Some(serde_json::json!({ "check": { "command": "clippy" } }));
        let all = ServerSpec::merged(&[ra.clone(), gone, zls]);
        assert_eq!(all[0], ra);
        assert_eq!(all[1].name, "zls");
        assert!(!all.iter().any(|s| s.name == "gopls"));
        assert_eq!(all.iter().filter(|s| s.name == "rust-analyzer").count(), 1);
    }

    #[test]
    fn roots_at_the_outermost_marker_in_the_project() {
        let dir = std::env::temp_dir().join(format!("chat-lsp-roots-{}", std::process::id()));
        let member = dir.join("crates/core/src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("crates/core/Cargo.toml"), "").unwrap();
        let markers = vec!["Cargo.toml".to_string()];
        let file = member.join("lib.rs");
        assert_eq!(root_for(&file, &markers, std::slice::from_ref(&dir)), dir);
        // Outside every project: the nearest marker.
        assert_eq!(root_for(&file, &markers, &[]), dir.join("crates/core"));
        // No marker: the project folder.
        assert_eq!(root_for(&file, &[], std::slice::from_ref(&dir)), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
