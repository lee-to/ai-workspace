use crate::models::{
    ArtifactDependencyKind, ArtifactReaction, EventSeverity, EventStatus, EventTargetRelationKind,
    EventTargetStatus, ServiceLinkKind, SharedItemKind, WorkspaceEventKind,
};
use crate::path::normalize_portable_rel_path;
use crate::walk::{self, WalkOptions};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const CLOUD_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const MAX_CLOUD_DOCUMENTS: usize = 1_000;
pub const MAX_CLOUD_DOCUMENT_BYTES: usize = 1_024 * 1_024;
pub const MAX_CLOUD_SNAPSHOT_BYTES: usize = 16 * 1_024 * 1_024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudProjectSnapshot {
    pub schema_version: u32,
    pub project: CloudProject,
    #[serde(default)]
    pub groups: Vec<CloudGroup>,
    #[serde(default)]
    pub shares: Vec<CloudShare>,
    #[serde(default)]
    pub documents: Vec<CloudDocument>,
    #[serde(default)]
    pub notes: Vec<CloudNote>,
    #[serde(default)]
    pub service_links: Vec<CloudServiceLink>,
    #[serde(default)]
    pub dependencies: Vec<CloudArtifactDependency>,
    #[serde(default)]
    pub events: Vec<CloudEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudProject {
    pub cloud_key: String,
    pub name: String,
    pub slug: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudGroup {
    pub cloud_key: String,
    pub name: String,
    pub slug: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudShare {
    pub cloud_key: String,
    pub project_slug: String,
    pub relative_path: String,
    pub kind: SharedItemKind,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudDocument {
    pub cloud_key: String,
    pub share_key: String,
    pub project_slug: String,
    pub relative_path: String,
    pub label: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudNote {
    pub cloud_key: String,
    pub project_slug: String,
    pub scope: CloudNoteScope,
    pub group_slug: Option<String>,
    pub label: Option<String>,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudNoteScope {
    Project,
    Group,
}

impl CloudNoteScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Group => "group",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudServiceLink {
    pub cloud_key: String,
    pub from_project_slug: String,
    pub to_project_slug: String,
    pub kind: ServiceLinkKind,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudArtifactDependency {
    pub cloud_key: String,
    pub project_slug: String,
    pub share_path: String,
    pub target_project_slug: String,
    pub kind: ArtifactDependencyKind,
    pub reaction: ArtifactReaction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudEvent {
    pub cloud_key: String,
    pub source_project_slug: String,
    pub source_project_name: String,
    #[serde(default)]
    pub group_slugs: Vec<String>,
    pub kind: WorkspaceEventKind,
    pub title: String,
    pub body: Option<String>,
    pub severity: EventSeverity,
    pub status: EventStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub targets: Vec<CloudEventTarget>,
    #[serde(default)]
    pub artifacts: Vec<CloudEventArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudEventTarget {
    pub cloud_key: String,
    pub project_slug: Option<String>,
    pub relation_kind: EventTargetRelationKind,
    pub status: EventTargetStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudEventArtifact {
    pub cloud_key: String,
    pub project_slug: Option<String>,
    pub relative_path: String,
    pub reaction: ArtifactReaction,
    pub reason: String,
    pub status: crate::models::EventArtifactStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPushRequest {
    pub base_revision: Option<i64>,
    pub force: bool,
    pub snapshot_hash: String,
    pub snapshot: CloudProjectSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPushResponse {
    pub revision: i64,
    pub snapshot_hash: String,
    pub counts: CloudSnapshotCounts,
    pub no_op: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPushError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<i64>,
    pub current_snapshot_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudSnapshotCounts {
    pub groups: usize,
    pub shares: usize,
    pub documents: usize,
    pub notes: usize,
    pub service_links: usize,
    pub dependencies: usize,
    pub events: usize,
}

impl CloudProjectSnapshot {
    pub fn counts(&self) -> CloudSnapshotCounts {
        CloudSnapshotCounts {
            groups: self.groups.len(),
            shares: self.shares.len(),
            documents: self.documents.len(),
            notes: self.notes.len(),
            service_links: self.service_links.len(),
            dependencies: self.dependencies.len(),
            events: self.events.len(),
        }
    }

    pub fn validate(&self) -> Result<usize> {
        if self.schema_version != CLOUD_SNAPSHOT_SCHEMA_VERSION {
            bail!(
                "Unsupported cloud snapshot schema version: {}",
                self.schema_version
            );
        }
        validate_slug(&self.project.slug)?;
        validate_exact_key(&self.project.cloud_key, keys::project(&self.project.slug)?)?;
        if self.project.name.trim().is_empty() {
            bail!("Project name must not be empty");
        }
        if self.documents.len() > MAX_CLOUD_DOCUMENTS {
            bail!("Cloud snapshot exceeds {MAX_CLOUD_DOCUMENTS} documents");
        }

        let mut keys = HashSet::new();
        insert_key(&mut keys, &self.project.cloud_key)?;
        let mut group_slugs = HashSet::new();
        for group in &self.groups {
            validate_slug(&group.slug)?;
            validate_exact_key(&group.cloud_key, keys::group(&group.slug)?)?;
            if !group_slugs.insert(group.slug.as_str()) {
                bail!("Duplicate cloud group slug: {}", group.slug);
            }
            insert_key(&mut keys, &group.cloud_key)?;
        }
        let mut shares_by_key = HashMap::new();
        for share in &self.shares {
            validate_slug(&share.project_slug)?;
            require_project_slug(&share.project_slug, &self.project.slug)?;
            validate_relative_path(&share.relative_path)?;
            if share.kind == SharedItemKind::Note {
                bail!("Cloud shares must be files or directories");
            }
            validate_exact_key(
                &share.cloud_key,
                keys::share(&share.project_slug, &share.relative_path)?,
            )?;
            shares_by_key.insert(share.cloud_key.as_str(), share);
            insert_key(&mut keys, &share.cloud_key)?;
        }
        for document in &self.documents {
            validate_slug(&document.project_slug)?;
            require_project_slug(&document.project_slug, &self.project.slug)?;
            validate_relative_path(&document.relative_path)?;
            if std::path::Path::new(&document.relative_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("md"))
            {
                bail!("Cloud documents must be Markdown files");
            }
            validate_exact_key(
                &document.cloud_key,
                keys::file(&document.project_slug, &document.relative_path)?,
            )?;
            let share = shares_by_key
                .get(document.share_key.as_str())
                .ok_or_else(|| anyhow::anyhow!("Cloud document references an unknown share key"))?;
            let belongs_to_share = match share.kind {
                SharedItemKind::File => document.relative_path == share.relative_path,
                SharedItemKind::Dir => document
                    .relative_path
                    .strip_prefix(&share.relative_path)
                    .is_some_and(|suffix| suffix.starts_with('/')),
                SharedItemKind::Note => false,
            };
            if !belongs_to_share {
                bail!("Cloud document path does not belong to its share");
            }
            if document.content.len() > MAX_CLOUD_DOCUMENT_BYTES {
                bail!("Cloud document exceeds {MAX_CLOUD_DOCUMENT_BYTES} bytes");
            }
            insert_key(&mut keys, &document.cloud_key)?;
        }
        let mut note_ordinals = HashMap::new();
        for note in &self.notes {
            validate_slug(&note.project_slug)?;
            require_project_slug(&note.project_slug, &self.project.slug)?;
            match (note.scope, note.group_slug.as_deref()) {
                (CloudNoteScope::Project, None) => {}
                (CloudNoteScope::Group, Some(slug)) => {
                    validate_slug(slug)?;
                    if !group_slugs.contains(slug) {
                        bail!("Cloud note references an unknown group slug");
                    }
                }
                _ => bail!("Note group_slug must be set only for group notes"),
            }
            let fingerprint = super::snapshot::sha256_hex(&note_fingerprint_input(
                &note.project_slug,
                note.scope,
                note.group_slug.as_deref(),
                note.label.as_deref(),
                &note.content,
            )?);
            let ordinal = note_ordinals.entry(fingerprint.clone()).or_insert(0usize);
            validate_exact_key(
                &note.cloud_key,
                keys::note(&note.project_slug, note.scope, &fingerprint, *ordinal)?,
            )?;
            *ordinal += 1;
            if note.content.len() > MAX_CLOUD_DOCUMENT_BYTES {
                bail!("Cloud note exceeds {MAX_CLOUD_DOCUMENT_BYTES} bytes");
            }
            insert_key(&mut keys, &note.cloud_key)?;
        }
        for link in &self.service_links {
            validate_slug(&link.from_project_slug)?;
            validate_slug(&link.to_project_slug)?;
            require_project_slug(&link.from_project_slug, &self.project.slug)?;
            validate_exact_key(
                &link.cloud_key,
                keys::service_link(
                    &link.from_project_slug,
                    &link.to_project_slug,
                    link.kind.as_str(),
                )?,
            )?;
            insert_key(&mut keys, &link.cloud_key)?;
        }
        for dependency in &self.dependencies {
            validate_slug(&dependency.project_slug)?;
            validate_slug(&dependency.target_project_slug)?;
            require_project_slug(&dependency.project_slug, &self.project.slug)?;
            validate_relative_path(&dependency.share_path)?;
            validate_exact_key(
                &dependency.cloud_key,
                keys::dependency(
                    &dependency.project_slug,
                    &dependency.share_path,
                    &dependency.target_project_slug,
                    dependency.kind.as_str(),
                )?,
            )?;
            if !shares_by_key.contains_key(
                keys::share(&dependency.project_slug, &dependency.share_path)?.as_str(),
            ) {
                bail!("Cloud dependency references an unknown share");
            }
            insert_key(&mut keys, &dependency.cloud_key)?;
        }
        let mut event_ordinals = HashMap::new();
        for event in &self.events {
            validate_slug(&event.source_project_slug)?;
            require_project_slug(&event.source_project_slug, &self.project.slug)?;
            let fingerprint = super::snapshot::sha256_hex(&event_fingerprint_input(event)?);
            let ordinal = event_ordinals.entry(fingerprint.clone()).or_insert(0usize);
            validate_exact_key(
                &event.cloud_key,
                keys::event(&event.source_project_slug, &fingerprint, *ordinal)?,
            )?;
            *ordinal += 1;
            insert_key(&mut keys, &event.cloud_key)?;
            for slug in &event.group_slugs {
                validate_slug(slug)?;
                if !group_slugs.contains(slug.as_str()) {
                    bail!("Cloud event references an unknown group slug");
                }
            }
            for (index, target) in event.targets.iter().enumerate() {
                if let Some(slug) = &target.project_slug {
                    validate_slug(slug)?;
                }
                validate_exact_key(
                    &target.cloud_key,
                    keys::event_target(
                        &event.cloud_key,
                        target.project_slug.as_deref(),
                        target.relation_kind.as_str(),
                        index,
                    )?,
                )?;
                insert_key(&mut keys, &target.cloud_key)?;
            }
            for (index, artifact) in event.artifacts.iter().enumerate() {
                if let Some(slug) = &artifact.project_slug {
                    validate_slug(slug)?;
                }
                validate_relative_path(&artifact.relative_path)?;
                validate_exact_key(
                    &artifact.cloud_key,
                    keys::event_artifact(
                        &event.cloud_key,
                        artifact.project_slug.as_deref(),
                        &artifact.relative_path,
                        index,
                    )?,
                )?;
                insert_key(&mut keys, &artifact.cloud_key)?;
            }
        }

        let bytes = serde_json::to_vec(self)?.len();
        if bytes > MAX_CLOUD_SNAPSHOT_BYTES {
            bail!("Cloud snapshot exceeds {MAX_CLOUD_SNAPSHOT_BYTES} bytes");
        }
        Ok(bytes)
    }
}

pub fn validate_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && slug.len() <= 100
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        bail!("Invalid cloud slug: {slug}");
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<()> {
    if normalize_portable_rel_path(path)? != path {
        bail!("Cloud path is not normalized: {path}");
    }
    if !walk::path_allowed_by_options(std::path::Path::new(path), WalkOptions::default()) {
        bail!("Cloud path is blocked by synchronization policy");
    }
    Ok(())
}

fn validate_exact_key(actual: &str, expected: String) -> Result<()> {
    if actual != expected {
        bail!("Cloud key does not match its record coordinates");
    }
    Ok(())
}

fn require_project_slug(actual: &str, expected: &str) -> Result<()> {
    if actual != expected {
        bail!("Cloud record project slug does not match snapshot project");
    }
    Ok(())
}

fn insert_key(keys: &mut HashSet<String>, key: &str) -> Result<()> {
    if !keys.insert(key.to_owned()) {
        bail!("Duplicate cloud key: {key}");
    }
    Ok(())
}

pub mod keys {
    use super::{CloudNoteScope, validate_relative_path, validate_slug};
    use anyhow::{Result, bail};

    pub fn project(project_slug: &str) -> Result<String> {
        validate_slug(project_slug)?;
        Ok(format!("project:{project_slug}"))
    }

    pub fn group(group_slug: &str) -> Result<String> {
        validate_slug(group_slug)?;
        Ok(format!("group:{group_slug}"))
    }

    pub fn share(project_slug: &str, relative_path: &str) -> Result<String> {
        validate_slug(project_slug)?;
        validate_relative_path(relative_path)?;
        Ok(format!("share:{project_slug}:{relative_path}"))
    }

    pub fn file(project_slug: &str, relative_path: &str) -> Result<String> {
        validate_slug(project_slug)?;
        validate_relative_path(relative_path)?;
        Ok(format!("file:{project_slug}:{relative_path}"))
    }

    pub fn note(
        project_slug: &str,
        scope: CloudNoteScope,
        fingerprint: &str,
        ordinal: usize,
    ) -> Result<String> {
        validate_slug(project_slug)?;
        validate_fingerprint(fingerprint)?;
        Ok(format!(
            "note:{project_slug}:{}:{fingerprint}:{ordinal}",
            scope.as_str()
        ))
    }

    pub fn service_link(from_slug: &str, to_slug: &str, kind: &str) -> Result<String> {
        validate_slug(from_slug)?;
        validate_slug(to_slug)?;
        Ok(format!("link:{from_slug}:{to_slug}:{kind}"))
    }

    pub fn dependency(
        project_slug: &str,
        share_path: &str,
        target_slug: &str,
        kind: &str,
    ) -> Result<String> {
        validate_slug(project_slug)?;
        validate_slug(target_slug)?;
        validate_relative_path(share_path)?;
        Ok(format!(
            "dependency:{project_slug}:{share_path}:{target_slug}:{kind}"
        ))
    }

    pub fn event(source_slug: &str, fingerprint: &str, ordinal: usize) -> Result<String> {
        validate_slug(source_slug)?;
        validate_fingerprint(fingerprint)?;
        Ok(format!("event:{source_slug}:{fingerprint}:{ordinal}"))
    }

    pub fn event_target(
        event_key: &str,
        project_slug: Option<&str>,
        relation_kind: &str,
        ordinal: usize,
    ) -> Result<String> {
        if !event_key.starts_with("event:") {
            bail!("Invalid event key");
        }
        let project_slug = project_slug.unwrap_or("_deleted");
        if project_slug != "_deleted" {
            validate_slug(project_slug)?;
        }
        Ok(format!(
            "event-target:{event_key}:{project_slug}:{relation_kind}:{ordinal}"
        ))
    }

    pub fn event_artifact(
        event_key: &str,
        project_slug: Option<&str>,
        relative_path: &str,
        ordinal: usize,
    ) -> Result<String> {
        if !event_key.starts_with("event:") {
            bail!("Invalid event key");
        }
        let project_slug = project_slug.unwrap_or("_deleted");
        if project_slug != "_deleted" {
            validate_slug(project_slug)?;
        }
        validate_relative_path(relative_path)?;
        Ok(format!(
            "event-artifact:{event_key}:{project_slug}:{relative_path}:{ordinal}"
        ))
    }

    pub fn validate_fingerprint(fingerprint: &str) -> Result<()> {
        if fingerprint.len() != 64
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("Fingerprint must be a 64-character hexadecimal SHA-256 digest");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct NoteFingerprintInput<'a> {
    project_slug: &'a str,
    scope: CloudNoteScope,
    group_slug: Option<&'a str>,
    label: Option<&'a str>,
    content: &'a str,
}

pub fn note_fingerprint_input(
    project_slug: &str,
    scope: CloudNoteScope,
    group_slug: Option<&str>,
    label: Option<&str>,
    content: &str,
) -> Result<Vec<u8>> {
    validate_slug(project_slug)?;
    match (scope, group_slug) {
        (CloudNoteScope::Project, None) => {}
        (CloudNoteScope::Group, Some(slug)) => validate_slug(slug)?,
        _ => bail!("Note group_slug must be set only for group notes"),
    }
    Ok(serde_json::to_vec(&NoteFingerprintInput {
        project_slug,
        scope,
        group_slug,
        label,
        content,
    })?)
}

#[derive(Serialize)]
struct EventFingerprintInput<'a> {
    source_project_slug: &'a str,
    group_slugs: &'a [String],
    kind: WorkspaceEventKind,
    title: &'a str,
    body: Option<&'a str>,
    severity: EventSeverity,
    created_at: &'a str,
    targets: Vec<(&'a Option<String>, EventTargetRelationKind)>,
    artifacts: Vec<(&'a Option<String>, &'a str, ArtifactReaction, &'a str)>,
}

pub fn event_fingerprint_input(event: &CloudEvent) -> Result<Vec<u8>> {
    validate_slug(&event.source_project_slug)?;
    let mut group_slugs = event.group_slugs.clone();
    group_slugs.sort();
    let mut targets: Vec<_> = event
        .targets
        .iter()
        .map(|target| (&target.project_slug, target.relation_kind))
        .collect();
    targets.sort_by_key(|(slug, kind)| (slug.as_deref(), kind.as_str()));
    let mut artifacts: Vec<_> = event
        .artifacts
        .iter()
        .map(|artifact| {
            (
                &artifact.project_slug,
                artifact.relative_path.as_str(),
                artifact.reaction,
                artifact.reason.as_str(),
            )
        })
        .collect();
    artifacts.sort_by_key(|(slug, path, reaction, reason)| {
        (slug.as_deref(), *path, reaction.as_str(), *reason)
    });
    Ok(serde_json::to_vec(&EventFingerprintInput {
        source_project_slug: &event.source_project_slug,
        group_slugs: &group_slugs,
        kind: event.kind,
        title: &event.title,
        body: event.body.as_deref(),
        severity: event.severity,
        created_at: &event.created_at,
        targets,
        artifacts,
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> CloudProjectSnapshot {
        CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: CloudProject {
                cloud_key: "project:api".into(),
                name: "API".into(),
                slug: "api".into(),
            },
            groups: vec![],
            shares: vec![CloudShare {
                cloud_key: "share:api:docs/readme.md".into(),
                project_slug: "api".into(),
                relative_path: "docs/readme.md".into(),
                kind: SharedItemKind::File,
                label: Some("Readme".into()),
            }],
            documents: vec![CloudDocument {
                cloud_key: "file:api:docs/readme.md".into(),
                share_key: "share:api:docs/readme.md".into(),
                project_slug: "api".into(),
                relative_path: "docs/readme.md".into(),
                label: Some("Readme".into()),
                content: "# API".into(),
            }],
            notes: vec![],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        }
    }

    #[test]
    fn wire_shape_contains_only_cloud_fields() {
        let snapshot = snapshot();
        snapshot.validate().unwrap();
        let value = serde_json::to_value(snapshot).unwrap();
        let text = value.to_string();

        assert_eq!(value["schema_version"], json!(1));
        assert_eq!(value["project"]["cloud_key"], "project:api");
        for forbidden in [
            "local_id",
            "project_id",
            "shared_item_id",
            "absolute_path",
            "mtime",
            "fts",
            "codegraph",
            "credential",
            "token",
        ] {
            assert!(
                !text.contains(forbidden),
                "wire payload contains {forbidden}"
            );
        }
    }

    #[test]
    fn validation_rejects_unsafe_paths_and_missing_keys() {
        let mut unsafe_path = snapshot();
        unsafe_path.documents[0].relative_path = "/etc/passwd".into();
        assert!(unsafe_path.validate().is_err());

        let mut sensitive_path = snapshot();
        sensitive_path.documents[0].relative_path = ".env".into();
        sensitive_path.documents[0].cloud_key = "file:api:.env".into();
        assert!(sensitive_path.validate().is_err());

        let mut missing_key = snapshot();
        missing_key.documents[0].cloud_key.clear();
        assert!(missing_key.validate().is_err());

        let mut mismatched_key = snapshot();
        mismatched_key.project.cloud_key = "project:other".into();
        assert!(mismatched_key.validate().is_err());

        let mut unrelated_share = snapshot();
        unrelated_share.shares.push(CloudShare {
            cloud_key: "share:api:other.md".into(),
            project_slug: "api".into(),
            relative_path: "other.md".into(),
            kind: SharedItemKind::File,
            label: None,
        });
        unrelated_share.documents[0].share_key = "share:api:other.md".into();
        assert!(unrelated_share.validate().is_err());
    }

    #[test]
    fn validation_enforces_document_count_limit() {
        let mut snapshot = snapshot();
        snapshot.documents = (0..=MAX_CLOUD_DOCUMENTS)
            .map(|index| CloudDocument {
                cloud_key: format!("file:api:docs/{index}.md"),
                share_key: "share:api:docs/readme.md".into(),
                project_slug: "api".into(),
                relative_path: format!("docs/{index}.md"),
                label: None,
                content: String::new(),
            })
            .collect();
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn validation_enforces_exact_snapshot_size_limit() {
        let mut snapshot = snapshot();
        let base_bytes = serde_json::to_vec(&snapshot).unwrap().len();
        snapshot.project.name = "x".repeat(MAX_CLOUD_SNAPSHOT_BYTES - base_bytes + 3);

        assert_eq!(snapshot.validate().unwrap(), MAX_CLOUD_SNAPSHOT_BYTES);
        snapshot.project.name.push('x');
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn event_key_ignores_mutable_status_by_contract() {
        let fingerprint = "a".repeat(64);
        let open = keys::event("api", &fingerprint, 0).unwrap();
        let closed = keys::event("api", &fingerprint, 0).unwrap();
        assert_eq!(open, closed);
    }

    #[test]
    fn deleted_event_coordinates_do_not_collide_with_a_real_deleted_slug() {
        let event = format!("event:api:{}:0", "a".repeat(64));
        assert_ne!(
            keys::event_target(&event, None, "affected", 0).unwrap(),
            keys::event_target(&event, Some("deleted"), "affected", 0).unwrap()
        );
        assert_ne!(
            keys::event_artifact(&event, None, "README.md", 0).unwrap(),
            keys::event_artifact(&event, Some("deleted"), "README.md", 0).unwrap()
        );
    }

    #[test]
    fn event_fingerprint_excludes_mutable_status_fields() {
        let mut event = CloudEvent {
            cloud_key:
                "event:api:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:0"
                    .into(),
            source_project_slug: "api".into(),
            source_project_name: "API".into(),
            group_slugs: vec!["backend".into()],
            kind: WorkspaceEventKind::ServiceChanged,
            title: "API changed".into(),
            body: None,
            severity: EventSeverity::Warning,
            status: EventStatus::Open,
            created_at: "2026-08-25T00:00:00Z".into(),
            updated_at: "2026-08-25T00:00:00Z".into(),
            targets: vec![],
            artifacts: vec![],
        };
        let before = event_fingerprint_input(&event).unwrap();
        event.status = EventStatus::Closed;
        event.updated_at = "2026-08-26T00:00:00Z".into();
        assert_eq!(before, event_fingerprint_input(&event).unwrap());
    }
}
