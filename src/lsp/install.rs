//! Installing language servers on first use, into Chat's own folder, as Zed
//! does: nothing global is touched. npm servers go into a private prefix with
//! the user's own npm; rust-analyzer is its release binary; gopls is built
//! with `go install` into a private GOBIN. A server already on PATH is
//! always preferred over one installed here.
use super::{ServerSpec, registry};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, LazyLock, Mutex},
};

/// How a default server is installed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Installer {
    /// `packages` into a prefix named `dir`; the server is `bin` in its `.bin`.
    /// Several servers can share one package (JSON, CSS and HTML do).
    Npm { dir: &'static str, packages: &'static [&'static str], bin: &'static str },
    /// The release binary, gzipped, for this machine.
    Release { dir: &'static str, bin: &'static str },
    /// `go install module` with a private GOBIN.
    Go { dir: &'static str, module: &'static str, bin: &'static str },
}

impl Installer {
    fn dir(self) -> &'static str {
        match self {
            Self::Npm { dir, .. } | Self::Release { dir, .. } | Self::Go { dir, .. } => dir,
        }
    }

    /// Where the server's command ends up, under `root`.
    fn bin_in(self, root: &Path) -> PathBuf {
        let dir = root.join(self.dir());
        match self {
            Self::Npm { bin, .. } => dir.join("node_modules/.bin").join(bin),
            Self::Release { bin, .. } => dir.join(bin),
            Self::Go { bin, .. } => dir.join("bin").join(bin),
        }
    }

    /// What has to be on PATH to install it.
    fn needs(self) -> (&'static str, &'static str) {
        match self {
            Self::Npm { .. } => ("npm", "Node.js (npm)"),
            Self::Release { .. } => ("curl", "curl"),
            Self::Go { .. } => ("go", "Go"),
        }
    }
}

/// The installer for a server, when it's one of the defaults run as they come:
/// a server set up with its own command in settings is the user's to install.
pub fn installer(spec: &ServerSpec) -> Option<Installer> {
    let default = ServerSpec::defaults().into_iter().find(|d| d.name == spec.name)?;
    if default.command != spec.command {
        return None;
    }
    let shared = |bin| Installer::Npm { dir: "vscode-langservers-extracted", packages: &["vscode-langservers-extracted"], bin };
    Some(match spec.name.as_str() {
        "typescript-language-server" => Installer::Npm {
            dir: "typescript-language-server",
            packages: &["typescript-language-server", "typescript"],
            bin: "typescript-language-server",
        },
        "vscode-json-language-server" => shared("vscode-json-language-server"),
        "vscode-css-language-server" => shared("vscode-css-language-server"),
        "vscode-html-language-server" => shared("vscode-html-language-server"),
        "yaml-language-server" => Installer::Npm { dir: "yaml-language-server", packages: &["yaml-language-server"], bin: "yaml-language-server" },
        "pyright" => Installer::Npm { dir: "pyright", packages: &["pyright"], bin: "pyright-langserver" },
        "rust-analyzer" if release_url("rust-analyzer").is_some() => Installer::Release { dir: "rust-analyzer", bin: "rust-analyzer" },
        "gopls" => Installer::Go { dir: "gopls", module: "golang.org/x/tools/gopls@latest", bin: "gopls" },
        _ => return None,
    })
}

/// The latest release of a binary server for this machine.
fn release_url(name: &str) -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    let target = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        _ => return None,
    };
    match name {
        "rust-analyzer" => Some(format!("https://github.com/rust-lang/rust-analyzer/releases/latest/download/rust-analyzer-{arch}-{target}.gz")),
        _ => None,
    }
}

const TS: &str = "typescript-language-server";

/// The TypeScript a TypeScript server should use: the project's own (in its
/// `node_modules`, or one above it, as in a monorepo), so the editor matches the
/// build; else the one Chat installs beside the server.
pub fn tsdk(root: &Path) -> Option<PathBuf> {
    let lib = |dir: &Path| {
        let lib = dir.join("node_modules/typescript/lib");
        lib.join("tsserver.js").is_file().then_some(lib)
    };
    root.ancestors().find_map(lib).or_else(|| lib(&servers_dir()?.join(TS)))
}

/// Whether the project brings its own TypeScript.
pub fn project_tsdk(root: &Path) -> bool {
    root.ancestors().any(|d| d.join("node_modules/typescript/lib/tsserver.js").is_file())
}

/// Whether a server needs installing before it can run at `root`: its command
/// is nowhere, or, for TypeScript, there's no TypeScript for it to use. A
/// TypeScript server installed on its own refuses to start without one.
pub fn needs_install(spec: &ServerSpec, root: &Path, path: Option<&str>) -> bool {
    locate(spec, path).is_none() || (spec.name == TS && tsdk(root).is_none())
}

/// Options to start a server with, beyond its own: where TypeScript is, when
/// it's Chat's and not the project's.
pub fn options(spec: &ServerSpec, root: &Path) -> Option<serde_json::Value> {
    let mut options = spec.options.clone();
    if spec.name == TS && !project_tsdk(root) && let Some(lib) = tsdk(root) {
        let all = options.get_or_insert_with(|| serde_json::json!({}));
        if let Some(map) = all.as_object_mut() {
            map.insert("tsserver".into(), serde_json::json!({ "path": lib }));
        }
    }
    options
}

/// Chat's folder for the servers it installs.
pub fn servers_dir() -> Option<PathBuf> {
    Some(crate::settings::dir()?.join("servers"))
}

fn dir_has_typescript(root: &Path) -> bool {
    root.join(TS).join("node_modules/typescript/lib/tsserver.js").is_file()
}

fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The command a server Chat installed runs as, if it's there.
pub fn installed(spec: &ServerSpec) -> Option<PathBuf> {
    let bin = installer(spec)?.bin_in(&servers_dir()?);
    runnable(&bin).then_some(bin)
}

/// Where to run a server from: its command on PATH, else Chat's own install.
pub fn locate(spec: &ServerSpec, path: Option<&str>) -> Option<PathBuf> {
    registry::which(&spec.command, path).filter(|p| !registry::proxy_missing(p)).or_else(|| installed(spec))
}

/// Whether the server runs from Chat's own install, which it can update.
pub fn managed(spec: &ServerSpec, path: Option<&str>) -> bool {
    registry::which(&spec.command, path).filter(|p| !registry::proxy_missing(p)).is_none() && installed(spec).is_some()
}

/// Why a server can't be installed here, if it can't.
pub fn missing(spec: &ServerSpec, path: Option<&str>) -> Option<String> {
    let Some(how) = installer(spec) else {
        return Some(format!("`{}` isn't on your PATH.", spec.command));
    };
    let (tool, what) = how.needs();
    registry::which(tool, path).is_none().then(|| format!("Installing it needs {what}, which isn't on your PATH."))
}

/// One install at a time per folder: JSON, CSS and HTML share theirs.
fn lock(dir: &str) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(Mutex::default);
    LOCKS.lock().unwrap().entry(dir.to_string()).or_default().clone()
}

/// What an install printed, and whether it worked.
pub struct Outcome {
    pub result: Result<PathBuf, String>,
    pub output: Vec<String>,
}

/// Install a server, or with `latest` install it again at its newest. Blocks
/// until done: run it off the UI thread.
pub fn install(spec: &ServerSpec, path: Option<&str>, latest: bool) -> Outcome {
    let mut output = Vec::new();
    let result = (|| {
        let how = installer(spec).ok_or_else(|| format!("{} can't be installed by Chat", spec.name))?;
        if let Some(why) = missing(spec, path) {
            return Err(why);
        }
        let root = servers_dir().ok_or("there's no folder to install into")?;
        let guard = lock(how.dir());
        let _held = guard.lock().unwrap();
        let bin = how.bin_in(&root);
        // Another server sharing the package may have just installed it. A
        // TypeScript server is only whole with TypeScript beside it.
        let whole = runnable(&bin) && (spec.name != TS || dir_has_typescript(&root));
        if !latest && whole {
            output.push(format!("already installed at {}", bin.display()));
            return Ok(bin);
        }
        let dir = root.join(how.dir());
        std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        let mut cmd = match how {
            Installer::Npm { packages, .. } => {
                let mut cmd = Command::new(registry::which("npm", path).ok_or("npm isn't on PATH")?);
                cmd.args(["install", "--no-audit", "--no-fund", "--no-save", "--loglevel=error", "--prefix"]).arg(&dir);
                cmd.args(packages.iter().map(|p| if latest { format!("{p}@latest") } else { p.to_string() }));
                cmd
            }
            Installer::Release { bin: name, .. } => {
                let url = release_url(name).ok_or("no release for this machine")?;
                // Downloaded whole, then moved into place, so a broken download
                // never leaves a half-written server behind.
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", r#"curl -fsSL --retry 2 "$1" | gunzip > "$2.part" && chmod 755 "$2.part" && mv -f "$2.part" "$2""#, "sh", &url])
                    .arg(&bin);
                output.push(format!("downloading {url}"));
                cmd
            }
            Installer::Go { module, .. } => {
                let mut cmd = Command::new(registry::which("go", path).ok_or("go isn't on PATH")?);
                cmd.args(["install", module]).env("GOBIN", dir.join("bin"));
                cmd
            }
        };
        if let Some(path) = path {
            cmd.env("PATH", path);
        }
        output.push(format!("installing {} into {}", spec.name, dir.display()));
        let ran = cmd.current_dir(&dir).output().map_err(|e| format!("couldn't run the installer: {e}"))?;
        output.extend(String::from_utf8_lossy(&ran.stdout).lines().chain(String::from_utf8_lossy(&ran.stderr).lines()).map(str::to_string));
        if !ran.status.success() {
            let last = output.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            return Err(format!("the install failed: {last}"));
        }
        if !runnable(&bin) {
            return Err(format!("the install finished, but {} isn't there", bin.display()));
        }
        output.push(format!("installed {}", bin.display()));
        Ok(bin)
    })();
    Outcome { result, output }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ServerSpec {
        ServerSpec::defaults().into_iter().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn knows_how_to_install_the_common_servers() {
        assert!(matches!(installer(&spec("typescript-language-server")), Some(Installer::Npm { packages, .. }) if packages.contains(&"typescript")));
        // JSON, CSS and HTML share one package and one folder.
        let dirs: Vec<_> = ["vscode-json-language-server", "vscode-css-language-server", "vscode-html-language-server"]
            .iter()
            .map(|n| installer(&spec(n)).unwrap().dir())
            .collect();
        assert!(dirs.iter().all(|d| *d == "vscode-langservers-extracted"));
        assert!(matches!(installer(&spec("gopls")), Some(Installer::Go { .. })));
        assert!(installer(&spec("clangd")).is_none());
        // A server pointed at another command is the user's own.
        let mut own = spec("typescript-language-server");
        own.command = "/opt/tsls".into();
        assert!(installer(&own).is_none());
    }

    #[test]
    fn puts_commands_where_installers_leave_them() {
        let root = Path::new("/x");
        assert_eq!(
            installer(&spec("vscode-css-language-server")).unwrap().bin_in(root),
            Path::new("/x/vscode-langservers-extracted/node_modules/.bin/vscode-css-language-server")
        );
        assert_eq!(installer(&spec("gopls")).unwrap().bin_in(root), Path::new("/x/gopls/bin/gopls"));
    }

    #[test]
    fn prefers_the_projects_own_typescript() {
        let base = std::env::temp_dir().join(format!("chat-tsdk-{}", std::process::id()));
        let app = base.join("packages/app");
        std::fs::create_dir_all(&app).unwrap();
        // Nothing yet: no TypeScript of the project's, nor (in this folder) Chat's.
        assert!(!project_tsdk(&app));
        // Hoisted to the monorepo's root.
        let lib = base.join("node_modules/typescript/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("tsserver.js"), "").unwrap();
        assert!(project_tsdk(&app));
        assert_eq!(tsdk(&app), Some(lib));
        // With its own, the server is left to find it: no path is forced on it.
        let ts = spec(TS);
        assert!(options(&ts, &app).is_none());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn says_what_an_install_needs() {
        let ts = spec("typescript-language-server");
        let why = missing(&ts, Some("/nonexistent")).unwrap();
        assert!(why.contains("Node.js"), "{why}");
        assert!(missing(&spec("clangd"), Some("/nonexistent")).unwrap().contains("isn't on your PATH"));
    }
}
