//! Markdown full-text indexer.
//!
//! Walks shared directories (and kind='file' .md shares), reads .md files, and
//! pushes their content into the `files_fts` index via [`crate::db::Db`].

use anyhow::{Context as _, Result};
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::db::{Db, ValidatedProjectPath, validate_project_rel_path};
use crate::models::{FileSearchHit, SharedItem, SharedItemKind};
use crate::walk::{self, WalkOptions, walk_project_tree};

/// Skip files larger than 1 MB (same limit used by grep).
pub const MAX_INDEX_FILE_SIZE: u64 = 1_024 * 1_024;

/// Summary of an indexing pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped_size: usize,
    pub skipped_non_utf8: usize,
    pub skipped_missing: usize,
}

fn is_md_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn mtime_epoch(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn reject_indexed_item(
    db: &Db,
    shared_item_id: i64,
    rel_path: &str,
    reason: impl std::fmt::Display,
    stats: Option<&mut IndexStats>,
) -> Result<()> {
    warn!(
        "index: dropping unsafe or stale index for shared item {} path '{}': {}",
        shared_item_id, rel_path, reason
    );
    db.delete_file_index(shared_item_id)?;
    if let Some(stats) = stats {
        stats.skipped_missing += 1;
    }
    Ok(())
}

fn validate_indexed_item_path(
    db: &Db,
    shared_item_id: i64,
    project_root: &Path,
    rel_path: &str,
    stats: Option<&mut IndexStats>,
) -> Result<Option<ValidatedProjectPath>> {
    if !walk::path_allowed_by_options(Path::new(rel_path), WalkOptions::default()) {
        reject_indexed_item(
            db,
            shared_item_id,
            rel_path,
            "path blocked by policy",
            stats,
        )?;
        return Ok(None);
    }

    match validate_project_rel_path(project_root, rel_path) {
        Ok(validated) => Ok(Some(validated)),
        Err(err) => {
            reject_indexed_item(db, shared_item_id, rel_path, err, stats)?;
            Ok(None)
        }
    }
}

fn canonical_root(project_root: &Path) -> Result<PathBuf> {
    project_root.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize project root: {}",
            project_root.display()
        )
    })
}

fn canonical_meta_path_allowed(project_root: &Path, abs_path: &str) -> Result<Option<PathBuf>> {
    let canonical_root = canonical_root(project_root)?;
    let Ok(canonical_meta) = Path::new(abs_path).canonicalize() else {
        return Ok(None);
    };
    if !canonical_meta.starts_with(&canonical_root) {
        return Ok(None);
    }
    let rel = canonical_meta
        .strip_prefix(&canonical_root)
        .unwrap_or(canonical_meta.as_path());
    if !walk::path_allowed_by_options(rel, WalkOptions::default()) {
        return Ok(None);
    }
    Ok(Some(canonical_meta))
}

fn validate_child_markdown_path(
    project_root: &Path,
    rel_path: &str,
) -> Result<Option<ValidatedProjectPath>> {
    if !walk::path_allowed_by_options(Path::new(rel_path), WalkOptions::default()) {
        warn!("index: skipping child blocked by policy: {}", rel_path);
        return Ok(None);
    }
    match validate_project_rel_path(project_root, rel_path) {
        Ok(validated) => Ok(Some(validated)),
        Err(err) => {
            warn!(
                "index: skipping child outside project root '{}': {}",
                rel_path, err
            );
            Ok(None)
        }
    }
}

/// Read a file and push it into the FTS index. Returns true if indexed,
/// false if skipped (size/non-utf8/missing).
fn index_single(
    db: &Db,
    shared_item_id: i64,
    validated: &ValidatedProjectPath,
    stats: &mut IndexStats,
) -> Result<bool> {
    let meta = match std::fs::metadata(&validated.canonical_path) {
        Ok(m) => m,
        Err(e) => {
            debug!(
                "index: missing {}: {}",
                validated.canonical_path.display(),
                e
            );
            stats.skipped_missing += 1;
            db.delete_file_index(shared_item_id)?;
            return Ok(false);
        }
    };
    if meta.len() > MAX_INDEX_FILE_SIZE {
        debug!(
            "index: skip (size {} > {}) {}",
            meta.len(),
            MAX_INDEX_FILE_SIZE,
            validated.canonical_path.display()
        );
        stats.skipped_size += 1;
        return Ok(false);
    }
    let bytes = match std::fs::read(&validated.canonical_path) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "index: read failed {}: {}",
                validated.canonical_path.display(),
                e
            );
            stats.skipped_missing += 1;
            db.delete_file_index(shared_item_id)?;
            return Ok(false);
        }
    };
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            warn!(
                "index: skip non-utf8 {}",
                validated.canonical_path.display()
            );
            stats.skipped_non_utf8 += 1;
            return Ok(false);
        }
    };
    db.index_file(
        shared_item_id,
        &validated.rel_path,
        &validated.canonical_path.to_string_lossy(),
        &content,
        mtime_epoch(&meta),
        meta.len() as i64,
    )?;
    stats.indexed += 1;
    Ok(true)
}

/// Index a single shared item. For `kind='file'` with .md → index one file.
/// For `kind='dir'` → walk the directory and index every .md file under it.
/// Notes and non-.md files are skipped.
pub fn index_shared_item(db: &Db, item: &SharedItem, project_root: &Path) -> Result<IndexStats> {
    let mut stats = IndexStats::default();
    match item.kind {
        SharedItemKind::File => {
            let Some(rel) = item.path.as_deref() else {
                return Ok(stats);
            };
            let Some(validated) =
                validate_indexed_item_path(db, item.id, project_root, rel, Some(&mut stats))?
            else {
                return Ok(stats);
            };
            if !is_md_path(Path::new(rel)) {
                debug!("index: skip non-md file share {}", rel);
                db.delete_file_index(item.id)?;
                return Ok(stats);
            }
            index_single(db, item.id, &validated, &mut stats)?;
        }
        SharedItemKind::Dir => {
            // MVP: index all .md files under the shared dir as a single
            // aggregate document keyed by the dir's shared_item_id. This keeps
            // the FTS→shared_items FK simple at the cost of coarser snippets.
            let Some(rel) = item.path.as_deref() else {
                return Ok(stats);
            };
            let Some(validated) =
                validate_indexed_item_path(db, item.id, project_root, rel, Some(&mut stats))?
            else {
                return Ok(stats);
            };
            if !validated.canonical_path.is_dir() {
                reject_indexed_item(
                    db,
                    item.id,
                    rel,
                    "shared directory path is not a directory",
                    Some(&mut stats),
                )?;
                return Ok(stats);
            }

            let root = canonical_root(project_root)?;
            let entries = walk_project_tree(
                &root,
                Some(&validated.rel_path),
                None,
                WalkOptions::default(),
            );
            let mut aggregate = String::new();
            let mut total_size: u64 = 0;
            let mut latest_mtime: i64 = 0;
            for entry in entries {
                if entry.is_dir || !is_md_path(Path::new(&entry.path)) {
                    continue;
                }
                let Some(child) = validate_child_markdown_path(&root, &entry.path)? else {
                    continue;
                };
                let Ok(meta) = std::fs::metadata(&child.canonical_path) else {
                    stats.skipped_missing += 1;
                    continue;
                };
                if meta.len() > MAX_INDEX_FILE_SIZE {
                    stats.skipped_size += 1;
                    debug!("index: skip (size) {}", child.canonical_path.display());
                    continue;
                }
                let Ok(bytes) = std::fs::read(&child.canonical_path) else {
                    stats.skipped_missing += 1;
                    continue;
                };
                let Ok(text) = String::from_utf8(bytes) else {
                    stats.skipped_non_utf8 += 1;
                    warn!("index: skip non-utf8 {}", child.canonical_path.display());
                    continue;
                };
                aggregate.push_str("\n\n### ");
                aggregate.push_str(&child.rel_path);
                aggregate.push_str("\n\n");
                aggregate.push_str(&text);
                total_size += meta.len();
                latest_mtime = latest_mtime.max(mtime_epoch(&meta));
                stats.indexed += 1;
            }
            if !aggregate.is_empty() {
                db.index_file(
                    item.id,
                    &validated.rel_path,
                    &validated.canonical_path.to_string_lossy(),
                    &aggregate,
                    latest_mtime,
                    total_size as i64,
                )?;
            } else {
                // No .md content under this dir — ensure index is empty
                db.delete_file_index(item.id)?;
            }
        }
        SharedItemKind::Note => {}
    }
    Ok(stats)
}

/// Re-read the on-disk file for a shared item if its mtime/size changed.
/// Returns true if a reindex was performed.
#[allow(dead_code)]
pub fn refresh_if_stale(db: &Db, item: &SharedItem, project_root: &Path) -> Result<bool> {
    let Some(rel) = item.path.as_deref() else {
        return Ok(false);
    };
    let Some(validated) = validate_indexed_item_path(db, item.id, project_root, rel, None)? else {
        return Ok(true);
    };
    let Some((_abs, old_mtime, old_size)) = db.get_file_index_meta(item.id)? else {
        // Not yet indexed — index it now.
        index_shared_item(db, item, project_root)?;
        return Ok(true);
    };

    match item.kind {
        SharedItemKind::File => {
            let Ok(meta) = std::fs::metadata(&validated.canonical_path) else {
                db.delete_file_index(item.id)?;
                return Ok(false);
            };
            if meta.len() as i64 != old_size || mtime_epoch(&meta) != old_mtime {
                debug!(
                    "refresh: mtime/size changed for {}",
                    validated.canonical_path.display()
                );
                index_shared_item(db, item, project_root)?;
                return Ok(true);
            }
        }
        SharedItemKind::Dir => {
            // For directories we re-scan and compare aggregate mtime/size.
            let root = canonical_root(project_root)?;
            let entries = walk_project_tree(
                &root,
                Some(&validated.rel_path),
                None,
                WalkOptions::default(),
            );
            let mut total_size: u64 = 0;
            let mut latest_mtime: i64 = 0;
            for entry in entries {
                if entry.is_dir || !is_md_path(Path::new(&entry.path)) {
                    continue;
                }
                let Some(child) = validate_child_markdown_path(&root, &entry.path)? else {
                    continue;
                };
                let Ok(meta) = std::fs::metadata(&child.canonical_path) else {
                    continue;
                };
                if meta.len() > MAX_INDEX_FILE_SIZE {
                    continue;
                }
                total_size += meta.len();
                latest_mtime = latest_mtime.max(mtime_epoch(&meta));
            }
            if total_size as i64 != old_size || latest_mtime != old_mtime {
                debug!("refresh: dir aggregate changed for {}", rel);
                index_shared_item(db, item, project_root)?;
                return Ok(true);
            }
        }
        SharedItemKind::Note => {}
    }
    Ok(false)
}

/// Rebuild the entire files_fts index for every shared_item whose kind is file/dir.
pub fn reindex_all(db: &Db) -> Result<IndexStats> {
    let start = Instant::now();
    info!("reindex_all: starting full rebuild");
    let mut stats = IndexStats::default();

    for project in db.list_projects()? {
        let project_root = PathBuf::from(&project.path);
        let items = db
            .get_shared_items_for_project(project.id)
            .with_context(|| format!("listing items for project {}", project.id))?;
        for item in items {
            match item.kind {
                SharedItemKind::File | SharedItemKind::Dir => {
                    let s = index_shared_item(db, &item, &project_root)?;
                    stats.indexed += s.indexed;
                    stats.skipped_size += s.skipped_size;
                    stats.skipped_non_utf8 += s.skipped_non_utf8;
                    stats.skipped_missing += s.skipped_missing;
                }
                SharedItemKind::Note => {}
            }
        }
    }

    info!(
        "reindex_all: indexed={} skipped_size={} skipped_non_utf8={} skipped_missing={} in {:?}",
        stats.indexed,
        stats.skipped_size,
        stats.skipped_non_utf8,
        stats.skipped_missing,
        start.elapsed()
    );
    Ok(stats)
}

/// Lazy pre-search refresh: stat each indexed file and reindex stale ones.
/// Bounded to `max_checks` to keep search latency predictable.
pub fn refresh_stale(db: &Db, max_checks: usize) -> Result<usize> {
    let start = Instant::now();
    let metas = db.list_file_index_meta()?;
    let mut refreshed = 0usize;
    for (id, abs_path, old_mtime, old_size) in metas.into_iter().take(max_checks) {
        let Some(item) = db.get_item_by_id(id)? else {
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };

        let Some(pid) = item.project_id else {
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };
        let Some(project) = db.get_project_by_id(pid)? else {
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };
        let root = PathBuf::from(&project.path);
        let Some(rel) = item.path.as_deref() else {
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };
        let Some(validated) = validate_indexed_item_path(db, id, &root, rel, None)? else {
            refreshed += 1;
            continue;
        };
        let Some(canonical_meta) = canonical_meta_path_allowed(&root, &abs_path)? else {
            warn!(
                "refresh_stale: dropping stale meta path outside project for id={}: {}",
                id, abs_path
            );
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };
        if canonical_meta != validated.canonical_path {
            index_shared_item(db, &item, &root)?;
            refreshed += 1;
            continue;
        }

        let Ok(meta) = std::fs::metadata(&validated.canonical_path) else {
            db.delete_file_index(id)?;
            refreshed += 1;
            continue;
        };

        if item.kind == SharedItemKind::Dir || meta.is_dir() {
            if refresh_if_stale(db, &item, &root)? {
                refreshed += 1;
            }
        } else if meta.len() as i64 != old_size || mtime_epoch(&meta) != old_mtime {
            index_shared_item(db, &item, &root)?;
            refreshed += 1;
        }
    }
    debug!(
        "refresh_stale: refreshed {} files in {:?}",
        refreshed,
        start.elapsed()
    );
    Ok(refreshed)
}

/// Revalidate FTS hits that could otherwise expose stale unsafe snippets.
///
/// `refresh_stale` is intentionally bounded for latency. Before returning
/// search snippets, reindex matching directory aggregates because their visible
/// row path can still contain old hidden/sensitive child content.
pub fn refresh_search_hits(db: &Db, hits: &[FileSearchHit]) -> Result<usize> {
    let mut refreshed = 0usize;

    for hit in hits {
        if !walk::path_allowed_by_options(Path::new(&hit.path), WalkOptions::default()) {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        }

        let Some(item) = db.get_item_by_id(hit.shared_item_id)? else {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        };

        let Some(project_id) = item.project_id else {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        };
        let Some(project) = db.get_project_by_id(project_id)? else {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        };
        let root = PathBuf::from(&project.path);
        let Some(rel) = item.path.as_deref() else {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        };
        let Some(validated) = validate_indexed_item_path(db, hit.shared_item_id, &root, rel, None)?
        else {
            refreshed += 1;
            continue;
        };
        if let Some((abs_path, _mtime, _size)) = db.get_file_index_meta(hit.shared_item_id)?
            && canonical_meta_path_allowed(&root, &abs_path)?.as_ref()
                != Some(&validated.canonical_path)
        {
            db.delete_file_index(hit.shared_item_id)?;
            refreshed += 1;
            continue;
        }

        if item.kind != SharedItemKind::Dir && hit.path == validated.rel_path {
            continue;
        }

        index_shared_item(db, &item, &root)?;
        refreshed += 1;
    }

    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::fs;
    use tempfile::{NamedTempFile, TempDir};

    fn setup() -> (Db, TempDir, i64) {
        let tmp_db = NamedTempFile::new().unwrap();
        let db = Db::open(tmp_db.path()).unwrap();
        std::mem::forget(tmp_db); // keep file alive for db connection
        let proj_dir = TempDir::new().unwrap();
        let pid = db
            .create_project("p", proj_dir.path().to_str().unwrap())
            .unwrap();
        (db, proj_dir, pid)
    }

    fn setup_nested_project() -> (Db, TempDir, PathBuf, i64) {
        let tmp_db = NamedTempFile::new().unwrap();
        let db = Db::open(tmp_db.path()).unwrap();
        std::mem::forget(tmp_db);
        let workspace = TempDir::new().unwrap();
        let project_root = workspace.path().join("app");
        fs::create_dir(&project_root).unwrap();
        let pid = db
            .create_project("p", project_root.to_str().unwrap())
            .unwrap();
        (db, workspace, project_root, pid)
    }

    #[test]
    fn index_md_file_share() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join("readme.md"), "alpha bravo charlie").unwrap();
        let id = db.share_file(pid, "readme.md", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        let stats = index_shared_item(&db, &item, proj.path()).unwrap();
        assert_eq!(stats.indexed, 1);
        let hits = db.search_files("bravo", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "readme.md");
    }

    #[test]
    fn skip_non_md_file_share() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join("code.rs"), "fn main() {}").unwrap();
        let id = db.share_file(pid, "code.rs", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        let stats = index_shared_item(&db, &item, proj.path()).unwrap();
        assert_eq!(stats.indexed, 0);
    }

    #[test]
    fn skip_hidden_sensitive_md_file_share() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join(".env.md"), "hidden_secret_marker").unwrap();
        let id = db.share_file(pid, ".env.md", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();

        let stats = index_shared_item(&db, &item, proj.path()).unwrap();

        assert_eq!(stats.indexed, 0);
        assert!(
            db.search_files("hidden_secret_marker", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn index_dir_aggregates_md_files() {
        let (db, proj, pid) = setup();
        fs::create_dir_all(proj.path().join("docs")).unwrap();
        fs::write(proj.path().join("docs/a.md"), "alpha unique_word_aaa").unwrap();
        fs::write(proj.path().join("docs/b.md"), "bravo unique_word_bbb").unwrap();
        fs::write(proj.path().join("docs/ignore.txt"), "not markdown").unwrap();
        let id = db.share_dir(pid, "docs", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        let stats = index_shared_item(&db, &item, proj.path()).unwrap();
        assert_eq!(stats.indexed, 2);
        assert_eq!(db.search_files("unique_word_aaa", 10).unwrap().len(), 1);
        assert_eq!(db.search_files("unique_word_bbb", 10).unwrap().len(), 1);
    }

    #[test]
    fn skip_large_file() {
        let (db, proj, pid) = setup();
        let big = "x".repeat((MAX_INDEX_FILE_SIZE as usize) + 1);
        fs::write(proj.path().join("huge.md"), big).unwrap();
        let id = db.share_file(pid, "huge.md", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        let stats = index_shared_item(&db, &item, proj.path()).unwrap();
        assert_eq!(stats.indexed, 0);
        assert_eq!(stats.skipped_size, 1);
    }

    #[test]
    fn refresh_detects_mtime_change() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join("t.md"), "old_token").unwrap();
        let id = db.share_file(pid, "t.md", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        index_shared_item(&db, &item, proj.path()).unwrap();

        // Modify and ensure mtime advances
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(proj.path().join("t.md"), "new_token").unwrap();

        let changed = refresh_if_stale(&db, &item, proj.path()).unwrap();
        assert!(changed);
        assert_eq!(db.search_files("old_token", 10).unwrap().len(), 0);
        assert_eq!(db.search_files("new_token", 10).unwrap().len(), 1);
    }

    #[test]
    fn refresh_stale_removes_previously_indexed_sensitive_file() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join(".env.md"), "stale_secret_marker").unwrap();
        let id = db.share_file(pid, ".env.md", None).unwrap();
        let abs = proj.path().join(".env.md");
        let meta = fs::metadata(&abs).unwrap();
        db.index_file(
            id,
            ".env.md",
            &abs.to_string_lossy(),
            "stale_secret_marker",
            mtime_epoch(&meta),
            meta.len() as i64,
        )
        .unwrap();
        assert!(db.get_file_index_meta(id).unwrap().is_some());

        let refreshed = refresh_stale(&db, 200).unwrap();

        assert_eq!(refreshed, 1);
        assert!(
            db.search_files("stale_secret_marker", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn refresh_stale_reindexes_directory_to_drop_hidden_children() {
        let (db, proj, pid) = setup();
        fs::create_dir_all(proj.path().join("docs")).unwrap();
        fs::write(proj.path().join("docs/public.md"), "public_marker").unwrap();
        fs::write(proj.path().join("docs/.hidden.md"), "stale_hidden_marker").unwrap();
        let id = db.share_dir(pid, "docs", None).unwrap();
        let abs = proj.path().join("docs");
        db.index_file(
            id,
            "docs",
            &abs.to_string_lossy(),
            "public_marker stale_hidden_marker",
            1,
            1024,
        )
        .unwrap();
        assert_eq!(db.search_files("stale_hidden_marker", 10).unwrap().len(), 1);

        let refreshed = refresh_stale(&db, 200).unwrap();

        assert_eq!(refreshed, 1);
        assert!(
            db.search_files("stale_hidden_marker", 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.search_files("public_marker", 10).unwrap().len(), 1);
    }

    #[test]
    fn reindex_all_processes_all_projects() {
        let (db, proj, pid) = setup();
        fs::write(proj.path().join("a.md"), "reindexable_abc").unwrap();
        db.share_file(pid, "a.md", None).unwrap();
        let stats = reindex_all(&db).unwrap();
        assert!(stats.indexed >= 1);
        assert_eq!(db.search_files("reindexable_abc", 10).unwrap().len(), 1);
    }

    #[test]
    fn reindex_all_drops_legacy_parent_traversal_share() {
        let (db, workspace, _project_root, pid) = setup_nested_project();
        fs::write(workspace.path().join("secret.md"), "outside_secret_marker").unwrap();
        let id = db.share_file(pid, "../secret.md", None).unwrap();

        let stats = reindex_all(&db).unwrap();

        assert_eq!(stats.indexed, 0);
        assert!(db.get_file_index_meta(id).unwrap().is_none());
        assert!(
            db.search_files("outside_secret_marker", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn reindex_all_drops_legacy_symlink_escape_share() {
        use std::os::unix::fs::symlink;

        let (db, workspace, project_root, pid) = setup_nested_project();
        fs::write(workspace.path().join("secret.md"), "symlink_secret_marker").unwrap();
        symlink(
            workspace.path().join("secret.md"),
            project_root.join("escape.md"),
        )
        .unwrap();
        let id = db.share_file(pid, "escape.md", None).unwrap();

        let stats = reindex_all(&db).unwrap();

        assert_eq!(stats.indexed, 0);
        assert!(db.get_file_index_meta(id).unwrap().is_none());
        assert!(
            db.search_files("symlink_secret_marker", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_files_drops_stale_out_of_root_meta_path() {
        let (db, workspace, project_root, pid) = setup_nested_project();
        fs::write(project_root.join("README.md"), "safe readme").unwrap();
        let outside = workspace.path().join("secret.md");
        fs::write(&outside, "stale_outside_marker").unwrap();
        let meta = fs::metadata(&outside).unwrap();
        let id = db.share_file(pid, "README.md", None).unwrap();
        db.index_file(
            id,
            "README.md",
            &outside.to_string_lossy(),
            "stale_outside_marker",
            mtime_epoch(&meta),
            meta.len() as i64,
        )
        .unwrap();

        let hits = db.search_files("stale_outside_marker", 10).unwrap();
        assert!(hits.is_empty());
        assert!(db.get_file_index_meta(id).unwrap().is_none());
    }
}
