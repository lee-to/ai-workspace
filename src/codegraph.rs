//! Rust-only semantic code graph indexing.
//!
//! The MVP intentionally uses a conservative regex parser instead of pulling in
//! a larger parser dependency. It extracts stable Rust symbols and simple call
//! references while keeping all persisted graph access behind the Db layer.

use anyhow::{Context as _, Result};
use log::{debug, error, info, warn};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db::{Db, validate_project_rel_path};
use crate::models::{
    CodeEdge, CodeEdgeKind, CodeFile, CodeNode, CodeNodeKind, CodeReferenceKind, CodeUnresolvedRef,
    Project, SharedItemKind,
};
use crate::path::normalize_portable_rel_path;
use crate::walk::{self, WalkOptions, walk_project_tree};

const LANGUAGE_RUST: &str = "rust";
const MAX_CODE_FILE_SIZE: u64 = 1_024 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeGraphScope {
    SharedOnly,
    FullProject,
}

impl CodeGraphScope {
    pub fn is_full_project(self) -> bool {
        matches!(self, Self::FullProject)
    }
}

#[derive(Debug, Default, Clone)]
pub struct CodeGraphRunStats {
    pub scanned_files: usize,
    pub indexed_files: usize,
    pub skipped_unchanged: usize,
    pub skipped_policy: usize,
    pub skipped_oversized: usize,
    pub removed_files: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub unresolved_ref_count: usize,
    pub resolved_ref_count: usize,
}

#[derive(Debug, Clone)]
struct CandidateFile {
    rel_path: String,
    abs_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ParsedRustFile {
    file: CodeFile,
    nodes: Vec<CodeNode>,
    edges: Vec<CodeEdge>,
    refs: Vec<CodeUnresolvedRef>,
}

#[derive(Debug, Clone)]
struct Container {
    node_id: String,
    node_index: usize,
    kind: CodeNodeKind,
    display_name: String,
    base_depth: i64,
}

#[derive(Debug)]
struct RustPatterns {
    use_re: Regex,
    mod_re: Regex,
    struct_re: Regex,
    enum_re: Regex,
    trait_re: Regex,
    impl_re: Regex,
    fn_re: Regex,
    const_re: Regex,
    type_re: Regex,
    call_re: Regex,
}

impl RustPatterns {
    fn new() -> Result<Self> {
        Ok(Self {
            use_re: Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);")?,
            mod_re: Regex::new(
                r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:;|\{)",
            )?,
            struct_re: Regex::new(
                r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)",
            )?,
            enum_re: Regex::new(r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)")?,
            trait_re: Regex::new(
                r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)",
            )?,
            impl_re: Regex::new(r"^\s*impl(?:<[^>]+>)?\s+(.+?)\s*\{")?,
            fn_re: Regex::new(
                r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*([^;{]*)",
            )?,
            const_re: Regex::new(
                r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?(?:const|static)\s+([A-Z_A-Za-z][A-Za-z0-9_]*)",
            )?,
            type_re: Regex::new(r"^\s*(?:(pub(?:\([^)]*\))?)\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)")?,
            call_re: Regex::new(
                r"\b([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\s*(?:!|\()",
            )?,
        })
    }
}

pub fn reindex_project(
    db: &Db,
    project: &Project,
    scope: CodeGraphScope,
) -> Result<CodeGraphRunStats> {
    info!(
        "codegraph reindex start: project_id={} slug={} scope={:?}",
        project.id, project.slug, scope
    );
    db.clear_code_graph_project(project.id)
        .context("codegraph reindex cleanup failed")?;
    sync_project_inner(db, project, scope, true)
}

pub fn sync_project(
    db: &Db,
    project: &Project,
    scope: CodeGraphScope,
) -> Result<CodeGraphRunStats> {
    info!(
        "codegraph sync start: project_id={} slug={} scope={:?}",
        project.id, project.slug, scope
    );
    sync_project_inner(db, project, scope, false)
}

fn sync_project_inner(
    db: &Db,
    project: &Project,
    scope: CodeGraphScope,
    force: bool,
) -> Result<CodeGraphRunStats> {
    let mut stats = CodeGraphRunStats::default();
    let candidates = collect_candidate_files(db, project, scope, &mut stats)?;
    let candidate_paths = candidates
        .iter()
        .map(|candidate| candidate.rel_path.clone())
        .collect::<BTreeSet<_>>();

    if !force {
        for indexed_path in db.list_code_file_paths(project.id)? {
            if !candidate_paths.contains(&indexed_path) {
                db.delete_code_graph_file(project.id, &indexed_path)
                    .with_context(|| {
                        format!(
                            "codegraph cleanup failed for project_id={} path='{}'",
                            project.id, indexed_path
                        )
                    })?;
                stats.removed_files += 1;
            }
        }
    }

    let changed_paths = candidates
        .iter()
        .map(|candidate| candidate.rel_path.clone())
        .collect::<HashSet<_>>();
    let existing_nodes = db
        .list_code_nodes_for_project(project.id)?
        .into_iter()
        .filter(|node| !changed_paths.contains(&node.file_path))
        .collect::<Vec<_>>();

    let patterns = RustPatterns::new()?;
    let mut parsed_files = Vec::new();
    for candidate in candidates {
        stats.scanned_files += 1;
        debug!(
            "codegraph parse candidate start: project_id={} path={}",
            project.id, candidate.rel_path
        );
        match parse_candidate(db, project, &candidate, &patterns, force) {
            Ok(ParseDecision::Unchanged) => {
                stats.skipped_unchanged += 1;
                debug!(
                    "codegraph incremental skip unchanged: project_id={} path={}",
                    project.id, candidate.rel_path
                );
            }
            Ok(ParseDecision::SkippedPolicy) => {
                stats.skipped_policy += 1;
            }
            Ok(ParseDecision::SkippedOversized) => {
                stats.skipped_oversized += 1;
            }
            Ok(ParseDecision::Parsed(parsed)) => {
                debug!(
                    "codegraph parse candidate complete: path={} nodes={} refs={}",
                    parsed.file.path,
                    parsed.nodes.len(),
                    parsed.refs.len()
                );
                parsed_files.push(*parsed);
            }
            Err(err) => {
                error!(
                    "codegraph extraction failed: project_id={} path={} error={}",
                    project.id, candidate.rel_path, err
                );
                return Err(err).context("codegraph extract phase failed");
            }
        }
    }

    resolve_references(&existing_nodes, &mut parsed_files, &mut stats);

    for parsed in parsed_files {
        stats.indexed_files += 1;
        stats.node_count += parsed.nodes.len();
        stats.edge_count += parsed.edges.len();
        stats.unresolved_ref_count += parsed.refs.len();
        db.replace_code_graph_file(&parsed.file, &parsed.nodes, &parsed.edges, &parsed.refs)
            .with_context(|| {
                format!(
                    "codegraph write transaction failed for project_id={} path='{}'",
                    parsed.file.project_id, parsed.file.path
                )
            })?;
    }

    info!(
        "codegraph sync complete: project_id={} scanned={} indexed={} skipped_unchanged={} removed={} nodes={} edges={} unresolved={} resolved={}",
        project.id,
        stats.scanned_files,
        stats.indexed_files,
        stats.skipped_unchanged,
        stats.removed_files,
        stats.node_count,
        stats.edge_count,
        stats.unresolved_ref_count,
        stats.resolved_ref_count
    );
    Ok(stats)
}

enum ParseDecision {
    Parsed(Box<ParsedRustFile>),
    Unchanged,
    SkippedPolicy,
    SkippedOversized,
}

fn parse_candidate(
    db: &Db,
    project: &Project,
    candidate: &CandidateFile,
    patterns: &RustPatterns,
    force: bool,
) -> Result<ParseDecision> {
    if !walk::path_allowed_by_options(Path::new(&candidate.rel_path), WalkOptions::default()) {
        warn!(
            "codegraph skipping path blocked by policy: project_id={} path={}",
            project.id, candidate.rel_path
        );
        return Ok(ParseDecision::SkippedPolicy);
    }

    let meta = std::fs::metadata(&candidate.abs_path)
        .with_context(|| format!("Failed to stat {}", candidate.abs_path.display()))?;
    if meta.len() > MAX_CODE_FILE_SIZE {
        warn!(
            "codegraph skipping oversized file: project_id={} path={} size={}",
            project.id,
            candidate.rel_path,
            meta.len()
        );
        db.delete_code_graph_file(project.id, &candidate.rel_path)?;
        return Ok(ParseDecision::SkippedOversized);
    }
    let content = match std::fs::read_to_string(&candidate.abs_path) {
        Ok(content) => content,
        Err(err) => {
            warn!(
                "codegraph skipping non-UTF-8 or unreadable file: project_id={} path={} error={}",
                project.id, candidate.rel_path, err
            );
            db.delete_code_graph_file(project.id, &candidate.rel_path)?;
            return Ok(ParseDecision::SkippedPolicy);
        }
    };
    let content_hash = stable_hash_hex(content.as_bytes());
    let size = meta.len() as i64;
    let mtime = mtime_epoch(&meta);
    if !force
        && db
            .get_code_file_meta(project.id, &candidate.rel_path)?
            .is_some_and(|(hash, old_size, old_mtime)| {
                hash == content_hash && old_size == size && old_mtime == mtime
            })
    {
        return Ok(ParseDecision::Unchanged);
    }

    let parsed = parse_rust_source(
        project.id,
        &candidate.rel_path,
        &content,
        content_hash,
        size,
        mtime,
        patterns,
    )?;
    Ok(ParseDecision::Parsed(Box::new(parsed)))
}

fn collect_candidate_files(
    db: &Db,
    project: &Project,
    scope: CodeGraphScope,
    stats: &mut CodeGraphRunStats,
) -> Result<Vec<CandidateFile>> {
    let project_root = Path::new(&project.path);
    let canonical_root = project_root.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize project root for codegraph: {}",
            project_root.display()
        )
    })?;
    let mut candidates = BTreeMap::<String, CandidateFile>::new();

    if scope.is_full_project() {
        warn!(
            "codegraph full-project mode enabled: project_id={} path={}",
            project.id, project.path
        );
        for entry in walk_project_tree(&canonical_root, None, None, WalkOptions::default()) {
            if entry.is_dir || !is_rust_path(Path::new(&entry.path)) {
                continue;
            }
            if !walk::path_allowed_by_options(Path::new(&entry.path), WalkOptions::default()) {
                stats.skipped_policy += 1;
                continue;
            }
            candidates.insert(
                entry.path.clone(),
                CandidateFile {
                    abs_path: canonical_root.join(&entry.path),
                    rel_path: entry.path,
                },
            );
        }
        return Ok(candidates.into_values().collect());
    }

    for item in db.get_shared_items_for_project(project.id)? {
        match item.kind {
            SharedItemKind::File => {
                let Some(path) = item.path.as_deref() else {
                    continue;
                };
                let normalized = match normalize_portable_rel_path(path) {
                    Ok(path) => path,
                    Err(err) => {
                        warn!(
                            "codegraph skipping invalid shared file path '{}': {}",
                            path, err
                        );
                        stats.skipped_policy += 1;
                        continue;
                    }
                };
                if !is_rust_path(Path::new(&normalized)) {
                    continue;
                }
                if !walk::path_allowed_by_options(Path::new(&normalized), WalkOptions::default()) {
                    warn!(
                        "codegraph skipping blocked shared file path '{}'",
                        normalized
                    );
                    stats.skipped_policy += 1;
                    continue;
                }
                match validate_project_rel_path(project_root, &normalized) {
                    Ok(validated) => {
                        candidates.insert(
                            validated.rel_path.clone(),
                            CandidateFile {
                                rel_path: validated.rel_path,
                                abs_path: validated.canonical_path,
                            },
                        );
                    }
                    Err(err) => {
                        warn!(
                            "codegraph skipping shared file outside project '{}': {}",
                            normalized, err
                        );
                        stats.skipped_policy += 1;
                    }
                }
            }
            SharedItemKind::Dir => {
                let Some(path) = item.path.as_deref() else {
                    continue;
                };
                let validated = match validate_project_rel_path(project_root, path) {
                    Ok(validated) => validated,
                    Err(err) => {
                        warn!(
                            "codegraph skipping shared dir outside project '{}': {}",
                            path, err
                        );
                        stats.skipped_policy += 1;
                        continue;
                    }
                };
                if !walk::path_allowed_by_options(
                    Path::new(&validated.rel_path),
                    WalkOptions::default(),
                ) {
                    warn!(
                        "codegraph skipping blocked shared dir path '{}'",
                        validated.rel_path
                    );
                    stats.skipped_policy += 1;
                    continue;
                }
                for entry in walk_project_tree(
                    &canonical_root,
                    Some(&validated.rel_path),
                    None,
                    WalkOptions::default(),
                ) {
                    if entry.is_dir || !is_rust_path(Path::new(&entry.path)) {
                        continue;
                    }
                    if !walk::path_allowed_by_options(
                        Path::new(&entry.path),
                        WalkOptions::default(),
                    ) {
                        stats.skipped_policy += 1;
                        continue;
                    }
                    let Ok(child) = validate_project_rel_path(project_root, &entry.path) else {
                        stats.skipped_policy += 1;
                        continue;
                    };
                    candidates.insert(
                        child.rel_path.clone(),
                        CandidateFile {
                            rel_path: child.rel_path,
                            abs_path: child.canonical_path,
                        },
                    );
                }
            }
            SharedItemKind::Note => {}
        }
    }

    Ok(candidates.into_values().collect())
}

fn parse_rust_source(
    project_id: i64,
    rel_path: &str,
    content: &str,
    content_hash: String,
    size: i64,
    mtime: i64,
    patterns: &RustPatterns,
) -> Result<ParsedRustFile> {
    debug!(
        "codegraph rust parse start: project_id={} path={}",
        project_id, rel_path
    );
    let module_path = module_path_from_rel_path(rel_path);
    let file_node_id = stable_node_id(project_id, rel_path, CodeNodeKind::File, &module_path, 1);
    let total_lines = content.lines().count().max(1) as i64;
    let mut nodes = vec![CodeNode {
        stable_id: file_node_id.clone(),
        project_id,
        kind: CodeNodeKind::File,
        name: Path::new(rel_path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| rel_path.to_string()),
        qualified_name: module_path.clone(),
        file_path: rel_path.to_string(),
        language: LANGUAGE_RUST.to_string(),
        start_line: 1,
        start_column: 1,
        end_line: total_lines,
        end_column: 1,
        docstring: None,
        signature: None,
        visibility: None,
        flags_json: None,
    }];
    let mut edges = Vec::new();
    let mut refs = Vec::new();
    let mut stack = Vec::<Container>::new();
    let mut brace_depth = 0_i64;
    let mut pending_doc = Vec::<String>::new();

    for (idx, line) in content.lines().enumerate() {
        let line_number = idx as i64 + 1;
        while stack
            .last()
            .is_some_and(|container| brace_depth <= container.base_depth)
        {
            let container = stack.pop().expect("stack checked by last");
            nodes[container.node_index].end_line =
                (line_number - 1).max(nodes[container.node_index].start_line);
        }

        let trimmed = line.trim();
        if let Some(doc) = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
        {
            pending_doc.push(doc.trim().to_string());
            brace_depth += brace_delta(line);
            continue;
        }
        if trimmed.starts_with("#[") || trimmed.is_empty() {
            brace_depth += brace_delta(line);
            continue;
        }

        let active_function = stack
            .iter()
            .rev()
            .find(|container| {
                matches!(
                    container.kind,
                    CodeNodeKind::Function | CodeNodeKind::Method
                )
            })
            .map(|container| container.node_id.clone());

        let mut declaration_line = false;
        if let Some(captures) = patterns.use_re.captures(line) {
            declaration_line = true;
            let path = captures.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            let name = import_name(path);
            let qualified_name = format!("{module_path}::use::{line_number}");
            add_node(
                &mut nodes,
                &mut edges,
                project_id,
                rel_path,
                CodeNodeKind::Import,
                name,
                qualified_name,
                line_number,
                line,
                take_doc(&mut pending_doc),
                visibility_from(captures.get(0).map(|m| m.as_str()).unwrap_or("")),
                stack
                    .last()
                    .map(|container| container.node_id.as_str())
                    .unwrap_or(&file_node_id),
            );
            refs.push(CodeUnresolvedRef {
                id: None,
                project_id,
                source_node_id: file_node_id.clone(),
                file_path: rel_path.to_string(),
                ref_name: path.to_string(),
                kind: CodeReferenceKind::Imports,
                line: line_number,
                column: captures.get(1).map(|m| m.start() as i64 + 1).unwrap_or(1),
                metadata_json: None,
            });
        } else if let Some(captures) = patterns.impl_re.captures(line) {
            declaration_line = true;
            let target = captures
                .get(1)
                .map(|m| clean_type_name(m.as_str()))
                .unwrap_or_else(|| "impl".to_string());
            let name = format!("impl {target}");
            let qualified_name = format!("{module_path}::{name}@{line_number}");
            let (node_id, node_index) = add_node(
                &mut nodes,
                &mut edges,
                project_id,
                rel_path,
                CodeNodeKind::Impl,
                name.clone(),
                qualified_name,
                line_number,
                line,
                take_doc(&mut pending_doc),
                None,
                stack
                    .last()
                    .map(|container| container.node_id.as_str())
                    .unwrap_or(&file_node_id),
            );
            if line.contains('{') {
                stack.push(Container {
                    node_id,
                    node_index,
                    kind: CodeNodeKind::Impl,
                    display_name: target,
                    base_depth: brace_depth,
                });
            }
        } else if let Some(captures) = patterns.fn_re.captures(line) {
            declaration_line = true;
            let name = captures.get(2).map(|m| m.as_str()).unwrap_or("fn");
            let parent_type = stack
                .iter()
                .rev()
                .find(|container| {
                    matches!(container.kind, CodeNodeKind::Impl | CodeNodeKind::Trait)
                })
                .map(|container| container.display_name.clone());
            let kind = if parent_type.is_some() {
                CodeNodeKind::Method
            } else {
                CodeNodeKind::Function
            };
            let qualified_name = if let Some(parent_type) = parent_type {
                format!("{module_path}::{parent_type}::{name}")
            } else {
                format!("{module_path}::{name}")
            };
            let (node_id, node_index) = add_node(
                &mut nodes,
                &mut edges,
                project_id,
                rel_path,
                kind,
                name.to_string(),
                qualified_name,
                line_number,
                line,
                take_doc(&mut pending_doc),
                visibility_from(captures.get(1).map(|m| m.as_str()).unwrap_or("")),
                stack
                    .last()
                    .map(|container| container.node_id.as_str())
                    .unwrap_or(&file_node_id),
            );
            if line.contains('{') && !trimmed.ends_with(';') {
                stack.push(Container {
                    node_id,
                    node_index,
                    kind,
                    display_name: name.to_string(),
                    base_depth: brace_depth,
                });
            }
        } else if let Some((kind, captures)) = [
            (CodeNodeKind::Module, patterns.mod_re.captures(line)),
            (CodeNodeKind::Struct, patterns.struct_re.captures(line)),
            (CodeNodeKind::Enum, patterns.enum_re.captures(line)),
            (CodeNodeKind::Trait, patterns.trait_re.captures(line)),
            (CodeNodeKind::Const, patterns.const_re.captures(line)),
            (CodeNodeKind::TypeAlias, patterns.type_re.captures(line)),
        ]
        .into_iter()
        .find_map(|(kind, captures)| captures.map(|captures| (kind, captures)))
        {
            declaration_line = true;
            let name = captures.get(2).map(|m| m.as_str()).unwrap_or("item");
            let qualified_name = format!("{module_path}::{name}");
            let (node_id, node_index) = add_node(
                &mut nodes,
                &mut edges,
                project_id,
                rel_path,
                kind,
                name.to_string(),
                qualified_name,
                line_number,
                line,
                take_doc(&mut pending_doc),
                visibility_from(captures.get(1).map(|m| m.as_str()).unwrap_or("")),
                stack
                    .last()
                    .map(|container| container.node_id.as_str())
                    .unwrap_or(&file_node_id),
            );
            if line.contains('{')
                && matches!(
                    kind,
                    CodeNodeKind::Module
                        | CodeNodeKind::Struct
                        | CodeNodeKind::Enum
                        | CodeNodeKind::Trait
                )
            {
                stack.push(Container {
                    node_id,
                    node_index,
                    kind,
                    display_name: name.to_string(),
                    base_depth: brace_depth,
                });
            }
        } else {
            pending_doc.clear();
        }

        if !declaration_line && let Some(source_node_id) = active_function {
            extract_call_refs(
                &mut refs,
                project_id,
                rel_path,
                &source_node_id,
                line,
                line_number,
                &patterns.call_re,
            );
        }

        brace_depth += brace_delta(line);
    }

    while let Some(container) = stack.pop() {
        nodes[container.node_index].end_line = total_lines;
    }

    let file = CodeFile {
        project_id,
        path: rel_path.to_string(),
        language: LANGUAGE_RUST.to_string(),
        content_hash,
        size,
        mtime,
        indexed_at: String::new(),
        node_count: nodes.len() as i64,
        errors_json: None,
    };

    debug!(
        "codegraph rust parse end: project_id={} path={} nodes={} edges={} refs={}",
        project_id,
        rel_path,
        nodes.len(),
        edges.len(),
        refs.len()
    );
    Ok(ParsedRustFile {
        file,
        nodes,
        edges,
        refs,
    })
}

#[allow(clippy::too_many_arguments)]
fn add_node(
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    project_id: i64,
    rel_path: &str,
    kind: CodeNodeKind,
    name: String,
    qualified_name: String,
    line_number: i64,
    source_line: &str,
    docstring: Option<String>,
    visibility: Option<String>,
    parent_id: &str,
) -> (String, usize) {
    let stable_id = stable_node_id(project_id, rel_path, kind, &qualified_name, line_number);
    let node_index = nodes.len();
    nodes.push(CodeNode {
        stable_id: stable_id.clone(),
        project_id,
        kind,
        name,
        qualified_name,
        file_path: rel_path.to_string(),
        language: LANGUAGE_RUST.to_string(),
        start_line: line_number,
        start_column: first_non_ws_column(source_line),
        end_line: line_number,
        end_column: source_line.len() as i64 + 1,
        docstring,
        signature: Some(signature_from_line(source_line)),
        visibility,
        flags_json: None,
    });
    edges.push(CodeEdge {
        id: None,
        project_id,
        source_node_id: parent_id.to_string(),
        target_node_id: stable_id.clone(),
        kind: CodeEdgeKind::Contains,
        line: Some(line_number),
        column: Some(1),
        metadata_json: None,
        provenance: "rust-regex-mvp".to_string(),
    });
    (stable_id, node_index)
}

fn resolve_references(
    existing_nodes: &[CodeNode],
    parsed_files: &mut [ParsedRustFile],
    stats: &mut CodeGraphRunStats,
) {
    let mut all_nodes = existing_nodes.to_vec();
    for parsed in parsed_files.iter() {
        all_nodes.extend(parsed.nodes.iter().cloned());
    }

    let mut by_name: HashMap<String, Vec<CodeNode>> = HashMap::new();
    let mut by_qualified: HashMap<String, Vec<CodeNode>> = HashMap::new();
    for node in all_nodes.into_iter().filter(|node| {
        !matches!(
            node.kind,
            CodeNodeKind::File | CodeNodeKind::Import | CodeNodeKind::Impl
        )
    }) {
        by_name
            .entry(node.name.clone())
            .or_default()
            .push(node.clone());
        by_qualified
            .entry(node.qualified_name.clone())
            .or_default()
            .push(node);
    }

    for parsed in parsed_files {
        let refs = std::mem::take(&mut parsed.refs);
        for unresolved in refs {
            match resolve_one_ref(&unresolved, &by_name, &by_qualified) {
                Some(target_node_id) => {
                    parsed.edges.push(CodeEdge {
                        id: None,
                        project_id: unresolved.project_id,
                        source_node_id: unresolved.source_node_id,
                        target_node_id,
                        kind: match unresolved.kind {
                            CodeReferenceKind::Calls => CodeEdgeKind::Calls,
                            CodeReferenceKind::Imports => CodeEdgeKind::Imports,
                            CodeReferenceKind::References => CodeEdgeKind::References,
                        },
                        line: Some(unresolved.line),
                        column: Some(unresolved.column),
                        metadata_json: unresolved.metadata_json,
                        provenance: "rust-regex-mvp-resolver".to_string(),
                    });
                    stats.resolved_ref_count += 1;
                }
                None => parsed.refs.push(unresolved),
            }
        }
    }
}

fn resolve_one_ref(
    reference: &CodeUnresolvedRef,
    by_name: &HashMap<String, Vec<CodeNode>>,
    by_qualified: &HashMap<String, Vec<CodeNode>>,
) -> Option<String> {
    let ref_name = reference.ref_name.trim();
    if ref_name.is_empty() {
        return None;
    }

    let mut candidates = Vec::<CodeNode>::new();
    if let Some(exact) = by_qualified.get(ref_name) {
        candidates.extend(exact.iter().cloned());
    }
    if ref_name.contains("::") {
        let suffix = format!("::{ref_name}");
        candidates.extend(
            by_qualified
                .values()
                .flat_map(|nodes| nodes.iter())
                .filter(|node| node.qualified_name.ends_with(&suffix))
                .cloned(),
        );
    }
    let short = ref_name.rsplit("::").next().unwrap_or(ref_name);
    if let Some(named) = by_name.get(short) {
        candidates.extend(named.iter().cloned());
    }

    candidates.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    candidates.dedup_by(|left, right| left.stable_id == right.stable_id);
    candidates.retain(|node| node.stable_id != reference.source_node_id);
    if candidates.len() == 1 {
        return candidates.first().map(|node| node.stable_id.clone());
    }
    if candidates.len() > 1 {
        warn!(
            "codegraph ambiguous reference: project_id={} file={} ref={} candidates={}",
            reference.project_id,
            reference.file_path,
            reference.ref_name,
            candidates.len()
        );
    }
    None
}

fn extract_call_refs(
    refs: &mut Vec<CodeUnresolvedRef>,
    project_id: i64,
    rel_path: &str,
    source_node_id: &str,
    line: &str,
    line_number: i64,
    call_re: &Regex,
) {
    for captures in call_re.captures_iter(line) {
        let Some(name_match) = captures.get(1) else {
            continue;
        };
        let name = name_match.as_str();
        if is_ignored_call_token(name) {
            continue;
        }
        refs.push(CodeUnresolvedRef {
            id: None,
            project_id,
            source_node_id: source_node_id.to_string(),
            file_path: rel_path.to_string(),
            ref_name: name.to_string(),
            kind: CodeReferenceKind::Calls,
            line: line_number,
            column: name_match.start() as i64 + 1,
            metadata_json: None,
        });
    }
}

fn is_ignored_call_token(name: &str) -> bool {
    matches!(
        name,
        "if" | "while"
            | "for"
            | "loop"
            | "match"
            | "return"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "vec"
            | "format"
            | "println"
            | "eprintln"
            | "debug"
            | "info"
            | "warn"
            | "error"
    )
}

pub fn source_snippet(project: &Project, node: &CodeNode, context_lines: usize) -> Result<String> {
    let validated = validate_project_rel_path(Path::new(&project.path), &node.file_path)?;
    if !walk::path_allowed_by_options(Path::new(&validated.rel_path), WalkOptions::default()) {
        anyhow::bail!("codegraph source snippet blocked by path policy");
    }
    let content = std::fs::read_to_string(&validated.canonical_path).with_context(|| {
        format!(
            "Failed to read source snippet {}",
            validated.canonical_path.display()
        )
    })?;
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(String::new());
    }
    let start = node.start_line.saturating_sub(context_lines as i64).max(1) as usize;
    let end = (node.end_line + context_lines as i64)
        .max(node.start_line)
        .min(lines.len() as i64) as usize;
    let snippet = (start..=end)
        .filter_map(|line_number| {
            lines
                .get(line_number.saturating_sub(1))
                .map(|line| format!("{line_number}: {line}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(snippet)
}

fn is_rust_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn mtime_epoch(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn stable_node_id(
    project_id: i64,
    rel_path: &str,
    kind: CodeNodeKind,
    qualified_name: &str,
    line_number: i64,
) -> String {
    let key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        project_id,
        rel_path,
        kind.as_str(),
        qualified_name,
        line_number
    );
    format!("cg_{}", stable_hash_hex(key.as_bytes()))
}

fn module_path_from_rel_path(rel_path: &str) -> String {
    let without_ext = rel_path.strip_suffix(".rs").unwrap_or(rel_path);
    let mut parts = without_ext
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.first() == Some(&"src") {
        parts.remove(0);
    }
    if matches!(parts.last().copied(), Some("lib" | "main" | "mod")) {
        parts.pop();
    }
    if parts.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", parts.join("::"))
    }
}

fn clean_type_name(input: &str) -> String {
    let mut value = input.trim().trim_end_matches('{').trim();
    if let Some((_, rhs)) = value.rsplit_once(" for ") {
        value = rhs.trim();
    }
    value
        .split_whitespace()
        .next()
        .unwrap_or(value)
        .split('<')
        .next()
        .unwrap_or(value)
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .to_string()
}

fn import_name(path: &str) -> String {
    path.rsplit("::")
        .next()
        .unwrap_or(path)
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '*')
        .to_string()
}

fn visibility_from(input: &str) -> Option<String> {
    if let Some(start) = input.find("pub") {
        let rest = &input[start..];
        let visibility = rest
            .split_whitespace()
            .next()
            .unwrap_or("pub")
            .trim_end_matches('{')
            .to_string();
        return Some(visibility);
    }
    None
}

fn take_doc(pending_doc: &mut Vec<String>) -> Option<String> {
    if pending_doc.is_empty() {
        None
    } else {
        Some(std::mem::take(pending_doc).join("\n"))
    }
}

fn signature_from_line(line: &str) -> String {
    line.split('{')
        .next()
        .unwrap_or(line)
        .trim()
        .trim_end_matches(';')
        .to_string()
}

fn first_non_ws_column(line: &str) -> i64 {
    line.chars()
        .position(|ch| !ch.is_whitespace())
        .map(|idx| idx as i64 + 1)
        .unwrap_or(1)
}

fn brace_delta(line: &str) -> i64 {
    let opens = line.chars().filter(|ch| *ch == '{').count() as i64;
    let closes = line.chars().filter(|ch| *ch == '}').count() as i64;
    opens - closes
}
