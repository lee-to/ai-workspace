use log::{debug, info, warn};
use serde::Serialize;
use std::io::{BufRead, Read as _};
use std::path::Path;

/// A single entry in the project file tree.
#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
}

/// A single grep match within a project.
#[derive(Debug, Clone, Serialize)]
pub struct GrepMatch {
    pub path: String,
    pub line_number: usize,
    pub line_content: String,
}

/// Maximum file size for grep scanning (1 MB).
const MAX_GREP_FILE_SIZE: u64 = 1_024 * 1_024;

/// Maximum number of grep matches to return.
const MAX_GREP_MATCHES: usize = 100;

/// Size of the buffer used for binary detection (8 KB).
const BINARY_DETECT_SIZE: usize = 8 * 1024;

/// Check if a file is likely binary by looking for null bytes in the first 8KB.
fn is_binary(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0u8; BINARY_DETECT_SIZE];
    let Ok(n) = file.read(&mut buf) else {
        return false;
    };
    buf[..n].contains(&0)
}

/// Walk the project file tree, respecting .gitignore rules.
///
/// If `subpath` is provided, only entries under that subdirectory are returned.
/// Paths in the result are relative to `root`.
pub fn walk_project_tree(root: &Path, subpath: Option<&str>) -> Vec<FileEntry> {
    let walk_root = match subpath {
        Some(sub) => root.join(sub),
        None => root.to_path_buf(),
    };

    info!(
        "walk_project_tree: root={}, subpath={:?}",
        root.display(),
        subpath
    );

    if !walk_root.exists() {
        warn!("Walk root does not exist: {}", walk_root.display());
        return Vec::new();
    }

    let walker = ignore::WalkBuilder::new(&walk_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();

    let mut entries = Vec::new();
    for result in walker {
        let Ok(entry) = result else {
            continue;
        };

        let entry_path = entry.path();

        // Skip the root directory itself
        if entry_path == walk_root {
            continue;
        }

        let rel = match entry_path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let rel_str = rel.to_string_lossy().to_string();
        let name = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        debug!("walk entry: {} (dir={})", rel_str, is_dir);

        entries.push(FileEntry {
            path: rel_str,
            name,
            is_dir,
        });
    }

    info!("walk_project_tree: found {} entries", entries.len());
    entries
}

/// Grep through project files for a regex pattern, respecting .gitignore.
///
/// If `glob_pattern` is provided, only files matching that glob are searched.
/// Returns up to MAX_GREP_MATCHES results. Skips binary files and files > 1MB.
pub fn grep_project(
    root: &Path,
    pattern: &str,
    glob_pattern: Option<&str>,
) -> Result<Vec<GrepMatch>, String> {
    info!(
        "grep_project: root={}, pattern={}, glob={:?}",
        root.display(),
        pattern,
        glob_pattern
    );

    let re = regex::Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);

    // Apply glob filter if provided
    let glob_matcher = if let Some(glob) = glob_pattern {
        let mut types_builder = ignore::types::TypesBuilder::new();
        types_builder
            .add("custom", glob)
            .map_err(|e| format!("Invalid glob pattern: {}", e))?;
        types_builder.select("custom");
        Some(types_builder.build().map_err(|e| format!("Glob build error: {}", e))?)
    } else {
        None
    };

    let walker = builder.build();
    let mut matches = Vec::new();

    for result in walker {
        if matches.len() >= MAX_GREP_MATCHES {
            debug!("grep_project: hit max matches limit ({})", MAX_GREP_MATCHES);
            break;
        }

        let Ok(entry) = result else {
            continue;
        };

        let entry_path = entry.path();

        // Skip directories
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
            continue;
        }

        // Check glob filter
        if let Some(ref types) = glob_matcher {
            let matched = types.matched(entry_path, false);
            if matched.is_ignore() || (!matched.is_whitelist() && glob_pattern.is_some()) {
                continue;
            }
        }

        // Check file size
        let Ok(meta) = std::fs::metadata(entry_path) else {
            continue;
        };
        if meta.len() > MAX_GREP_FILE_SIZE {
            debug!(
                "grep_project: skipping large file {} ({} bytes)",
                entry_path.display(),
                meta.len()
            );
            continue;
        }

        // Check binary
        if is_binary(entry_path) {
            debug!(
                "grep_project: skipping binary file {}",
                entry_path.display()
            );
            continue;
        }

        let Ok(file) = std::fs::File::open(entry_path) else {
            continue;
        };

        let rel = match entry_path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };

        let reader = std::io::BufReader::new(file);
        for (line_idx, line_result) in reader.lines().enumerate() {
            if matches.len() >= MAX_GREP_MATCHES {
                break;
            }

            let Ok(line) = line_result else {
                break;
            };

            if re.is_match(&line) {
                debug!("grep match: {}:{}", rel, line_idx + 1);
                matches.push(GrepMatch {
                    path: rel.clone(),
                    line_number: line_idx + 1,
                    line_content: line,
                });
            }
        }
    }

    info!("grep_project: found {} matches", matches.len());
    Ok(matches)
}
