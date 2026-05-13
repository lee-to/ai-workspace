//! Markdown full-text indexer.
//!
//! Walks shared directories (and kind='file' .md shares), reads .md files, and
//! pushes their content into the `files_fts` index via [`crate::db::Db`].

use anyhow::{Context as _, Result};
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::db::Db;
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

/// Read a file and push it into the FTS index. Returns true if indexed,
/// false if skipped (size/non-utf8/missing).
fn index_single(
    db: &Db,
    shared_item_id: i64,
    project_root: &Path,
    rel_path: &str,
    stats: &mut IndexStats,
) -> Result<bool> {
    let abs = project_root.join(rel_path);
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => {
            debug!("index: missing {}: {}", abs.display(), e);
            stats.skipped_missing += 1;
            db.delete_indexed_file(shared_item_id, rel_path)?;
            return Ok(false);
        }
    };
    if meta.len() > MAX_INDEX_FILE_SIZE {
        debug!(
            "index: skip (size {} > {}) {}",
            meta.len(),
            MAX_INDEX_FILE_SIZE,
            abs.display()
        );
        stats.skipped_size += 1;
        db.delete_indexed_file(shared_item_id, rel_path)?;
        return Ok(false);
    }
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(e) => {
            warn!("index: read failed {}: {}", abs.display(), e);
            stats.skipped_missing += 1;
            db.delete_indexed_file(shared_item_id, rel_path)?;
            return Ok(false);
        }
    };
    let content = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            warn!("index: skip non-utf8 {}", abs.display());
            stats.skipped_non_utf8 += 1;
            db.delete_indexed_file(shared_item_id, rel_path)?;
            return Ok(false);
        }
    };
    let abs_str = abs.to_string_lossy().to_string();
    db.index_file(
        shared_item_id,
        rel_path,
        &abs_str,
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
            if !is_md_path(Path::new(rel)) {
                debug!("index: skip non-md file share {}", rel);
                db.delete_file_index(item.id)?;
                return Ok(stats);
            }
            if !walk::path_allowed_by_options(Path::new(rel), WalkOptions::default()) {
                debug!("index: remove hidden/sensitive file share {}", rel);
                db.delete_file_index(item.id)?;
                return Ok(stats);
            }
            index_single(db, item.id, project_root, rel, &mut stats)?;
        }
        SharedItemKind::Dir => {
            let Some(rel) = item.path.as_deref() else {
                return Ok(stats);
            };
            let entries = walk_project_tree(project_root, Some(rel), None, WalkOptions::default());
            let mut seen = std::collections::HashSet::new();
            for entry in entries {
                if entry.is_dir || !is_md_path(Path::new(&entry.path)) {
                    continue;
                }
                let rel_path = entry.path.clone();
                if index_single(db, item.id, project_root, &rel_path, &mut stats)? {
                    seen.insert(rel_path);
                }
            }

            for (indexed_rel_path, _, _, _) in db.list_indexed_files_for_item(item.id)? {
                if !seen.contains(&indexed_rel_path) {
                    db.delete_indexed_file(item.id, &indexed_rel_path)?;
                }
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

    match item.kind {
        SharedItemKind::File => {
            if !is_md_path(Path::new(rel)) {
                db.delete_file_index(item.id)?;
                return Ok(false);
            }
            if !walk::path_allowed_by_options(Path::new(rel), WalkOptions::default()) {
                db.delete_file_index(item.id)?;
                return Ok(true);
            }
            let Some((_abs, old_mtime, old_size)) = db.get_file_index_meta(item.id)? else {
                index_shared_item(db, item, project_root)?;
                return Ok(true);
            };

            let abs = project_root.join(rel);
            let Ok(meta) = std::fs::metadata(&abs) else {
                db.delete_indexed_file(item.id, rel)?;
                return Ok(true);
            };
            if meta.len() as i64 != old_size || mtime_epoch(&meta) != old_mtime {
                debug!("refresh: mtime/size changed for {}", abs.display());
                index_shared_item(db, item, project_root)?;
                return Ok(true);
            }
        }
        SharedItemKind::Dir => {
            if !walk::path_allowed_by_options(Path::new(rel), WalkOptions::default()) {
                db.delete_file_index(item.id)?;
                return Ok(true);
            }

            let entries = walk_project_tree(project_root, Some(rel), None, WalkOptions::default());
            let mut current = std::collections::HashMap::new();
            for entry in entries {
                if entry.is_dir || !is_md_path(Path::new(&entry.path)) {
                    continue;
                }
                let abs = project_root.join(&entry.path);
                let Ok(meta) = std::fs::metadata(&abs) else {
                    continue;
                };
                if meta.len() > MAX_INDEX_FILE_SIZE {
                    continue;
                }
                current.insert(entry.path, (mtime_epoch(&meta), meta.len() as i64));
            }

            let indexed = db.list_indexed_files_for_item(item.id)?;
            let mut stale = indexed.len() != current.len();
            if !stale {
                for (indexed_rel_path, _, indexed_mtime, indexed_size) in &indexed {
                    if current.get(indexed_rel_path).is_none_or(|(mtime, size)| {
                        *mtime != *indexed_mtime || *size != *indexed_size
                    }) {
                        stale = true;
                        break;
                    }
                }
            }

            if stale {
                debug!("refresh: dir children changed for {}", rel);
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
    db.clear_file_index()?;

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
    if max_checks == 0 {
        debug!("refresh_stale: refreshed 0 files in {:?}", start.elapsed());
        return Ok(0);
    }

    let metas = db.list_file_index_meta(max_checks)?;
    let mut remaining_checks = max_checks;
    let mut refreshed = 0usize;
    for (indexed_file_id, shared_item_id, rel_path, abs_path, old_mtime, old_size) in
        metas.into_iter()
    {
        remaining_checks -= 1;
        let Some(item) = db.get_item_by_id(shared_item_id)? else {
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
            continue;
        };

        if item.path.as_deref().is_none_or(|rel| {
            !walk::path_allowed_by_options(Path::new(rel), WalkOptions::default())
        }) || !walk::path_allowed_by_options(Path::new(&rel_path), WalkOptions::default())
        {
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
            continue;
        }

        let Ok(meta) = std::fs::metadata(&abs_path) else {
            // file disappeared — drop from index
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
            continue;
        };
        let Some(pid) = item.project_id else {
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
            continue;
        };
        let Some(project) = db.get_project_by_id(pid)? else {
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
            continue;
        };

        if meta.is_dir() || meta.len() > MAX_INDEX_FILE_SIZE {
            db.delete_indexed_file_by_id(indexed_file_id)?;
            refreshed += 1;
        } else if meta.len() as i64 != old_size || mtime_epoch(&meta) != old_mtime {
            let root = PathBuf::from(&project.path);
            let mut stats = IndexStats::default();
            index_single(db, shared_item_id, &root, &rel_path, &mut stats)?;
            refreshed += 1;
        }
    }

    for item in db.list_unindexed_file_items(remaining_checks)? {
        let Some(project_id) = item.project_id else {
            db.delete_file_index(item.id)?;
            refreshed += 1;
            continue;
        };
        let Some(project) = db.get_project_by_id(project_id)? else {
            db.delete_file_index(item.id)?;
            refreshed += 1;
            continue;
        };

        let stats = index_shared_item(db, &item, &PathBuf::from(&project.path))?;
        if stats.indexed > 0
            || stats.skipped_size > 0
            || stats.skipped_non_utf8 > 0
            || stats.skipped_missing > 0
        {
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
/// search snippets, reindex matching directory-owned file hits because old
/// indexed rows may still point at hidden/sensitive child paths.
pub fn refresh_search_hits(db: &Db, hits: &[FileSearchHit]) -> Result<usize> {
    let mut refreshed = 0usize;

    for hit in hits {
        if !walk::path_allowed_by_options(Path::new(&hit.path), WalkOptions::default()) {
            db.delete_indexed_file(hit.shared_item_id, &hit.path)?;
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

        if item.kind == SharedItemKind::Dir
            && refresh_if_stale(db, &item, &PathBuf::from(&project.path))?
        {
            refreshed += 1;
        }
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
    fn index_dir_indexes_md_files_individually() {
        let (db, proj, pid) = setup();
        fs::create_dir_all(proj.path().join("docs")).unwrap();
        fs::write(proj.path().join("docs/a.md"), "alpha unique_word_aaa").unwrap();
        fs::write(proj.path().join("docs/b.md"), "bravo unique_word_bbb").unwrap();
        fs::write(proj.path().join("docs/ignore.txt"), "not markdown").unwrap();
        let id = db.share_dir(pid, "docs", None).unwrap();
        let item = db.get_item_by_id(id).unwrap().unwrap();
        let stats = index_shared_item(&db, &item, proj.path()).unwrap();
        assert_eq!(stats.indexed, 2);
        let a_hits = db.search_files("unique_word_aaa", 10).unwrap();
        let b_hits = db.search_files("unique_word_bbb", 10).unwrap();
        assert_eq!(a_hits.len(), 1);
        assert_eq!(a_hits[0].path, "docs/a.md");
        assert_eq!(b_hits.len(), 1);
        assert_eq!(b_hits[0].path, "docs/b.md");
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
        assert_eq!(db.search_files("stale_secret_marker", 10).unwrap().len(), 1);

        let refreshed = refresh_stale(&db, 200).unwrap();

        assert!(refreshed >= 1);
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

        assert!(refreshed >= 1);
        assert!(
            db.search_files("stale_hidden_marker", 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.search_files("public_marker", 10).unwrap().len(), 1);
    }

    #[test]
    fn refresh_stale_zero_budget_does_not_reconcile_unindexed_directory() {
        let (db, proj, pid) = setup();
        fs::create_dir_all(proj.path().join("docs")).unwrap();
        fs::write(
            proj.path().join("docs/new.md"),
            "zero_budget_directory_marker",
        )
        .unwrap();
        db.share_dir(pid, "docs", None).unwrap();

        let refreshed = refresh_stale(&db, 0).unwrap();

        assert_eq!(refreshed, 0);
        assert!(
            db.search_files("zero_budget_directory_marker", 10)
                .unwrap()
                .is_empty()
        );
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
}
