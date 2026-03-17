use anyhow::{Context as _, Result};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharedItemKind {
    File,
    Dir,
    Note,
}

impl SharedItemKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SharedItemKind::File => "file",
            SharedItemKind::Dir => "dir",
            SharedItemKind::Note => "note",
        }
    }
}

impl std::fmt::Display for SharedItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SharedItemKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file" => Ok(SharedItemKind::File),
            "dir" => Ok(SharedItemKind::Dir),
            "note" => Ok(SharedItemKind::Note),
            _ => Err(anyhow::anyhow!("unknown shared item kind: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedItem {
    pub id: i64,
    pub kind: SharedItemKind,
    /// For files/dirs: relative path from project.path. For notes: NULL.
    pub path: Option<String>,
    /// For notes: the note content. For files/dirs: NULL.
    pub content: Option<String>,
    /// Human-readable label for the shared item.
    pub label: Option<String>,
    /// For files/dirs/project notes: the owning project. For group notes: NULL.
    pub project_id: Option<i64>,
    /// For group notes: the group it belongs to. For files/dirs/project notes: NULL.
    pub group_id: Option<i64>,
    /// For group notes: the project that created it.
    pub created_by_project_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

// --- Workspace Config (for .ai-workspace.json) ---

/// A shared item entry in the config file.
/// String form = path only (no label). Object form = path + label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShareEntry {
    /// Just a path string, no label
    PathOnly(String),
    /// Object with path and optional label
    WithLabel { path: String, label: String },
}

impl ShareEntry {
    pub fn path(&self) -> &str {
        match self {
            ShareEntry::PathOnly(p) => p,
            ShareEntry::WithLabel { path, .. } => path,
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            ShareEntry::PathOnly(_) => None,
            ShareEntry::WithLabel { label, .. } => Some(label),
        }
    }
}

/// A note entry in the config file (project-scoped only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteEntry {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The `.ai-workspace.json` config file format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub share: Vec<ShareEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NoteEntry>,
}

/// Summary of what sync_from_config changed.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub groups_added: usize,
    pub groups_removed: usize,
    pub shares_added: usize,
    pub shares_removed: usize,
    pub notes_added: usize,
    pub notes_removed: usize,
    pub notes_updated: usize,
}

impl WorkspaceConfig {
    /// Load config from a JSON file. Returns descriptive error on malformed JSON.
    pub fn load(path: &Path) -> Result<Self> {
        debug!("Loading workspace config from {}", path.display());
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Self = serde_json::from_str(&content).with_context(|| {
            format!(
                "Malformed JSON in {}: expected valid .ai-workspace.json",
                path.display()
            )
        })?;
        info!(
            "Loaded workspace config: name={}, groups={}, shares={}, notes={}",
            config.name,
            config.groups.len(),
            config.share.len(),
            config.notes.len()
        );
        Ok(config)
    }

    /// Save config to a JSON file (pretty-printed with trailing newline).
    pub fn save(&self, path: &Path) -> Result<()> {
        debug!("Saving workspace config to {}", path.display());
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize workspace config")?;
        std::fs::write(path, format!("{}\n", json))
            .with_context(|| format!("Failed to write {}", path.display()))?;
        info!("Saved workspace config to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn shared_item_kind_as_str() {
        assert_eq!(SharedItemKind::File.as_str(), "file");
        assert_eq!(SharedItemKind::Dir.as_str(), "dir");
        assert_eq!(SharedItemKind::Note.as_str(), "note");
    }

    #[test]
    fn shared_item_kind_display() {
        assert_eq!(format!("{}", SharedItemKind::File), "file");
        assert_eq!(format!("{}", SharedItemKind::Dir), "dir");
        assert_eq!(format!("{}", SharedItemKind::Note), "note");
    }

    #[test]
    fn shared_item_kind_from_str() {
        assert_eq!(
            SharedItemKind::from_str("file").unwrap(),
            SharedItemKind::File
        );
        assert_eq!(
            SharedItemKind::from_str("dir").unwrap(),
            SharedItemKind::Dir
        );
        assert_eq!(
            SharedItemKind::from_str("note").unwrap(),
            SharedItemKind::Note
        );
    }

    #[test]
    fn shared_item_kind_from_str_invalid() {
        assert!(SharedItemKind::from_str("unknown").is_err());
        assert!(SharedItemKind::from_str("").is_err());
        assert!(SharedItemKind::from_str("File").is_err());
    }

    #[test]
    fn shared_item_kind_serde_roundtrip() {
        let kind = SharedItemKind::File;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"file\"");
        let parsed: SharedItemKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn project_serde_roundtrip() {
        let project = Project {
            id: 1,
            name: "test".to_string(),
            path: "/tmp/test".to_string(),
            created_at: "2024-01-01".to_string(),
        };
        let json = serde_json::to_string(&project).unwrap();
        let parsed: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.name, "test");
    }

    #[test]
    fn shared_item_serde_with_optional_fields() {
        let item = SharedItem {
            id: 1,
            kind: SharedItemKind::Note,
            path: None,
            content: Some("hello".to_string()),
            label: Some("lbl".to_string()),
            project_id: Some(1),
            group_id: None,
            created_by_project_id: None,
            created_at: "2024-01-01".to_string(),
            updated_at: "2024-01-01".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let parsed: SharedItem = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, SharedItemKind::Note);
        assert!(parsed.path.is_none());
        assert_eq!(parsed.content.as_deref(), Some("hello"));
    }

    // --- WorkspaceConfig tests ---

    #[test]
    fn share_entry_path_only_serde() {
        let entry = ShareEntry::PathOnly("src/main.rs".to_string());
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(json, "\"src/main.rs\"");
        let parsed: ShareEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path(), "src/main.rs");
        assert!(parsed.label().is_none());
    }

    #[test]
    fn share_entry_with_label_serde() {
        let entry = ShareEntry::WithLabel {
            path: "config.yml".to_string(),
            label: "deploy config".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ShareEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path(), "config.yml");
        assert_eq!(parsed.label(), Some("deploy config"));
    }

    #[test]
    fn workspace_config_serde_roundtrip() {
        let config = WorkspaceConfig {
            name: "my-project".to_string(),
            groups: vec!["team-a".to_string()],
            share: vec![
                ShareEntry::PathOnly("README.md".to_string()),
                ShareEntry::WithLabel {
                    path: "api.json".to_string(),
                    label: "API spec".to_string(),
                },
            ],
            notes: vec![NoteEntry {
                content: "Important note".to_string(),
                label: Some("info".to_string()),
            }],
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: WorkspaceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn workspace_config_empty_fields_omitted() {
        let config = WorkspaceConfig {
            name: "minimal".to_string(),
            groups: vec![],
            share: vec![],
            notes: vec![],
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("groups"));
        assert!(!json.contains("share"));
        assert!(!json.contains("notes"));
    }

    #[test]
    fn workspace_config_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ai-workspace.json");

        let config = WorkspaceConfig {
            name: "test-proj".to_string(),
            groups: vec!["grp".to_string()],
            share: vec![ShareEntry::PathOnly("file.txt".to_string())],
            notes: vec![NoteEntry {
                content: "hello".to_string(),
                label: None,
            }],
        };

        config.save(&path).unwrap();

        // File should end with newline
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'));

        let loaded = WorkspaceConfig::load(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn workspace_config_load_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ai-workspace.json");
        std::fs::write(&path, "{ not valid json }").unwrap();

        let result = WorkspaceConfig::load(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Malformed JSON"));
    }

    #[test]
    fn workspace_config_load_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(WorkspaceConfig::load(&path).is_err());
    }

    #[test]
    fn note_entry_label_omitted_when_none() {
        let entry = NoteEntry {
            content: "hello".to_string(),
            label: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("label"));
    }
}
