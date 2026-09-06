use super::models::{
    CLOUD_SNAPSHOT_SCHEMA_VERSION, CloudArtifactDependency, CloudDocument, CloudEvent,
    CloudEventArtifact, CloudEventTarget, CloudGroup, CloudNote, CloudNoteScope, CloudProject,
    CloudProjectSnapshot, CloudServiceLink, CloudShare, event_fingerprint_input, keys,
    note_fingerprint_input,
};
use crate::db::{Db, validate_project_rel_path};
use crate::indexer::MAX_INDEX_FILE_SIZE;
use crate::models::{Project, SharedItem, SharedItemKind, normalize_project_slug};
use crate::path::normalize_portable_rel_path;
use crate::walk::{self, WalkOptions};
use anyhow::{Context as _, Result};
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

pub struct BuiltCloudSnapshot {
    pub snapshot: CloudProjectSnapshot,
    pub snapshot_hash: String,
    pub serialized_bytes: usize,
}

#[derive(Default)]
struct DocumentSkips {
    unsafe_path: usize,
    missing: usize,
    non_markdown: usize,
    oversized: usize,
    non_utf8: usize,
}

pub fn build_project_snapshot(
    db: &Db,
    project: &Project,
    include_markdown: bool,
) -> Result<BuiltCloudSnapshot> {
    let started = Instant::now();
    info!(
        "cloud snapshot start project_slug='{}' include_markdown={}",
        project.slug, include_markdown
    );

    let local_items = db.get_shared_items_owned_by_project(project.id)?;
    let mut groups = db.get_groups_for_project(project.id)?;
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    let group_slugs: HashMap<_, _> = groups
        .iter()
        .map(|group| (group.id, normalize_project_slug(&group.name)))
        .collect();
    let cloud_groups = groups
        .into_iter()
        .map(|group| {
            let slug = group_slugs[&group.id].clone();
            Ok(CloudGroup {
                cloud_key: keys::group(&slug)?,
                name: group.name,
                slug,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut share_items = local_items
        .iter()
        .filter(|item| matches!(item.kind, SharedItemKind::File | SharedItemKind::Dir))
        .filter_map(|item| safe_share(item, &project.slug).transpose())
        .collect::<Result<Vec<_>>>()?;
    share_items.sort_by(|a, b| a.1.relative_path.cmp(&b.1.relative_path));
    let shares = share_items
        .iter()
        .map(|(_, share)| share.clone())
        .collect::<Vec<_>>();

    let (documents, skips) = if include_markdown {
        collect_documents(Path::new(&project.path), &project.slug, &share_items)?
    } else {
        (Vec::new(), DocumentSkips::default())
    };
    debug!(
        "cloud snapshot project_slug='{}' markdown_skips unsafe={} missing={} non_markdown={} oversized={} non_utf8={}",
        project.slug,
        skips.unsafe_path,
        skips.missing,
        skips.non_markdown,
        skips.oversized,
        skips.non_utf8
    );

    let snapshot = CloudProjectSnapshot {
        schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
        project: CloudProject {
            cloud_key: keys::project(&project.slug)?,
            name: project.name.clone(),
            slug: project.slug.clone(),
        },
        groups: cloud_groups,
        shares,
        documents,
        notes: build_notes(&project.slug, &local_items, &group_slugs)?,
        service_links: build_service_links(db, project)?,
        dependencies: build_dependencies(db, project, &local_items)?,
        events: build_events(db, project, &group_slugs)?,
    };
    let serialized_bytes = snapshot.validate().map_err(|error| {
        warn!(
            "[FIX:cloud-validation] cloud snapshot rejected project_slug='{}' categories={:?}: {}",
            project.slug,
            snapshot.counts(),
            error
        );
        error
    })?;
    let snapshot_hash = sha256_hex(&serde_json::to_vec(&snapshot)?);
    let counts = snapshot.counts();
    info!(
        "cloud snapshot complete project_slug='{}' groups={} shares={} documents={} notes={} links={} dependencies={} events={} bytes={} duration_ms={}",
        project.slug,
        counts.groups,
        counts.shares,
        counts.documents,
        counts.notes,
        counts.service_links,
        counts.dependencies,
        counts.events,
        serialized_bytes,
        started.elapsed().as_millis()
    );
    Ok(BuiltCloudSnapshot {
        snapshot,
        snapshot_hash,
        serialized_bytes,
    })
}

fn safe_share(item: &SharedItem, project_slug: &str) -> Result<Option<(SharedItem, CloudShare)>> {
    let Some(path) = item.path.as_deref() else {
        return Ok(None);
    };
    let normalized = match normalize_portable_rel_path(path) {
        Ok(path) => path,
        Err(error) => {
            warn!("cloud snapshot skipping invalid shared path: {error}");
            return Ok(None);
        }
    };
    if !walk::path_allowed_by_options(Path::new(&normalized), WalkOptions::default()) {
        warn!("cloud snapshot skipping shared path blocked by cloud policy");
        return Ok(None);
    }
    Ok(Some((
        item.clone(),
        CloudShare {
            cloud_key: keys::share(project_slug, &normalized)?,
            project_slug: project_slug.to_owned(),
            relative_path: normalized,
            kind: item.kind,
            label: item.label.clone(),
        },
    )))
}

fn collect_documents(
    project_root: &Path,
    project_slug: &str,
    shares: &[(SharedItem, CloudShare)],
) -> Result<(Vec<CloudDocument>, DocumentSkips)> {
    let canonical_root = project_root.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize project root: {}",
            project_root.display()
        )
    })?;
    let mut documents = BTreeMap::new();
    let mut skips = DocumentSkips::default();

    for (item, share) in shares {
        match item.kind {
            SharedItemKind::File => collect_document(
                &canonical_root,
                project_slug,
                &share.cloud_key,
                &share.relative_path,
                share.label.as_deref(),
                None,
                &mut documents,
                &mut skips,
            )?,
            SharedItemKind::Dir => {
                let validated =
                    match validate_project_rel_path(&canonical_root, &share.relative_path) {
                        Ok(path) if path.canonical_path.is_dir() => path,
                        _ => {
                            skips.missing += 1;
                            warn!(
                                "cloud snapshot skipping missing shared directory '{}'",
                                share.relative_path
                            );
                            continue;
                        }
                    };
                for entry in walk::walk_project_tree(
                    &canonical_root,
                    Some(&validated.rel_path),
                    None,
                    WalkOptions::default(),
                ) {
                    if !entry.is_dir {
                        collect_document(
                            &canonical_root,
                            project_slug,
                            &share.cloud_key,
                            &entry.path,
                            share.label.as_deref(),
                            Some(&validated.canonical_path),
                            &mut documents,
                            &mut skips,
                        )?;
                    }
                }
            }
            SharedItemKind::Note => unreachable!("notes are not share candidates"),
        }
    }
    Ok((documents.into_values().collect(), skips))
}

#[allow(clippy::too_many_arguments)]
fn collect_document(
    project_root: &Path,
    project_slug: &str,
    share_key: &str,
    relative_path: &str,
    label: Option<&str>,
    shared_dir: Option<&Path>,
    documents: &mut BTreeMap<String, CloudDocument>,
    skips: &mut DocumentSkips,
) -> Result<()> {
    if Path::new(relative_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("md"))
    {
        skips.non_markdown += 1;
        return Ok(());
    }
    if !walk::path_allowed_by_options(Path::new(relative_path), WalkOptions::default()) {
        skips.unsafe_path += 1;
        warn!("cloud snapshot skipping Markdown path blocked by cloud policy");
        return Ok(());
    }
    let validated = match validate_project_rel_path(project_root, relative_path) {
        Ok(path) => path,
        Err(_) => {
            skips.unsafe_path += 1;
            warn!("cloud snapshot skipping Markdown path outside project root");
            return Ok(());
        }
    };
    if shared_dir.is_some_and(|directory| !validated.canonical_path.starts_with(directory)) {
        skips.unsafe_path += 1;
        warn!("cloud snapshot skipping Markdown path outside shared directory");
        return Ok(());
    }
    let metadata = match std::fs::metadata(&validated.canonical_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            skips.missing += 1;
            warn!(
                "cloud snapshot skipping missing Markdown '{}'",
                validated.rel_path
            );
            return Ok(());
        }
    };
    if metadata.len() > MAX_INDEX_FILE_SIZE {
        skips.oversized += 1;
        warn!(
            "cloud snapshot skipping oversized Markdown '{}'",
            validated.rel_path
        );
        return Ok(());
    }
    let bytes = std::fs::read(&validated.canonical_path)
        .with_context(|| format!("Failed to read shared Markdown '{}'", validated.rel_path))?;
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            skips.non_utf8 += 1;
            warn!(
                "cloud snapshot skipping non-UTF-8 Markdown '{}'",
                validated.rel_path
            );
            return Ok(());
        }
    };
    documents
        .entry(validated.rel_path.clone())
        .or_insert(CloudDocument {
            cloud_key: keys::file(project_slug, &validated.rel_path)?,
            share_key: share_key.to_owned(),
            project_slug: project_slug.to_owned(),
            relative_path: validated.rel_path,
            label: label.map(str::to_owned),
            content,
        });
    Ok(())
}

fn build_notes(
    project_slug: &str,
    items: &[SharedItem],
    group_slugs: &HashMap<i64, String>,
) -> Result<Vec<CloudNote>> {
    let mut candidates = Vec::new();
    for item in items
        .iter()
        .filter(|item| item.kind == SharedItemKind::Note)
    {
        let Some(content) = item.content.as_deref() else {
            continue;
        };
        let (scope, group_slug) = match item.group_id {
            Some(group_id) => (CloudNoteScope::Group, group_slugs.get(&group_id).cloned()),
            None => (CloudNoteScope::Project, None),
        };
        if scope == CloudNoteScope::Group && group_slug.is_none() {
            warn!("cloud snapshot skipping group note outside project membership");
            continue;
        }
        let input = note_fingerprint_input(
            project_slug,
            scope,
            group_slug.as_deref(),
            item.label.as_deref(),
            content,
        )?;
        candidates.push((
            sha256_hex(&input),
            scope,
            group_slug,
            item.label.clone(),
            content.to_owned(),
        ));
    }
    candidates.sort();
    let mut ordinals = HashMap::new();
    candidates
        .into_iter()
        .map(|(fingerprint, scope, group_slug, label, content)| {
            let ordinal = ordinals.entry(fingerprint.clone()).or_insert(0usize);
            let note = CloudNote {
                cloud_key: keys::note(project_slug, scope, &fingerprint, *ordinal)?,
                project_slug: project_slug.to_owned(),
                scope,
                group_slug,
                label,
                content,
            };
            *ordinal += 1;
            Ok(note)
        })
        .collect()
}

fn build_service_links(db: &Db, project: &Project) -> Result<Vec<CloudServiceLink>> {
    let mut links = Vec::new();
    for link in db.list_outgoing_service_links(project.id)? {
        let Some(target) = db.get_project_by_id(link.to_project_id)? else {
            continue;
        };
        links.push(CloudServiceLink {
            cloud_key: keys::service_link(&project.slug, &target.slug, link.kind.as_str())?,
            from_project_slug: project.slug.clone(),
            to_project_slug: target.slug,
            kind: link.kind,
            label: link.label,
        });
    }
    links.sort_by(|a, b| a.cloud_key.cmp(&b.cloud_key));
    Ok(links)
}

fn build_dependencies(
    db: &Db,
    project: &Project,
    items: &[SharedItem],
) -> Result<Vec<CloudArtifactDependency>> {
    let item_paths: HashMap<_, _> = items
        .iter()
        .filter_map(|item| item.path.as_ref().map(|path| (item.id, path.clone())))
        .collect();
    let mut dependencies = Vec::new();
    for dependency in db.list_artifact_dependencies_for_project(project.id)? {
        let Some(path) = item_paths.get(&dependency.shared_item_id) else {
            continue;
        };
        let Ok(path) = normalize_portable_rel_path(path) else {
            continue;
        };
        if !walk::path_allowed_by_options(Path::new(&path), WalkOptions::default()) {
            continue;
        }
        dependencies.push(CloudArtifactDependency {
            cloud_key: keys::dependency(
                &project.slug,
                &path,
                &dependency.depends_on_project_slug_snapshot,
                dependency.kind.as_str(),
            )?,
            project_slug: project.slug.clone(),
            share_path: path,
            target_project_slug: dependency.depends_on_project_slug_snapshot,
            kind: dependency.kind,
            reaction: dependency.reaction,
        });
    }
    dependencies.sort_by(|a, b| a.cloud_key.cmp(&b.cloud_key));
    Ok(dependencies)
}

fn build_events(
    db: &Db,
    project: &Project,
    group_slugs: &HashMap<i64, String>,
) -> Result<Vec<CloudEvent>> {
    let mut candidates = Vec::new();
    for event in db.list_workspace_events(Some(&project.slug), None)? {
        let mut targets = Vec::new();
        for target in db.list_event_targets(event.id)? {
            let project_slug = match target.affected_project_id {
                Some(id) => db.get_project_by_id(id)?.map(|project| project.slug),
                None => None,
            };
            targets.push(CloudEventTarget {
                cloud_key: String::new(),
                project_slug,
                relation_kind: target.relation_kind,
                status: target.status,
            });
        }
        targets.sort_by_key(|target| {
            (
                target.project_slug.clone(),
                target.relation_kind.as_str(),
                target.status.as_str(),
            )
        });

        let mut artifacts = Vec::new();
        for artifact in db.list_event_artifacts(event.id)? {
            let Ok(relative_path) = normalize_portable_rel_path(&artifact.path_snapshot) else {
                continue;
            };
            if !walk::path_allowed_by_options(Path::new(&relative_path), WalkOptions::default()) {
                continue;
            }
            let project_slug = match artifact.affected_project_id {
                Some(id) => db.get_project_by_id(id)?.map(|project| project.slug),
                None => None,
            };
            artifacts.push(CloudEventArtifact {
                cloud_key: String::new(),
                project_slug,
                relative_path,
                reaction: artifact.reaction,
                reason: artifact.reason,
                status: artifact.status,
            });
        }
        artifacts.sort_by_key(|artifact| {
            (
                artifact.project_slug.clone(),
                artifact.relative_path.clone(),
                artifact.reaction.as_str(),
                artifact.status.as_str(),
            )
        });

        let mut event_group_slugs = db
            .list_event_group_ids(event.id)?
            .into_iter()
            .filter_map(|id| group_slugs.get(&id).cloned())
            .collect::<Vec<_>>();
        event_group_slugs.sort();
        let cloud_event = CloudEvent {
            cloud_key: String::new(),
            source_project_slug: event.source_project_slug,
            source_project_name: event.source_project_name,
            group_slugs: event_group_slugs,
            kind: event.kind,
            title: event.title,
            body: event.body,
            severity: event.severity,
            status: event.status,
            created_at: event.created_at,
            updated_at: event.updated_at,
            targets,
            artifacts,
        };
        let fingerprint = sha256_hex(&event_fingerprint_input(&cloud_event)?);
        candidates.push((fingerprint, cloud_event));
    }
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            serde_json::to_string(&a.1)
                .unwrap_or_default()
                .cmp(&serde_json::to_string(&b.1).unwrap_or_default())
        })
    });
    let mut ordinals = HashMap::new();
    let mut events = Vec::new();
    for (fingerprint, mut event) in candidates {
        let ordinal = ordinals.entry(fingerprint.clone()).or_insert(0usize);
        event.cloud_key = keys::event(&project.slug, &fingerprint, *ordinal)?;
        for (index, target) in event.targets.iter_mut().enumerate() {
            target.cloud_key = keys::event_target(
                &event.cloud_key,
                target.project_slug.as_deref(),
                target.relation_kind.as_str(),
                index,
            )?;
        }
        for (index, artifact) in event.artifacts.iter_mut().enumerate() {
            artifact.cloud_key = keys::event_artifact(
                &event.cloud_key,
                artifact.project_slug.as_deref(),
                &artifact.relative_path,
                index,
            )?;
        }
        *ordinal += 1;
        events.push(event);
    }
    Ok(events)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, Db, Project) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("docs")).unwrap();
        fs::write(root.path().join("docs/readme.md"), "# Hello").unwrap();
        fs::write(root.path().join("docs/skip.txt"), "skip").unwrap();
        fs::create_dir(root.path().join(".ai-factory")).unwrap();
        fs::write(root.path().join(".ai-factory/secret.md"), "hidden").unwrap();
        let db = Db::open(&root.path().join("workspace.db")).unwrap();
        let id = db
            .create_project_with_slug("API", root.path().to_str().unwrap(), Some("api"))
            .unwrap();
        db.share_dir(id, "docs", Some("Docs")).unwrap();
        db.share_file(id, ".ai-factory/secret.md", None).unwrap();
        db.add_project_note(id, "remember this", Some("memo"))
            .unwrap();
        let project = db.get_project_by_id(id).unwrap().unwrap();
        (root, db, project)
    }

    #[test]
    fn snapshot_is_stable_and_excludes_unsafe_markdown() {
        let (_root, db, project) = fixture();
        let first = build_project_snapshot(&db, &project, true).unwrap();
        let second = build_project_snapshot(&db, &project, true).unwrap();

        assert_eq!(first.snapshot_hash, second.snapshot_hash);
        assert_eq!(first.snapshot.documents.len(), 1);
        assert_eq!(first.snapshot.documents[0].relative_path, "docs/readme.md");
        assert_eq!(first.snapshot.notes.len(), 1);
        let payload = serde_json::to_string(&first.snapshot).unwrap();
        assert!(payload.contains("remember this"));
        assert!(!payload.contains("secret.md"));
    }

    #[test]
    fn markdown_content_requires_opt_in() {
        let (_root, db, project) = fixture();
        let built = build_project_snapshot(&db, &project, false).unwrap();
        assert!(built.snapshot.documents.is_empty());
        assert_eq!(built.snapshot.notes.len(), 1);
    }

    #[test]
    fn snapshot_rejects_aggregate_payload_over_16_mib() {
        use crate::cloud::models::MAX_CLOUD_SNAPSHOT_BYTES;

        let (_root, db, mut project) = fixture();
        project.name = "x".repeat(MAX_CLOUD_SNAPSHOT_BYTES);

        assert_eq!(
            build_project_snapshot(&db, &project, false)
                .err()
                .unwrap()
                .to_string(),
            format!("Cloud snapshot exceeds {MAX_CLOUD_SNAPSHOT_BYTES} bytes")
        );
    }

    #[test]
    fn snapshot_includes_only_project_owned_group_notes() {
        let (_root, db, project) = fixture();
        let other_root = tempfile::tempdir().unwrap();
        let other_id = db
            .create_project_with_slug(
                "Worker",
                other_root.path().to_str().unwrap(),
                Some("worker"),
            )
            .unwrap();
        let group_id = db.get_or_create_group("Backend").unwrap();
        db.add_project_to_group(project.id, group_id).unwrap();
        db.add_project_to_group(other_id, group_id).unwrap();
        db.add_group_note(group_id, project.id, "owned by api", None)
            .unwrap();
        db.add_group_note(group_id, other_id, "owned by worker", None)
            .unwrap();

        let built = build_project_snapshot(&db, &project, false).unwrap();
        let payload = serde_json::to_string(&built.snapshot).unwrap();
        assert!(payload.contains("owned by api"));
        assert!(!payload.contains("owned by worker"));
    }

    #[test]
    fn cloud_policy_does_not_change_local_ai_factory_indexing() {
        let (_root, db, project) = fixture();
        let item = db
            .get_shared_items_for_project(project.id)
            .unwrap()
            .into_iter()
            .find(|item| item.path.as_deref() == Some(".ai-factory/secret.md"))
            .unwrap();

        let stats = indexer::index_shared_item(&db, &item, Path::new(&project.path)).unwrap();
        assert_eq!(stats.indexed, 1);
        let cloud = build_project_snapshot(&db, &project, true).unwrap();
        assert!(
            cloud
                .snapshot
                .documents
                .iter()
                .all(|document| document.relative_path != ".ai-factory/secret.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_skips_symlink_escape_non_utf8_and_oversized_markdown() {
        use std::os::unix::fs::symlink;

        let (root, db, project) = fixture();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("docs/escape.md")).unwrap();
        fs::write(root.path().join("docs/non-utf8.md"), [0xff, 0xfe]).unwrap();
        fs::write(
            root.path().join("docs/oversized.md"),
            vec![b'x'; MAX_INDEX_FILE_SIZE as usize + 1],
        )
        .unwrap();

        let built = build_project_snapshot(&db, &project, true).unwrap();
        assert_eq!(
            built
                .snapshot
                .documents
                .iter()
                .map(|document| document.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["docs/readme.md"]
        );
    }
}
