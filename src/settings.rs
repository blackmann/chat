//! What survives a restart, stored as JSON in Application Support: the projects on
//! the rail and the agents that can be started. No credentials live here; each
//! agent uses whatever sign-in its own CLI already holds.
pub use crate::agent::AgentSpec;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What sending another message does while the agent is still working.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowUpMode {
    /// Hold the message until the current turn finishes.
    #[default]
    Queue,
    /// Deliver the message to the running turn immediately.
    Steer,
}

/// A project on the rail: its main folder (where agents run and threads are
/// filed), the name it goes by, and any further folders it spans.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedEntry")]
pub struct ProjectEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<PathBuf>,
}

/// Earlier settings listed projects as bare paths.
#[derive(Deserialize)]
#[serde(untagged)]
enum SavedEntry {
    Path(PathBuf),
    Full {
        path: PathBuf,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        folders: Vec<PathBuf>,
    },
}
impl From<SavedEntry> for ProjectEntry {
    fn from(saved: SavedEntry) -> Self {
        match saved {
            SavedEntry::Path(path) => Self { path, name: None, folders: Vec::new() },
            SavedEntry::Full { path, name, folders } => Self { path, name, folders },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub projects: Vec<ProjectEntry>,
    /// Any command that speaks ACP on stdio can be listed here.
    pub agents: Vec<AgentSpec>,
    /// The agent new threads start with: the last one picked.
    pub agent: usize,
    /// Interface and code fonts by family name; unset means the default.
    pub ui_font: Option<String>,
    pub mono_font: Option<String>,
    /// How messages sent during a running turn are delivered.
    pub follow_up_mode: FollowUpMode,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_default_to_queue_mode() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.follow_up_mode, FollowUpMode::Queue);
    }

    #[test]
    fn projects_read_bare_paths_and_full_entries() {
        let settings: Settings =
            serde_json::from_str(r#"{"projects": ["/a", {"path": "/b", "name": "Bee", "folders": ["/c"]}]}"#).unwrap();
        assert_eq!(settings.projects[0], ProjectEntry { path: "/a".into(), name: None, folders: vec![] });
        assert_eq!(settings.projects[1].name.as_deref(), Some("Bee"));
        assert_eq!(settings.projects[1].folders, [PathBuf::from("/c")]);
        let again: Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(again.projects, settings.projects);
    }

    #[test]
    fn follow_up_mode_round_trips() {
        let settings = Settings { follow_up_mode: FollowUpMode::Steer, ..Default::default() };
        let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored.follow_up_mode, FollowUpMode::Steer);
    }
}
