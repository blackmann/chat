//! What survives a restart, stored as JSON in Application Support: the projects on
//! the rail and the agents that can be started. No credentials live here; each
//! agent uses whatever sign-in its own CLI already holds.
pub use crate::agent::AgentSpec;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub projects: Vec<PathBuf>,
    /// Any command that speaks ACP on stdio can be listed here.
    pub agents: Vec<AgentSpec>,
    /// The agent new threads start with: the last one picked.
    pub agent: usize,
    /// Interface and code fonts by family name; unset means the default.
    pub ui_font: Option<String>,
    pub mono_font: Option<String>,
}

/// Where settings live; `CHAT_CONFIG_DIR` overrides it, for trying things out.
pub fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CHAT_CONFIG_DIR") {
        return Some(dir.into());
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Chat")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("chat")
    })
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = dir()
            .and_then(|d| std::fs::read(d.join("settings.json")).ok())
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default();
        if settings.agents.is_empty() {
            settings.agents = AgentSpec::defaults();
        }
        settings.agent = settings.agent.min(settings.agents.len() - 1);
        settings
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let dir = dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("settings.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
    /// Where threads that belong to no project run.
    pub fn scratch() -> PathBuf {
        let dir = dir().unwrap_or_else(std::env::temp_dir).join("scratch");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}
