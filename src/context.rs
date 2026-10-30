//! Checkout identity and bounded shared-source revisions. Paths stay local;
//! clients receive opaque binding IDs and deterministic content/config hashes.
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db::{Db, validate_project_rel_path};
use crate::models::SharedItemKind;

const MAX_FILES: usize = 10_000;
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) type SearchContexts = BTreeMap<(i64, String), (serde_json::Value, bool)>;

#[derive(Debug)]
pub(crate) struct SnapshotLimit;
impl std::fmt::Display for SnapshotLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared snapshot limit exceeded; narrow its shares")
    }
}
impl std::error::Error for SnapshotLimit {}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SourceContext {
    pub complete: bool,
    pub project_id: i64,
    pub logical_project_id: String,
    pub binding_id: String,
    pub revision: Option<String>,
    pub source_digest: String,
    pub share_digest: String,
    pub snapshot_id: String,
}

pub(crate) fn verify_checkout(db: &Db, project_id: i64) -> Result<PathBuf> {
    db.verify_checkout(project_id)
}

fn frame(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub(crate) fn file_digest(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buf = [0u8; 65536];
    loop {
        let count = file.read(&mut buf)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_SOURCE_BYTES {
            return Err(SnapshotLimit.into());
        }
        hash.update(&buf[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn collect(
    root: &Path,
    rel: &str,
    scope_root: &Path,
    files: &mut BTreeSet<String>,
    budget: &mut usize,
) -> Result<()> {
    *budget += 1;
    if *budget > MAX_FILES || rel.split('/').count() > 64 {
        return Err(SnapshotLimit.into());
    }
    if !crate::walk::path_allowed_for_shared_ai_factory(Path::new(rel), Default::default()) {
        return Ok(());
    }
    let Ok(validated) = validate_project_rel_path(root, rel) else {
        return Ok(());
    };
    if !validated.canonical_path.starts_with(scope_root)
        || !crate::walk::path_allowed_for_shared_ai_factory(
            validated.canonical_path.strip_prefix(root)?,
            Default::default(),
        )
    {
        return Ok(());
    }
    if validated.canonical_path.is_dir() {
        for entry in std::fs::read_dir(&validated.canonical_path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Non-UTF8 shared path"))?;
            collect(root, &format!("{rel}/{name}"), scope_root, files, budget)?;
        }
    } else if validated.canonical_path.is_file() {
        files.insert(validated.rel_path);
        if files.len() > MAX_FILES {
            return Err(SnapshotLimit.into());
        }
    }
    Ok(())
}

fn git_revision(root: &Path) -> Result<Option<String>> {
    if !root.join(".git").try_exists()? {
        return Ok(None);
    }
    let mut cmd = Command::new("git");
    cmd.args(["-c", "safe.directory=*"])
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    let output = cmd.output().context("Cannot determine checkout revision")?;
    // An unborn repository has no commit. Source hashes still bind its contents.
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_string()))
}

pub(crate) fn capture(db: &Db, project_id: i64) -> Result<SourceContext> {
    let first = capture_once(db, project_id)?;
    let second = capture_once(db, project_id)?;
    if first.snapshot_id != second.snapshot_id {
        bail!("Stale context: source changed while capturing snapshot; retry");
    }
    Ok(second)
}

fn capture_once(db: &Db, project_id: i64) -> Result<SourceContext> {
    let root = verify_checkout(db, project_id)?;
    let (logical_project_id, binding_id, _) = db.checkout_binding(project_id)?;
    let mut items = db.get_shared_items_for_project(project_id)?;
    items.sort_by_key(|item| item.id);
    let mut config = Sha256::new();
    let mut files = BTreeSet::new();
    let mut budget = 0;
    for item in items {
        frame(&mut config, &serde_json::to_vec(&item)?);
        if matches!(item.kind, SharedItemKind::File | SharedItemKind::Dir)
            && let Some(path) = item.path.as_deref()
        {
            let path = crate::path::normalize_portable_rel_path(path)?;
            if root.join(&path).try_exists()? {
                if let Ok(validated) = validate_project_rel_path(&root, &path) {
                    collect(
                        &root,
                        &path,
                        &validated.canonical_path,
                        &mut files,
                        &mut budget,
                    )?;
                }
            } else {
                // Missing shares change the digest instead of looking current.
                frame(&mut config, format!("missing:{path}").as_bytes());
            }
        }
    }
    let mut groups = db.get_groups_for_project(project_id)?;
    groups.sort_by_key(|group| group.id);
    frame(&mut config, &serde_json::to_vec(&groups)?);
    let share_digest = format!("{:x}", config.finalize());
    let mut content = Sha256::new();
    let mut total_bytes = 0u64;
    for path in files {
        let validated = validate_project_rel_path(&root, &path)?;
        total_bytes =
            total_bytes.saturating_add(std::fs::metadata(&validated.canonical_path)?.len());
        if total_bytes > MAX_SOURCE_BYTES {
            return Err(SnapshotLimit.into());
        }
        frame(&mut content, path.as_bytes());
        frame(
            &mut content,
            file_digest(&validated.canonical_path)?.as_bytes(),
        );
    }
    let source_digest = format!("{:x}", content.finalize());
    let revision = git_revision(&root)?;
    let mut snapshot = Sha256::new();
    for value in [
        &logical_project_id,
        &binding_id,
        &source_digest,
        &share_digest,
        revision.as_deref().unwrap_or(""),
        &root.to_string_lossy(),
    ] {
        frame(&mut snapshot, value.as_bytes());
    }
    Ok(SourceContext {
        complete: true,
        project_id,
        logical_project_id,
        binding_id,
        revision,
        source_digest,
        share_digest,
        snapshot_id: format!("{:x}", snapshot.finalize()),
    })
}

pub(crate) fn search_contexts(
    db: &Db,
    hits: &[crate::models::FileSearchHit],
) -> Result<SearchContexts> {
    let mut contexts = BTreeMap::new();
    for hit in hits {
        if let std::collections::btree_map::Entry::Vacant(entry) = contexts.entry(hit.project_id) {
            let context = match capture(db, hit.project_id) {
                Ok(context) => Some(context),
                Err(e) if e.is::<SnapshotLimit>() => None,
                Err(e) => return Err(e),
            };
            entry.insert(context);
        }
    }
    let mut bound = BTreeMap::new();
    for hit in hits {
        let root = verify_checkout(db, hit.project_id)?;
        let path = validate_project_rel_path(&root, &hit.path)?;
        let stale = file_digest(&path.canonical_path)? != hit.content_hash;
        bound.insert(
            (hit.shared_item_id, hit.path.clone()),
            (
                match &contexts[&hit.project_id] {
                    Some(context) => serde_json::to_value(context)?,
                    None => incomplete_context(db, hit.project_id)?,
                },
                stale,
            ),
        );
    }
    for (pid, before) in contexts {
        if let Some(before) = before
            && before.snapshot_id != capture(db, pid)?.snapshot_id
        {
            bail!("Stale context: source changed during search; retry");
        }
    }
    Ok(bound)
}

pub(crate) fn incomplete_context(db: &Db, project_id: i64) -> Result<serde_json::Value> {
    verify_checkout(db, project_id)?;
    let (logical_project_id, binding_id, _) = db.checkout_binding(project_id)?;
    Ok(
        serde_json::json!({"project_id": project_id, "logical_project_id": logical_project_id,
        "binding_id": binding_id, "complete": false, "reason": "snapshot_limit"}),
    )
}

/// Reject stale indexed symbols before they can be combined with live source.
/// Unshared files are left to the caller's visibility policy and never read.
pub(crate) fn validate_code_index(
    db: &Db,
    project_id: i64,
    full_project: bool,
    visible: impl Fn(&str) -> bool,
) -> Result<()> {
    let root = verify_checkout(db, project_id)?;
    let project = db
        .get_project_by_id(project_id)?
        .context("Project not found")?;
    if crate::codegraph::unindexed_candidates(db, &project, full_project)?
        .iter()
        .any(|path| visible(path))
    {
        bail!(
            "Stale CodeGraph index: new source files; run ai-workspace codegraph sync or reindex"
        );
    }
    for (path, expected) in db.indexed_code_hashes(project_id)? {
        if visible(&path) {
            let validated = validate_project_rel_path(&root, &path).context(
                "Stale CodeGraph index: indexed source missing or unsafe; run codegraph sync",
            )?;
            if crate::codegraph::stable_hash_hex(&std::fs::read(&validated.canonical_path)?)
                != expected
            {
                bail!(
                    "Stale CodeGraph index: source changed; run ai-workspace codegraph sync or reindex"
                );
            }
        }
    }
    Ok(())
}
