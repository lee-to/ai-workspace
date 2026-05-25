use anyhow::{Context as _, Result};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const WORKSPACE_CONFIG_VERSION_FIELD: &str = "ai_workspace_config_version";
pub const WORKSPACE_CONFIG_VERSION: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub path: String,
    pub created_at: String,
}

pub fn normalize_project_slug(input: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;

    for ch in input.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
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

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[allow(dead_code)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        #[allow(dead_code)]
        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(anyhow::anyhow!(
                        "unknown {} value: {}",
                        stringify!($name),
                        s
                    )),
                }
            }
        }
    };
}

string_enum! {
    pub enum ServiceLinkKind {
        DependsOn => "depends_on",
        RelatedTo => "related_to",
    }
}

string_enum! {
    pub enum ArtifactDependencyKind {
        References => "references",
        ConsumesApi => "consumes_api",
        Documents => "documents",
        Configures => "configures",
    }
}

string_enum! {
    pub enum ArtifactReaction {
        Inspect => "inspect",
        Update => "update",
        Delete => "delete",
        RemoveReference => "remove_reference",
    }
}

string_enum! {
    pub enum WorkspaceEventKind {
        ServiceDeleted => "service_deleted",
        ServiceChanged => "service_changed",
        ArtifactChanged => "artifact_changed",
    }
}

string_enum! {
    pub enum EventSeverity {
        Info => "info",
        Warning => "warning",
        Error => "error",
        Critical => "critical",
    }
}

string_enum! {
    pub enum EventStatus {
        Open => "open",
        Closed => "closed",
    }
}

string_enum! {
    pub enum EventTargetRelationKind {
        LinkedService => "linked_service",
        ArtifactDependency => "artifact_dependency",
    }
}

string_enum! {
    pub enum EventTargetStatus {
        Open => "open",
        Resolved => "resolved",
    }
}

string_enum! {
    pub enum EventArtifactStatus {
        Open => "open",
        Resolved => "resolved",
    }
}

string_enum! {
    pub enum CodeNodeKind {
        File => "file",
        Module => "module",
        Struct => "struct",
        Enum => "enum",
        Trait => "trait",
        Impl => "impl",
        Function => "function",
        Method => "method",
        Const => "const",
        TypeAlias => "type_alias",
        Import => "import",
    }
}

string_enum! {
    pub enum CodeEdgeKind {
        Contains => "contains",
        Calls => "calls",
        Imports => "imports",
        References => "references",
    }
}

string_enum! {
    pub enum CodeReferenceKind {
        Calls => "calls",
        Imports => "imports",
        References => "references",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ServiceLink {
    pub id: i64,
    pub from_project_id: i64,
    pub to_project_id: i64,
    pub kind: ServiceLinkKind,
    pub label: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ArtifactDependency {
    pub id: i64,
    pub shared_item_id: i64,
    pub depends_on_project_id: Option<i64>,
    pub depends_on_project_slug_snapshot: String,
    pub kind: ArtifactDependencyKind,
    pub reaction: ArtifactReaction,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct WorkspaceEvent {
    pub id: i64,
    pub source_project_id: Option<i64>,
    pub source_project_slug: String,
    pub source_project_name: String,
    pub group_id: Option<i64>,
    pub kind: WorkspaceEventKind,
    pub title: String,
    pub body: Option<String>,
    pub severity: EventSeverity,
    pub status: EventStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct EventTarget {
    pub id: i64,
    pub event_id: i64,
    pub affected_project_id: Option<i64>,
    pub relation_kind: EventTargetRelationKind,
    pub status: EventTargetStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct EventArtifact {
    pub id: i64,
    pub event_id: i64,
    pub affected_project_id: Option<i64>,
    pub shared_item_id: Option<i64>,
    pub path_snapshot: String,
    pub reaction: ArtifactReaction,
    pub reason: String,
    pub status: EventArtifactStatus,
    pub created_at: String,
    pub updated_at: String,
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

/// A single hit from FTS5 search over indexed files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchHit {
    pub shared_item_id: i64,
    pub project_id: i64,
    pub path: String,
    pub snippet: String,
    /// bm25 score — lower is better (SQLite FTS5 convention).
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeFile {
    pub project_id: i64,
    pub path: String,
    pub language: String,
    pub content_hash: String,
    pub size: i64,
    pub mtime: i64,
    pub indexed_at: String,
    pub node_count: i64,
    pub errors_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeNode {
    pub stable_id: String,
    pub project_id: i64,
    pub kind: CodeNodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: String,
    pub start_line: i64,
    pub start_column: i64,
    pub end_line: i64,
    pub end_column: i64,
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    pub flags_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdge {
    pub id: Option<i64>,
    pub project_id: i64,
    pub source_node_id: String,
    pub target_node_id: String,
    pub kind: CodeEdgeKind,
    pub line: Option<i64>,
    pub column: Option<i64>,
    pub metadata_json: Option<String>,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeUnresolvedRef {
    pub id: Option<i64>,
    pub project_id: i64,
    pub source_node_id: String,
    pub file_path: String,
    pub ref_name: String,
    pub kind: CodeReferenceKind,
    pub line: i64,
    pub column: i64,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodeGraphStats {
    pub project_id: i64,
    pub file_count: i64,
    pub node_count: i64,
    pub edge_count: i64,
    pub unresolved_ref_count: i64,
    pub last_indexed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphSearchHit {
    pub node: CodeNode,
    pub rank: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct CodeNodeSearch {
    pub query: Option<String>,
    pub kind: Option<CodeNodeKind>,
    pub language: Option<String>,
    pub file_path: Option<String>,
    pub limit: usize,
}

// --- Workspace Config (for .ai-workspace.json) ---

/// An artifact dependency entry in the config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEntry {
    pub service: String,
    pub kind: ArtifactDependencyKind,
    pub reaction: ArtifactReaction,
}

fn option_vec_is_none<T>(value: &Option<Vec<T>>) -> bool {
    value.is_none()
}

/// A shared item entry in the config file.
/// String form = legacy path only. Object form can include label, kind, and dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShareEntry {
    /// Just a path string, no label
    PathOnly(String),
    /// Object with path and optional metadata
    WithMetadata {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<SharedItemKind>,
        #[serde(default, skip_serializing_if = "option_vec_is_none")]
        dependencies: Option<Vec<DependencyEntry>>,
    },
}

impl ShareEntry {
    pub fn path(&self) -> &str {
        match self {
            ShareEntry::PathOnly(p) => p,
            ShareEntry::WithMetadata { path, .. } => path,
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            ShareEntry::PathOnly(_) => None,
            ShareEntry::WithMetadata { label, .. } => label.as_deref(),
        }
    }

    pub fn kind(&self) -> Option<SharedItemKind> {
        match self {
            ShareEntry::PathOnly(_) => None,
            ShareEntry::WithMetadata { kind, .. } => *kind,
        }
    }

    pub fn dependencies(&self) -> Option<&[DependencyEntry]> {
        match self {
            ShareEntry::PathOnly(_) => None,
            ShareEntry::WithMetadata { dependencies, .. } => dependencies.as_deref(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
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
    pub dependencies_added: usize,
    pub dependencies_removed: usize,
    pub dependencies_updated: usize,
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
        let mut value =
            serde_json::to_value(self).context("Failed to serialize workspace config")?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                WORKSPACE_CONFIG_VERSION_FIELD.to_string(),
                serde_json::json!(WORKSPACE_CONFIG_VERSION),
            );
        }
        let json =
            serde_json::to_string_pretty(&value).context("Failed to serialize workspace config")?;
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
            slug: "test".to_string(),
            path: "/tmp/test".to_string(),
            created_at: "2024-01-01".to_string(),
        };
        let json = serde_json::to_string(&project).unwrap();
        let parsed: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 1);
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.slug, "test");
    }

    #[test]
    fn normalize_project_slug_handles_names() {
        assert_eq!(normalize_project_slug("Auth Service"), "auth-service");
        assert_eq!(normalize_project_slug("  Billing_API!! "), "billing-api");
        assert_eq!(normalize_project_slug("!!!"), "project");
    }

    #[test]
    fn service_event_enums_roundtrip() {
        assert_eq!(ServiceLinkKind::DependsOn.as_str(), "depends_on");
        assert_eq!(
            ServiceLinkKind::from_str("depends_on").unwrap(),
            ServiceLinkKind::DependsOn
        );
        assert_eq!(ArtifactDependencyKind::ConsumesApi.as_str(), "consumes_api");
        assert_eq!(
            ArtifactReaction::RemoveReference.as_str(),
            "remove_reference"
        );
        assert_eq!(
            WorkspaceEventKind::ServiceDeleted.as_str(),
            "service_deleted"
        );
        assert_eq!(EventSeverity::Warning.as_str(), "warning");
        assert_eq!(EventStatus::Closed.as_str(), "closed");
        assert_eq!(
            EventTargetRelationKind::ArtifactDependency.as_str(),
            "artifact_dependency"
        );
        assert_eq!(EventTargetStatus::Resolved.as_str(), "resolved");
        assert_eq!(EventArtifactStatus::Open.as_str(), "open");
        assert!(WorkspaceEventKind::from_str("unknown").is_err());
    }

    #[test]
    fn service_link_serde_uses_snake_case_kind() {
        let link = ServiceLink {
            id: 1,
            from_project_id: 10,
            to_project_id: 20,
            kind: ServiceLinkKind::DependsOn,
            label: Some("runtime dependency".to_string()),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
        };
        let json = serde_json::to_string(&link).unwrap();
        assert!(json.contains("\"kind\":\"depends_on\""));
        let parsed: ServiceLink = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, ServiceLinkKind::DependsOn);
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
        let entry = ShareEntry::WithMetadata {
            path: "config.yml".to_string(),
            label: Some("deploy config".to_string()),
            kind: None,
            dependencies: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ShareEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path(), "config.yml");
        assert_eq!(parsed.label(), Some("deploy config"));
    }

    #[test]
    fn share_entry_with_kind_and_dependencies_serde() {
        let entry = ShareEntry::WithMetadata {
            path: "docs/auth.md".to_string(),
            label: Some("auth docs".to_string()),
            kind: Some(SharedItemKind::File),
            dependencies: Some(vec![DependencyEntry {
                service: "auth".to_string(),
                kind: ArtifactDependencyKind::References,
                reaction: ArtifactReaction::Update,
            }]),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"kind\":\"file\""));
        assert!(json.contains("\"service\":\"auth\""));
        let parsed: ShareEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path(), "docs/auth.md");
        assert_eq!(parsed.kind(), Some(SharedItemKind::File));
        assert_eq!(parsed.dependencies().unwrap()[0].service, "auth");
    }

    #[test]
    fn workspace_config_serde_roundtrip() {
        let config = WorkspaceConfig {
            name: "my-project".to_string(),
            slug: Some("my-project".to_string()),
            groups: vec!["team-a".to_string()],
            share: vec![
                ShareEntry::PathOnly("README.md".to_string()),
                ShareEntry::WithMetadata {
                    path: "api.json".to_string(),
                    label: Some("API spec".to_string()),
                    kind: Some(SharedItemKind::File),
                    dependencies: None,
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
            slug: None,
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
            slug: None,
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
