use log::{debug, info, warn};
use serde::Serialize;
use std::collections::HashSet;
use std::io::{BufRead, Read as _};
use std::path::{Component, Path, PathBuf};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepScopeKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepScope {
    pub kind: GrepScopeKind,
    pub rel_path: String,
}

/// Maximum file size for grep scanning (1 MB).
const MAX_GREP_FILE_SIZE: u64 = 1_024 * 1_024;

/// Maximum number of grep matches to return.
const MAX_GREP_MATCHES: usize = 100;

/// Size of the buffer used for binary detection (8 KB).
const BINARY_DETECT_SIZE: usize = 8 * 1024;

/// File traversal policy. Defaults are safe for MCP exposure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkOptions {
    pub include_hidden: bool,
    pub include_sensitive: bool,
}

fn is_hidden_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(name) => {
            name.to_str().is_some_and(|name| name.starts_with('.'))
        }
        _ => false,
    })
}

fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const SENSITIVE_NAMES: &[&str] = &[
        ".env",
        ".npmrc",
        ".pypirc",
        ".netrc",
        ".aws",
        ".ssh",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];
    const SENSITIVE_EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx"];

    SENSITIVE_NAMES
        .iter()
        .any(|sensitive| lower == *sensitive || lower.starts_with(&format!("{sensitive}.")))
        || SENSITIVE_EXTENSIONS
            .iter()
            .any(|extension| lower.ends_with(extension) || lower.contains(&format!("{extension}.")))
}

fn is_sensitive_path(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(name) => name.to_str().is_some_and(is_sensitive_name),
        _ => false,
    })
}

pub fn path_allowed_by_options(path: &Path, options: WalkOptions) -> bool {
    (options.include_hidden || !is_hidden_path(path))
        && (options.include_sensitive || !is_sensitive_path(path))
}

pub fn path_allowed_for_shared_ai_factory(path: &Path, options: WalkOptions) -> bool {
    if path_allowed_by_options(path, options) {
        return true;
    }

    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };

    first == ".ai-factory" && (options.include_sensitive || !is_sensitive_path(path))
}

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

fn normalize_rel_path(input: &str) -> Option<PathBuf> {
    if input.trim().is_empty() || input.split(['/', '\\']).any(|part| part.is_empty()) {
        return None;
    }

    let path = Path::new(input);
    if path.is_absolute() {
        return None;
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return None,
        }
    }

    if normalized.as_os_str().is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn build_glob_matcher(glob_pattern: Option<&str>) -> Result<Option<ignore::types::Types>, String> {
    if let Some(glob) = glob_pattern {
        let mut types_builder = ignore::types::TypesBuilder::new();
        types_builder
            .add("custom", glob)
            .map_err(|e| format!("Invalid glob pattern: {}", e))?;
        types_builder.select("custom");
        Ok(Some(
            types_builder
                .build()
                .map_err(|e| format!("Glob build error: {}", e))?,
        ))
    } else {
        Ok(None)
    }
}

fn glob_allows_file(
    entry_path: &Path,
    glob_pattern: Option<&str>,
    glob_matcher: Option<&ignore::types::Types>,
) -> bool {
    let Some(types) = glob_matcher else {
        return true;
    };
    let matched = types.matched(entry_path, false);
    !(matched.is_ignore() || (!matched.is_whitelist() && glob_pattern.is_some()))
}

fn scan_grep_file(
    root: &Path,
    entry_path: &Path,
    re: &regex::Regex,
    glob_pattern: Option<&str>,
    glob_matcher: Option<&ignore::types::Types>,
    matches: &mut Vec<GrepMatch>,
    seen: &mut HashSet<(String, usize, String)>,
) {
    if matches.len() >= MAX_GREP_MATCHES {
        return;
    }

    if !glob_allows_file(entry_path, glob_pattern, glob_matcher) {
        return;
    }

    let Ok(meta) = std::fs::metadata(entry_path) else {
        return;
    };
    if meta.len() > MAX_GREP_FILE_SIZE {
        debug!(
            "grep: skipping large file {} ({} bytes)",
            entry_path.display(),
            meta.len()
        );
        return;
    }

    if is_binary(entry_path) {
        debug!("grep: skipping binary file {}", entry_path.display());
        return;
    }

    let Ok(file) = std::fs::File::open(entry_path) else {
        return;
    };

    let rel = match entry_path.strip_prefix(root) {
        Ok(r) => path_to_slash_string(r),
        Err(_) => return,
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
            let line_number = line_idx + 1;
            let key = (rel.clone(), line_number, line.clone());
            if seen.insert(key) {
                debug!("grep match: {}:{}", rel, line_number);
                matches.push(GrepMatch {
                    path: rel.clone(),
                    line_number,
                    line_content: line,
                });
            }
        }
    }
}

fn canonical_grep_candidate_allowed(
    canonical_root: &Path,
    canonical_scope_root: &Path,
    candidate_path: &Path,
    options: WalkOptions,
) -> bool {
    let Ok(canonical_candidate) = candidate_path.canonicalize() else {
        warn!(
            "grep: skipping candidate that cannot canonicalize: {}",
            candidate_path.display()
        );
        return false;
    };

    if !canonical_candidate.starts_with(canonical_root) {
        warn!(
            "grep: skipping candidate outside project root: {}",
            candidate_path.display()
        );
        return false;
    }

    if !canonical_candidate.starts_with(canonical_scope_root) {
        warn!(
            "grep: skipping candidate outside shared scope: {}",
            candidate_path.display()
        );
        return false;
    }

    let rel = canonical_candidate
        .strip_prefix(canonical_root)
        .unwrap_or(canonical_candidate.as_path());
    if !path_allowed_by_options(rel, options) {
        warn!(
            "grep: skipping candidate blocked by path policy: {}",
            candidate_path.display()
        );
        return false;
    }

    true
}

/// Walk the project file tree, respecting .gitignore rules.
///
/// If `subpath` is provided, only entries under that subdirectory are returned.
/// If `max_depth` is provided, limits traversal depth (1 = immediate children only).
/// Paths in the result are relative to `root`.
pub fn walk_project_tree(
    root: &Path,
    subpath: Option<&str>,
    max_depth: Option<usize>,
    options: WalkOptions,
) -> Vec<FileEntry> {
    if let Some(sub) = subpath
        && !path_allowed_by_options(Path::new(sub), options)
    {
        warn!("Walk subpath blocked by policy: {}", sub);
        return Vec::new();
    }

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

    let mut builder = ignore::WalkBuilder::new(&walk_root);
    builder
        .hidden(!options.include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .sort_by_file_name(|a, b| a.cmp(b));

    let policy_root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        let path = entry.path();
        let rel = path.strip_prefix(&policy_root).unwrap_or(path);
        path_allowed_by_options(rel, options)
    });

    if let Some(depth) = max_depth {
        builder.max_depth(Some(depth));
    }

    let walker = builder.build();

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

        let rel_str = path_to_slash_string(rel);
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
    options: WalkOptions,
) -> Result<Vec<GrepMatch>, String> {
    info!(
        "grep_project: root={}, pattern={}, glob={:?}",
        root.display(),
        pattern,
        glob_pattern
    );

    let re = regex::Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;
    let glob_matcher = build_glob_matcher(glob_pattern)?;

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(!options.include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);

    let policy_root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        let path = entry.path();
        let rel = path.strip_prefix(&policy_root).unwrap_or(path);
        path_allowed_by_options(rel, options)
    });

    let walker = builder.build();
    let mut matches = Vec::new();
    let mut seen = HashSet::new();

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

        scan_grep_file(
            root,
            entry_path,
            &re,
            glob_pattern,
            glob_matcher.as_ref(),
            &mut matches,
            &mut seen,
        );
    }

    info!("grep_project: found {} matches", matches.len());
    Ok(matches)
}

/// Grep only the provided project-relative file or directory paths.
///
/// This is used by MCP shared-scope access control so grep never opens
/// unshared files in default mode.
pub fn grep_project_paths(
    root: &Path,
    scopes: &[GrepScope],
    pattern: &str,
    glob_pattern: Option<&str>,
    options: WalkOptions,
) -> Result<Vec<GrepMatch>, String> {
    info!(
        "grep_project_paths: root={}, scopes={}, pattern={}, glob={:?}",
        root.display(),
        scopes.len(),
        pattern,
        glob_pattern
    );

    let re = regex::Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;
    let glob_matcher = build_glob_matcher(glob_pattern)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve project path: {}", e))?;

    let mut matches = Vec::new();
    let mut seen = HashSet::new();

    for scope in scopes {
        if matches.len() >= MAX_GREP_MATCHES {
            debug!(
                "grep_project_paths: hit max matches limit ({})",
                MAX_GREP_MATCHES
            );
            break;
        }

        let Some(normalized) = normalize_rel_path(&scope.rel_path) else {
            warn!(
                "grep_project_paths: skipping invalid scope '{}'",
                scope.rel_path
            );
            continue;
        };

        if !path_allowed_by_options(&normalized, options) {
            warn!(
                "grep_project_paths: skipping blocked scope '{}'",
                scope.rel_path
            );
            continue;
        }

        let target = canonical_root.join(normalized);
        let Ok(canonical_target) = target.canonicalize() else {
            warn!(
                "grep_project_paths: skipping missing scope '{}'",
                scope.rel_path
            );
            continue;
        };

        if !canonical_target.starts_with(&canonical_root) {
            warn!(
                "grep_project_paths: skipping out-of-root scope '{}'",
                scope.rel_path
            );
            continue;
        }

        let canonical_target_rel = canonical_target
            .strip_prefix(&canonical_root)
            .unwrap_or(canonical_target.as_path());
        if !path_allowed_by_options(canonical_target_rel, options) {
            warn!(
                "grep_project_paths: skipping blocked canonical scope '{}'",
                scope.rel_path
            );
            continue;
        }

        match scope.kind {
            GrepScopeKind::File => {
                if canonical_target.is_file() {
                    scan_grep_file(
                        &canonical_root,
                        &canonical_target,
                        &re,
                        glob_pattern,
                        glob_matcher.as_ref(),
                        &mut matches,
                        &mut seen,
                    );
                } else {
                    warn!(
                        "grep_project_paths: skipping file scope that is not a file '{}'",
                        scope.rel_path
                    );
                }
                continue;
            }
            GrepScopeKind::Dir => {
                if !canonical_target.is_dir() {
                    warn!(
                        "grep_project_paths: skipping dir scope that is not a directory '{}'",
                        scope.rel_path
                    );
                    continue;
                }
            }
        }

        let mut builder = ignore::WalkBuilder::new(&canonical_target);
        builder
            .hidden(!options.include_hidden)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true);

        let policy_root = canonical_root.clone();
        builder.filter_entry(move |entry| {
            let path = entry.path();
            let rel = path.strip_prefix(&policy_root).unwrap_or(path);
            path_allowed_by_options(rel, options)
        });

        for result in builder.build() {
            if matches.len() >= MAX_GREP_MATCHES {
                break;
            }

            let Ok(entry) = result else {
                continue;
            };

            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
                continue;
            }

            if !canonical_grep_candidate_allowed(
                &canonical_root,
                &canonical_target,
                entry.path(),
                options,
            ) {
                continue;
            }

            scan_grep_file(
                &canonical_root,
                entry.path(),
                &re,
                glob_pattern,
                glob_matcher.as_ref(),
                &mut matches,
                &mut seen,
            );
        }
    }

    debug!("grep_project_paths: found {} matches", matches.len());
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_slash_string_uses_forward_slashes() {
        let path = PathBuf::from("src").join("lib.rs");

        assert_eq!(path_to_slash_string(&path), "src/lib.rs");
    }

    #[test]
    fn path_policy_blocks_hidden_and_sensitive_by_default() {
        let options = WalkOptions::default();

        assert!(path_allowed_by_options(Path::new("src/main.rs"), options));
        assert!(!path_allowed_by_options(Path::new(".hidden.txt"), options));
        assert!(!path_allowed_by_options(Path::new(".env"), options));
        assert!(!path_allowed_by_options(Path::new(".env.md"), options));
        assert!(!path_allowed_by_options(Path::new("private.key"), options));
        assert!(!path_allowed_by_options(
            Path::new("private.key.md"),
            options
        ));
        assert!(!path_allowed_by_options(Path::new("id_rsa.md"), options));
        assert!(!path_allowed_by_options(Path::new(".ssh/id_rsa"), options));
    }

    #[test]
    fn path_policy_separates_hidden_and_sensitive_opt_ins() {
        let include_hidden = WalkOptions {
            include_hidden: true,
            include_sensitive: false,
        };
        let include_sensitive = WalkOptions {
            include_hidden: false,
            include_sensitive: true,
        };
        let include_both = WalkOptions {
            include_hidden: true,
            include_sensitive: true,
        };

        assert!(path_allowed_by_options(
            Path::new(".hidden.txt"),
            include_hidden
        ));
        assert!(!path_allowed_by_options(Path::new(".env"), include_hidden));
        assert!(path_allowed_by_options(
            Path::new("private.key"),
            include_sensitive
        ));
        assert!(!path_allowed_by_options(
            Path::new(".ssh/id_rsa"),
            include_sensitive
        ));
        assert!(path_allowed_by_options(Path::new(".env"), include_both));
        assert!(path_allowed_by_options(
            Path::new(".ssh/id_rsa"),
            include_both
        ));
    }
}
