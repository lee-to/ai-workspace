use log::{debug, error, info, warn};
use std::path::{Component, Path, PathBuf};

use super::protocol::{JsonRpcResponse, McpError};
use crate::db::Db;
use crate::models::SharedItemKind;
use crate::walk;

const PROJECT_WIDE_TOOLS_ENV: &str = "AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS";
const ACCESS_DENIED_NOT_SHARED: &str = "Access denied: path is not shared";
const ACCESS_DENIED_INVALID_PATH: &str = "Access denied: invalid path";

#[derive(Debug, Clone)]
struct SharedPathScope {
    kind: SharedItemKind,
    rel_path: String,
    canonical_path: Option<PathBuf>,
}

fn project_wide_tools_enabled() -> bool {
    let enabled = std::env::var(PROJECT_WIDE_TOOLS_ENV).ok().as_deref() == Some("1");
    if enabled {
        debug!(
            "{}=1; project-wide MCP tools enabled",
            PROJECT_WIDE_TOOLS_ENV
        );
    }
    enabled
}

fn normalize_rel_path(input: &str) -> Result<String, String> {
    if input.trim().is_empty() {
        return Err(ACCESS_DENIED_INVALID_PATH.to_string());
    }

    if input.split(['/', '\\']).any(|part| part.is_empty()) {
        return Err(ACCESS_DENIED_INVALID_PATH.to_string());
    }

    let path = Path::new(input);
    if path.is_absolute() {
        return Err(ACCESS_DENIED_INVALID_PATH.to_string());
    }

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(ACCESS_DENIED_INVALID_PATH.to_string());
                };
                if part.is_empty() {
                    return Err(ACCESS_DENIED_INVALID_PATH.to_string());
                }
                parts.push(part.to_string());
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ACCESS_DENIED_INVALID_PATH.to_string());
            }
        }
    }

    if parts.is_empty() {
        return Err(ACCESS_DENIED_INVALID_PATH.to_string());
    }

    Ok(parts.join("/"))
}

fn rel_is_same_or_descendant(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn rel_is_ancestor(path: &str, descendant: &str) -> bool {
    descendant
        .strip_prefix(path)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn resolve_project_root(project_id: i64, db: &Db) -> Result<(PathBuf, PathBuf), String> {
    let project = db
        .get_project_by_id(project_id)
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let root = PathBuf::from(&project.path);
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve project path: {}", e))?;

    Ok((root, canonical_root))
}

fn shared_path_scopes(
    project_id: i64,
    canonical_root: &Path,
    db: &Db,
) -> Result<Vec<SharedPathScope>, String> {
    let items = db
        .get_shared_items_for_project(project_id)
        .map_err(|e| format!("Failed to list shared items: {}", e))?;

    let mut scopes = Vec::new();
    for item in items {
        if !matches!(item.kind, SharedItemKind::File | SharedItemKind::Dir) {
            continue;
        }

        let Some(path) = item.path.as_deref() else {
            warn!("shared item {} missing path", item.id);
            continue;
        };

        let rel_path = match normalize_rel_path(path) {
            Ok(path) => path,
            Err(e) => {
                warn!("shared item {} has invalid path '{}': {}", item.id, path, e);
                continue;
            }
        };

        let candidate = canonical_root.join(&rel_path);
        let canonical_path = match candidate.canonicalize() {
            Ok(path) if path.starts_with(canonical_root) => Some(path),
            Ok(path) => {
                warn!(
                    "shared item {} points outside project root: {}",
                    item.id,
                    path.display()
                );
                None
            }
            Err(e) => {
                warn!(
                    "shared item {} path cannot canonicalize '{}': {}",
                    item.id, rel_path, e
                );
                None
            }
        };

        scopes.push(SharedPathScope {
            kind: item.kind,
            rel_path,
            canonical_path,
        });
    }

    debug!(
        "loaded {} shared path scope(s) for project {}",
        scopes.len(),
        project_id
    );
    Ok(scopes)
}

fn find_shared_scope<'a>(
    normalized_target: &str,
    scopes: &'a [SharedPathScope],
) -> Option<&'a SharedPathScope> {
    scopes.iter().find(|scope| match scope.kind {
        SharedItemKind::File => normalized_target == scope.rel_path,
        SharedItemKind::Dir => rel_is_same_or_descendant(normalized_target, &scope.rel_path),
        SharedItemKind::Note => false,
    })
}

fn canonical_path_is_shared(target: &Path, scope: &SharedPathScope) -> bool {
    let Some(scope_path) = scope.canonical_path.as_deref() else {
        return false;
    };

    match scope.kind {
        SharedItemKind::File => target == scope_path,
        SharedItemKind::Dir => target == scope_path || target.starts_with(scope_path),
        SharedItemKind::Note => false,
    }
}

fn tree_path_visible(rel_path: &str, scopes: &[SharedPathScope]) -> bool {
    scopes.iter().any(|scope| match scope.kind {
        SharedItemKind::File => {
            rel_path == scope.rel_path || rel_is_ancestor(rel_path, &scope.rel_path)
        }
        SharedItemKind::Dir => {
            rel_is_same_or_descendant(rel_path, &scope.rel_path)
                || rel_is_ancestor(rel_path, &scope.rel_path)
        }
        SharedItemKind::Note => false,
    })
}

fn subdir_intersects_shared_scope(subdir: &str, scopes: &[SharedPathScope]) -> bool {
    scopes.iter().any(|scope| match scope.kind {
        SharedItemKind::File => {
            subdir == scope.rel_path || rel_is_ancestor(subdir, &scope.rel_path)
        }
        SharedItemKind::Dir => {
            rel_is_same_or_descendant(subdir, &scope.rel_path)
                || rel_is_same_or_descendant(&scope.rel_path, subdir)
        }
        SharedItemKind::Note => false,
    })
}

fn shared_grep_scopes(scopes: &[SharedPathScope]) -> Vec<walk::GrepScope> {
    let mut grep_scopes = Vec::new();
    for scope in scopes {
        let kind = match scope.kind {
            SharedItemKind::File => walk::GrepScopeKind::File,
            SharedItemKind::Dir => walk::GrepScopeKind::Dir,
            SharedItemKind::Note => continue,
        };
        grep_scopes.push(walk::GrepScope {
            kind,
            rel_path: scope.rel_path.clone(),
        });
    }
    grep_scopes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    grep_scopes
}

fn shared_ai_factory_options(
    scopes: &[SharedPathScope],
    options: walk::WalkOptions,
) -> walk::WalkOptions {
    if scopes.iter().any(|scope| {
        walk::path_allowed_for_shared_ai_factory(Path::new(&scope.rel_path), options)
            && !walk::path_allowed_by_options(Path::new(&scope.rel_path), options)
    }) {
        walk::WalkOptions {
            include_hidden: true,
            include_sensitive: options.include_sensitive,
        }
    } else {
        options
    }
}

pub fn handle_tool_call(id: serde_json::Value, params: serde_json::Value) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    info!("Tool call: {}", tool_name);
    debug!("Tool arguments: {}", arguments);

    match tool_name {
        "workspace_context" => {
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_context(id, &db)
        }
        "workspace_read" => {
            let options = walk_options_from_args(&arguments);
            let item_id = arguments.get("item_id").and_then(|v| v.as_i64());
            let project_id = arguments.get("project_id").and_then(|v| v.as_i64());
            let path = arguments
                .get("rel_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Error if both item_id and project_id+rel_path provided
            if item_id.is_some() && (project_id.is_some() || path.is_some()) {
                return JsonRpcResponse::error(
                    id,
                    McpError::invalid_params(
                        "Provide either item_id OR project_id+rel_path, not both",
                    ),
                );
            }

            let read_by_item_id = item_id.is_some();
            let read_by_path = project_id.is_some() && path.is_some();
            if !read_by_item_id && !read_by_path {
                return JsonRpcResponse::error(
                    id,
                    McpError::invalid_params(
                        "Missing required parameters: provide item_id OR project_id+rel_path",
                    ),
                );
            }

            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };

            if let Some(iid) = item_id {
                workspace_read(id, iid, &db, options)
            } else if let (Some(pid), Some(p)) = (project_id, path) {
                workspace_read_by_path(id, pid, &p, &db, options)
            } else {
                unreachable!("workspace_read parameters validated before opening the database")
            }
        }
        "workspace_search" => {
            let query = match arguments.get("query").and_then(|v| v.as_str()) {
                Some(q) => q.to_string(),
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: query"),
                    );
                }
            };
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_search(id, &query, &db)
        }
        "list_groups" => {
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            list_groups(id, &db)
        }
        "list_projects" => {
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            list_projects(id, &db)
        }
        "project_tree" => {
            let project_id = match arguments.get("project_id").and_then(|v| v.as_i64()) {
                Some(pid) => pid,
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: project_id"),
                    );
                }
            };
            let path = arguments
                .get("subdir")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let max_depth = arguments
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|d| d as usize);
            let options = walk_options_from_args(&arguments);
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            project_tree(id, project_id, path.as_deref(), max_depth, &db, options)
        }
        "project_grep" => {
            let project_id = match arguments.get("project_id").and_then(|v| v.as_i64()) {
                Some(pid) => pid,
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: project_id"),
                    );
                }
            };
            let pattern = match arguments.get("pattern").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: pattern"),
                    );
                }
            };
            let glob = arguments
                .get("glob")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let options = walk_options_from_args(&arguments);
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            project_grep(id, project_id, &pattern, glob.as_deref(), &db, options)
        }
        "workspace_search_fulltext" => {
            let query = match arguments.get("query").and_then(|v| v.as_str()) {
                Some(q) => q.to_string(),
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: query"),
                    );
                }
            };
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|l| l as usize)
                .unwrap_or(20);
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_search_fulltext(id, &query, limit, &db)
        }
        "workspace_service_graph" => {
            let project = arguments
                .get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let project_id = arguments.get("project_id").and_then(|v| v.as_i64());
            let group_id = arguments.get("group_id").and_then(|v| v.as_i64());
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_service_graph(id, project.as_deref(), project_id, group_id, &db)
        }
        "workspace_events" => {
            let project = arguments
                .get("project")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let project_id = arguments.get("project_id").and_then(|v| v.as_i64());
            let source = arguments
                .get("source")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let status = match arguments.get("status").and_then(|v| v.as_str()) {
                Some(value) => match value.parse::<crate::models::EventStatus>() {
                    Ok(status) => Some(status),
                    Err(_) => {
                        return JsonRpcResponse::error(
                            id,
                            McpError::invalid_params(
                                "Invalid status. Expected one of: open, closed",
                            ),
                        );
                    }
                },
                None => None,
            };
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_events(
                id,
                project.as_deref(),
                project_id,
                source.as_deref(),
                status,
                &db,
            )
        }
        "workspace_event_details" => {
            let event_id = match arguments.get("event_id").and_then(|v| v.as_i64()) {
                Some(event_id) => event_id,
                None => {
                    return JsonRpcResponse::error(
                        id,
                        McpError::invalid_params("Missing required parameter: event_id"),
                    );
                }
            };
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            workspace_event_details(id, event_id, &db)
        }
        _ => {
            error!("Unknown tool: {}", tool_name);
            JsonRpcResponse::error(
                id,
                McpError::method_not_found(&format!("Unknown tool: {}", tool_name)),
            )
        }
    }
}

fn tool_result(id: serde_json::Value, text: String) -> JsonRpcResponse {
    JsonRpcResponse::result(
        id,
        serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": text
                }
            ]
        }),
    )
}

fn tool_error(id: serde_json::Value, msg: &str) -> JsonRpcResponse {
    JsonRpcResponse::result(
        id,
        serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": msg
                }
            ],
            "isError": true
        }),
    )
}

fn open_db() -> Result<Db, String> {
    Db::open_default().map_err(|e| format!("Failed to open database: {}", e))
}

fn walk_options_from_args(arguments: &serde_json::Value) -> walk::WalkOptions {
    walk::WalkOptions {
        include_hidden: arguments
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        include_sensitive: arguments
            .get("include_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

fn workspace_context(id: serde_json::Value, db: &Db) -> JsonRpcResponse {
    info!("workspace_context: gathering metadata");
    let include_project_paths = project_wide_tools_enabled();

    let projects = match db.list_projects() {
        Ok(p) => p,
        Err(e) => return tool_error(id, &format!("Failed to list projects: {}", e)),
    };

    let groups = match db.list_groups() {
        Ok(g) => g,
        Err(e) => return tool_error(id, &format!("Failed to list groups: {}", e)),
    };

    let mut context = serde_json::json!({
        "projects": [],
        "groups": []
    });

    let projects_arr = context["projects"].as_array_mut().unwrap();
    for p in &projects {
        let project_groups = match db.get_groups_for_project(p.id) {
            Ok(groups) => groups,
            Err(e) => {
                return tool_error(
                    id,
                    &format!("Failed to load groups for project '{}': {}", p.slug, e),
                );
            }
        };
        let items = match db.get_shared_items_for_project(p.id) {
            Ok(items) => items,
            Err(e) => {
                return tool_error(
                    id,
                    &format!(
                        "Failed to load shared items for project '{}': {}",
                        p.slug, e
                    ),
                );
            }
        };
        let deps = match db.list_artifact_dependencies_for_project(p.id) {
            Ok(deps) => deps,
            Err(e) => {
                return tool_error(
                    id,
                    &format!(
                        "Failed to load artifact dependencies for project '{}': {}",
                        p.slug, e
                    ),
                );
            }
        };
        let mut project = serde_json::json!({
            "id": p.id,
            "name": p.name,
            "slug": p.slug,
            "groups": project_groups.iter().map(|g| &g.name).collect::<Vec<_>>(),
            "shared_items": items.iter().map(|i| serde_json::json!({
                "id": i.id,
                "kind": i.kind.as_str(),
                "path": i.path,
                "label": i.label,
                "dependencies": deps.iter()
                    .filter(|dep| dep.shared_item_id == i.id)
                    .map(|dep| serde_json::json!({
                        "service": dep.depends_on_project_slug_snapshot,
                        "kind": dep.kind.as_str(),
                        "reaction": dep.reaction.as_str()
                    }))
                    .collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        });
        if include_project_paths {
            project["path"] = serde_json::json!(p.path);
        }
        projects_arr.push(project);
    }

    let groups_arr = context["groups"].as_array_mut().unwrap();
    for g in &groups {
        let group_projects = match db.get_projects_for_group(g.id) {
            Ok(projects) => projects,
            Err(e) => {
                return tool_error(
                    id,
                    &format!("Failed to load projects for group '{}': {}", g.name, e),
                );
            }
        };
        let notes = match db.get_all_items_for_group(g.id) {
            Ok(notes) => notes,
            Err(e) => {
                return tool_error(
                    id,
                    &format!("Failed to load shared items for group '{}': {}", g.name, e),
                );
            }
        };
        let mut seen_contents = std::collections::HashSet::new();
        let note_items: Vec<_> = notes
            .iter()
            .filter(|i| i.kind == crate::models::SharedItemKind::Note)
            .filter(|i| {
                let content = i.content.as_deref().unwrap_or("");
                seen_contents.insert(content.to_string())
            })
            .map(|i| {
                serde_json::json!({
                    "id": i.id,
                    "label": i.label,
                    "preview": i.content.as_deref().unwrap_or("").chars().take(100).collect::<String>()
                })
            })
            .collect();
        groups_arr.push(serde_json::json!({
            "id": g.id,
            "name": g.name,
            "projects": group_projects.iter().map(|p| serde_json::json!({
                "id": p.id,
                "name": p.name,
                "slug": p.slug,
            })).collect::<Vec<_>>(),
            "notes": note_items
        }));
    }

    match serde_json::to_string_pretty(&context) {
        Ok(text) => tool_result(id, text),
        Err(e) => tool_error(id, &format!("Failed to serialize workspace context: {}", e)),
    }
}

const PATH_POLICY_DENIED: &str = "Access denied: hidden or sensitive path requires explicit opt-in";

fn path_allowed_under_root(root: &Path, path: &Path, options: walk::WalkOptions) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    walk::path_allowed_by_options(rel, options)
}

fn path_allowed_for_shared_context_under_root(
    root: &Path,
    path: &Path,
    options: walk::WalkOptions,
) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    walk::path_allowed_for_shared_ai_factory(rel, options)
}

fn read_visible_path(
    id: serde_json::Value,
    canonical_root: &Path,
    canonical: &Path,
    options: walk::WalkOptions,
    allow_shared_ai_factory: bool,
) -> JsonRpcResponse {
    let path_allowed = if allow_shared_ai_factory {
        path_allowed_for_shared_context_under_root(canonical_root, canonical, options)
    } else {
        path_allowed_under_root(canonical_root, canonical, options)
    };
    if !path_allowed {
        return tool_error(id, PATH_POLICY_DENIED);
    }

    // Limit file reads to 10 MB to prevent OOM
    const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

    debug!("Reading file: {}", canonical.display());
    if canonical.is_dir() {
        match std::fs::read_dir(canonical) {
            Ok(entries) => {
                let mut listing: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        if allow_shared_ai_factory {
                            path_allowed_for_shared_context_under_root(
                                canonical_root,
                                &e.path(),
                                options,
                            )
                        } else {
                            path_allowed_under_root(canonical_root, &e.path(), options)
                        }
                    })
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                listing.sort();
                tool_result(id, listing.join("\n"))
            }
            Err(e) => {
                error!("Failed to read dir {}: {}", canonical.display(), e);
                tool_error(id, "Failed to read directory")
            }
        }
    } else {
        match std::fs::metadata(canonical) {
            Ok(meta) if meta.len() > MAX_READ_SIZE => tool_error(
                id,
                &format!(
                    "File too large ({} bytes, max {})",
                    meta.len(),
                    MAX_READ_SIZE
                ),
            ),
            _ => match std::fs::read_to_string(canonical) {
                Ok(content) => tool_result(id, content),
                Err(e) => {
                    error!("Failed to read file {}: {}", canonical.display(), e);
                    tool_error(id, "Failed to read file")
                }
            },
        }
    }
}

fn workspace_read(
    id: serde_json::Value,
    item_id: i64,
    db: &Db,
    options: walk::WalkOptions,
) -> JsonRpcResponse {
    info!("workspace_read: item_id={}", item_id);

    let item = match db.get_item_by_id(item_id) {
        Ok(Some(item)) => item,
        Ok(None) => return tool_error(id, &format!("Item {} not found", item_id)),
        Err(e) => return tool_error(id, &format!("Query error: {}", e)),
    };

    if item.kind == crate::models::SharedItemKind::Note {
        return tool_result(id, item.content.unwrap_or_default());
    }

    let project = match item
        .project_id
        .and_then(|pid| db.get_project_by_id(pid).ok().flatten())
    {
        Some(p) => p,
        None => return tool_error(id, "Invalid shared item: missing project"),
    };

    let rel_path = match item.path {
        Some(p) => p,
        None => return tool_error(id, "Invalid shared item: missing path"),
    };
    if !walk::path_allowed_for_shared_ai_factory(Path::new(&rel_path), options) {
        return tool_error(id, PATH_POLICY_DENIED);
    }

    let project_root = Path::new(&project.path);
    let full_path = project_root.join(&rel_path);

    // Path traversal protection
    let canonical = match full_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            error!("Cannot resolve path {}: {}", full_path.display(), e);
            return tool_error(id, "Cannot resolve path");
        }
    };
    let canonical_root = match project_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            error!(
                "Cannot resolve project path {}: {}",
                project_root.display(),
                e
            );
            return tool_error(id, "Cannot resolve project path");
        }
    };
    if !canonical.starts_with(&canonical_root) {
        return tool_error(id, "Access denied: path is outside project directory");
    }

    read_visible_path(id, &canonical_root, &canonical, options, true)
}

fn workspace_read_by_path(
    id: serde_json::Value,
    project_id: i64,
    path: &str,
    db: &Db,
    options: walk::WalkOptions,
) -> JsonRpcResponse {
    info!(
        "workspace_read_by_path: project_id={}, path={}",
        project_id, path
    );

    let project_wide = project_wide_tools_enabled();
    let (root, canonical_root, normalized, matched_scope) = if project_wide {
        let (root, canonical_root) = match resolve_project_root(project_id, db) {
            Ok(v) => v,
            Err(e) => return tool_error(id, &e),
        };
        let normalized = if path.trim() == "." {
            String::new()
        } else {
            match normalize_rel_path(path) {
                Ok(path) => path,
                Err(e) => return tool_error(id, &e),
            }
        };
        (root, canonical_root, normalized, None)
    } else {
        let normalized = match normalize_rel_path(path) {
            Ok(path) => path,
            Err(e) => {
                warn!(
                    "workspace_read_by_path denied invalid path '{}': {}",
                    path, e
                );
                return tool_error(id, &e);
            }
        };
        let (root, canonical_root) = match resolve_project_root(project_id, db) {
            Ok(v) => v,
            Err(e) => return tool_error(id, &e),
        };
        let scopes = match shared_path_scopes(project_id, &canonical_root, db) {
            Ok(scopes) => scopes,
            Err(e) => return tool_error(id, &e),
        };
        let Some(scope) = find_shared_scope(&normalized, &scopes).cloned() else {
            warn!("workspace_read_by_path denied unshared path: {}", path);
            return tool_error(id, ACCESS_DENIED_NOT_SHARED);
        };
        (root, canonical_root, normalized, Some(scope))
    };

    let full_path = root.join(&normalized);
    let canonical = match full_path.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            error!("Cannot resolve path {}: {}", full_path.display(), e);
            return tool_error(id, "Cannot resolve path");
        }
    };
    if !canonical.starts_with(&canonical_root) {
        warn!("workspace_read_by_path denied project escape: {}", path);
        return tool_error(id, "Access denied: path is outside project directory");
    }
    if let Some(scope) = matched_scope.as_ref()
        && !canonical_path_is_shared(&canonical, scope)
    {
        warn!(
            "workspace_read_by_path denied canonical path escape: {}",
            path
        );
        return tool_error(id, ACCESS_DENIED_NOT_SHARED);
    }

    read_visible_path(
        id,
        &canonical_root,
        &canonical,
        options,
        matched_scope.is_some(),
    )
}

fn workspace_search(id: serde_json::Value, query: &str, db: &Db) -> JsonRpcResponse {
    info!("workspace_search: query={}", query);

    match db.search_items(query) {
        Ok(items) => {
            let results: Vec<_> = items
                .iter()
                .map(|i| {
                    serde_json::json!({
                        "id": i.id,
                        "label": i.label,
                        "group_id": i.group_id,
                        "project_id": i.project_id,
                        "content": i.content,
                        "created_at": i.created_at
                    })
                })
                .collect();
            let text = serde_json::to_string_pretty(&results).unwrap_or_default();
            tool_result(id, text)
        }
        Err(e) => tool_error(id, &format!("Search error: {}", e)),
    }
}

fn workspace_search_fulltext(
    id: serde_json::Value,
    query: &str,
    limit: usize,
    db: &Db,
) -> JsonRpcResponse {
    info!(
        "workspace_search_fulltext: query='{}' limit={}",
        query, limit
    );

    // Bounded lazy refresh keeps common edits fresh; matching directory-owned
    // file hits are revalidated below before snippets can be returned.
    if let Err(e) = crate::indexer::refresh_stale(db, 200) {
        log::warn!("refresh_stale failed: {}", e);
        return tool_error(id, "Fulltext search refresh failed");
    }

    match db.search_files(query, limit) {
        Ok(mut hits) => {
            match crate::indexer::refresh_search_hits(db, &hits) {
                Ok(refreshed) if refreshed > 0 => match db.search_files(query, limit) {
                    Ok(updated_hits) => hits = updated_hits,
                    Err(e) => return tool_error(id, &format!("Fulltext search error: {}", e)),
                },
                Ok(_) => {}
                Err(e) => {
                    log::warn!("refresh_search_hits failed: {}", e);
                    return tool_error(id, "Fulltext search refresh failed");
                }
            }

            let results: Vec<_> = hits
                .iter()
                .filter(|h| {
                    walk::path_allowed_for_shared_ai_factory(
                        Path::new(&h.path),
                        walk::WalkOptions::default(),
                    )
                })
                .map(|h| {
                    serde_json::json!({
                        "shared_item_id": h.shared_item_id,
                        "project_id": h.project_id,
                        "path": h.path,
                        "snippet": h.snippet,
                        "rank": h.rank,
                    })
                })
                .collect();
            let text = serde_json::to_string_pretty(&results).unwrap_or_default();
            tool_result(id, text)
        }
        Err(e) => tool_error(id, &format!("Fulltext search error: {}", e)),
    }
}

fn list_groups(id: serde_json::Value, db: &Db) -> JsonRpcResponse {
    info!("list_groups");
    let include_project_paths = project_wide_tools_enabled();

    let groups = match db.list_groups() {
        Ok(g) => g,
        Err(e) => return tool_error(id, &format!("Failed to list groups: {}", e)),
    };

    let result: Vec<_> = groups
        .iter()
        .map(|g| {
            let projects = db.get_projects_for_group(g.id).unwrap_or_default();
            let project_values: Vec<_> = projects
                .iter()
                .map(|p| {
                    let mut project = serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "slug": p.slug
                    });
                    if include_project_paths {
                        project["path"] = serde_json::json!(p.path);
                    }
                    project
                })
                .collect();
            serde_json::json!({
                "id": g.id,
                "name": g.name,
                "projects": project_values
            })
        })
        .collect();

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
}

/// Resolve a project by ID and validate an optional subpath within it.
/// Returns (project_root_path, resolved_canonical_path).
fn resolve_project_path(
    project_id: i64,
    subpath: Option<&str>,
    db: &Db,
) -> Result<(PathBuf, PathBuf), String> {
    let (root, canonical_root) = resolve_project_root(project_id, db)?;

    let target = if let Some(sub) = subpath {
        let full = root.join(sub);
        let canonical = full
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path '{}': {}", sub, e))?;
        if !canonical.starts_with(&canonical_root) {
            return Err("Access denied: path is outside project directory".to_string());
        }
        canonical
    } else {
        canonical_root.clone()
    };

    Ok((canonical_root, target))
}

fn project_tree(
    id: serde_json::Value,
    project_id: i64,
    path: Option<&str>,
    max_depth: Option<usize>,
    db: &Db,
    options: walk::WalkOptions,
) -> JsonRpcResponse {
    info!(
        "project_tree: project_id={}, path={:?}, max_depth={:?}",
        project_id, path, max_depth
    );

    let (canonical_root, _) = match resolve_project_path(project_id, None, db) {
        Ok(v) => v,
        Err(e) => return tool_error(id, &e),
    };

    let project_wide = project_wide_tools_enabled();
    let scopes = if project_wide {
        Vec::new()
    } else {
        match shared_path_scopes(project_id, &canonical_root, db) {
            Ok(scopes) => scopes,
            Err(e) => return tool_error(id, &e),
        }
    };

    let normalized_subdir = match path {
        Some(path) => match normalize_rel_path(path) {
            Ok(path) => {
                if !project_wide && !subdir_intersects_shared_scope(&path, &scopes) {
                    debug!(
                        "project_tree: subdir '{}' has no shared-scope intersection",
                        path
                    );
                    return tool_result(id, String::new());
                }
                Some(path)
            }
            Err(e) => return tool_error(id, &e),
        },
        None => None,
    };

    let effective_options = if project_wide {
        options
    } else {
        shared_ai_factory_options(&scopes, options)
    };
    let entries = walk::walk_project_tree(
        &canonical_root,
        normalized_subdir.as_deref(),
        max_depth,
        effective_options,
    );
    let entries: Vec<_> = if project_wide {
        entries
    } else {
        entries
            .into_iter()
            .filter(|entry| tree_path_visible(&entry.path, &scopes))
            .collect()
    };
    debug!("project_tree: returning {} entries", entries.len());

    // Format as indented tree
    let mut lines = Vec::new();
    for entry in &entries {
        let depth = entry.path.matches('/').count();
        let indent = "  ".repeat(depth);
        let suffix = if entry.is_dir { "/" } else { "" };
        lines.push(format!("{}{}{}", indent, entry.name, suffix));
    }

    tool_result(id, lines.join("\n"))
}

fn project_grep(
    id: serde_json::Value,
    project_id: i64,
    pattern: &str,
    glob: Option<&str>,
    db: &Db,
    options: walk::WalkOptions,
) -> JsonRpcResponse {
    info!(
        "project_grep: project_id={}, pattern={}, glob={:?}",
        project_id, pattern, glob
    );

    let (canonical_root, _) = match resolve_project_path(project_id, None, db) {
        Ok(v) => v,
        Err(e) => return tool_error(id, &e),
    };

    let matches = if project_wide_tools_enabled() {
        walk::grep_project(&canonical_root, pattern, glob, options)
    } else {
        let scopes = match shared_path_scopes(project_id, &canonical_root, db) {
            Ok(scopes) => scopes,
            Err(e) => return tool_error(id, &e),
        };
        let grep_scopes = shared_grep_scopes(&scopes);
        let effective_options = shared_ai_factory_options(&scopes, options);
        walk::grep_project_paths(
            &canonical_root,
            &grep_scopes,
            pattern,
            glob,
            effective_options,
        )
    };

    let matches = match matches {
        Ok(m) => m,
        Err(e) => {
            return JsonRpcResponse::error(id, McpError::invalid_params(&e));
        }
    };

    // Group by file
    let mut grouped: std::collections::BTreeMap<&str, Vec<&walk::GrepMatch>> =
        std::collections::BTreeMap::new();
    for m in &matches {
        grouped.entry(&m.path).or_default().push(m);
    }

    let mut lines = Vec::new();
    for (path, file_matches) in &grouped {
        lines.push(format!("{}:", path));
        for m in file_matches {
            lines.push(format!("  {}:{}", m.line_number, m.line_content));
        }
    }

    tool_result(id, lines.join("\n"))
}

fn list_projects(id: serde_json::Value, db: &Db) -> JsonRpcResponse {
    info!("list_projects");
    let include_project_paths = project_wide_tools_enabled();

    let projects = match db.list_projects() {
        Ok(p) => p,
        Err(e) => return tool_error(id, &format!("Failed to list projects: {}", e)),
    };

    let result: Vec<_> = projects
        .iter()
        .map(|p| {
            let groups = db.get_groups_for_project(p.id).unwrap_or_default();
            let mut project = serde_json::json!({
                "id": p.id,
                "name": p.name,
                "slug": p.slug,
                "groups": groups.iter().map(|g| serde_json::json!({
                    "id": g.id,
                    "name": g.name
                })).collect::<Vec<_>>()
            });
            if include_project_paths {
                project["path"] = serde_json::json!(p.path);
            }
            project
        })
        .collect();

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
}

fn project_slug(db: &Db, project_id: i64) -> String {
    db.get_project_by_id(project_id)
        .ok()
        .flatten()
        .map(|project| project.slug)
        .unwrap_or_else(|| format!("project={project_id}"))
}

fn service_link_json(db: &Db, link: &crate::models::ServiceLink) -> serde_json::Value {
    serde_json::json!({
        "id": link.id,
        "from_project_id": link.from_project_id,
        "from": project_slug(db, link.from_project_id),
        "to_project_id": link.to_project_id,
        "to": project_slug(db, link.to_project_id),
        "kind": link.kind.as_str(),
        "label": link.label,
        "created_at": link.created_at,
        "updated_at": link.updated_at
    })
}

fn workspace_service_graph(
    id: serde_json::Value,
    project: Option<&str>,
    project_id: Option<i64>,
    group_id: Option<i64>,
    db: &Db,
) -> JsonRpcResponse {
    info!(
        "workspace_service_graph: project={:?}, project_id={:?}, group_id={:?}",
        project, project_id, group_id
    );

    if group_id.is_some() && (project.is_some() || project_id.is_some()) {
        return JsonRpcResponse::error(
            id,
            McpError::invalid_params("Provide either group_id OR project/project_id, not both"),
        );
    }
    if project.is_some() && project_id.is_some() {
        return JsonRpcResponse::error(
            id,
            McpError::invalid_params("Provide either project OR project_id, not both"),
        );
    }

    let (scope, links) = if let Some(group_id) = group_id {
        debug!(
            "workspace_service_graph: listing group graph group_id={}",
            group_id
        );
        match db.list_group_service_links(group_id) {
            Ok(links) => (serde_json::json!({"group_id": group_id}), links),
            Err(e) => return tool_error(id, &format!("Failed to list service graph: {}", e)),
        }
    } else if let Some(project_id) = project_id {
        match db.get_project_by_id(project_id) {
            Ok(Some(project)) => match service_graph_for_project(db, &project) {
                Ok(graph) => graph,
                Err(e) => return tool_error(id, &format!("Failed to list service graph: {}", e)),
            },
            Ok(None) => {
                warn!(
                    "workspace_service_graph: project_id={} not found",
                    project_id
                );
                return tool_error(id, &format!("Project {} not found", project_id));
            }
            Err(e) => return tool_error(id, &format!("Failed to resolve project: {}", e)),
        }
    } else if let Some(project) = project {
        match db.resolve_project_target(project) {
            Ok(Some(project)) => match service_graph_for_project(db, &project) {
                Ok(graph) => graph,
                Err(e) => return tool_error(id, &format!("Failed to list service graph: {}", e)),
            },
            Ok(None) => {
                warn!("workspace_service_graph: project='{}' not found", project);
                return tool_error(id, &format!("Project '{}' not found", project));
            }
            Err(e) => return tool_error(id, &format!("Failed to resolve project: {}", e)),
        }
    } else {
        debug!("workspace_service_graph: listing all service links");
        match db.list_service_links() {
            Ok(links) => (serde_json::json!({"all": true}), links),
            Err(e) => return tool_error(id, &format!("Failed to list service links: {}", e)),
        }
    };

    info!(
        "workspace_service_graph: returning {} service links",
        links.len()
    );
    let result = serde_json::json!({
        "scope": scope,
        "links": links.iter().map(|link| service_link_json(db, link)).collect::<Vec<_>>()
    });
    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
}

fn service_graph_for_project(
    db: &Db,
    project: &crate::models::Project,
) -> anyhow::Result<(serde_json::Value, Vec<crate::models::ServiceLink>)> {
    debug!(
        "workspace_service_graph: listing graph for project id={} slug='{}'",
        project.id, project.slug
    );
    let groups = db.get_groups_for_project(project.id)?;
    if groups.is_empty() {
        warn!(
            "workspace_service_graph: project slug='{}' has no groups; returning direct links",
            project.slug
        );
        let mut links = db.list_outgoing_service_links(project.id)?;
        for link in db.list_incoming_service_links(project.id)? {
            if !links.iter().any(|existing| existing.id == link.id) {
                links.push(link);
            }
        }
        return Ok((
            serde_json::json!({
                "project_id": project.id,
                "project": project.slug
            }),
            links,
        ));
    }

    let mut links: Vec<crate::models::ServiceLink> = Vec::new();
    let mut scope_groups = Vec::new();
    for group in groups {
        scope_groups.push(serde_json::json!({
            "id": group.id,
            "name": group.name
        }));
        for link in db.list_group_service_links(group.id)? {
            if !links.iter().any(|existing| existing.id == link.id) {
                links.push(link);
            }
        }
    }

    Ok((
        serde_json::json!({
            "project_id": project.id,
            "project": project.slug,
            "groups": scope_groups
        }),
        links,
    ))
}

fn workspace_events(
    id: serde_json::Value,
    project: Option<&str>,
    project_id: Option<i64>,
    source: Option<&str>,
    status: Option<crate::models::EventStatus>,
    db: &Db,
) -> JsonRpcResponse {
    info!(
        "workspace_events: project={:?}, project_id={:?}, source={:?}, status={:?}",
        project, project_id, source, status
    );
    if project.is_some() && project_id.is_some() {
        return JsonRpcResponse::error(
            id,
            McpError::invalid_params("Provide either project OR project_id, not both"),
        );
    }
    if (project.is_some() || project_id.is_some()) && (source.is_some() || status.is_some()) {
        return JsonRpcResponse::error(
            id,
            McpError::invalid_params(
                "Project inbox mode cannot be combined with source/status filters",
            ),
        );
    }

    let events = if let Some(project_id) = project_id {
        match db.get_project_by_id(project_id) {
            Ok(Some(_)) => match db.list_workspace_event_inbox(project_id) {
                Ok(events) => events,
                Err(e) => return tool_error(id, &format!("Failed to list event inbox: {}", e)),
            },
            Ok(None) => {
                warn!("workspace_events: project_id={} not found", project_id);
                return tool_error(id, &format!("Project {} not found", project_id));
            }
            Err(e) => return tool_error(id, &format!("Failed to resolve project: {}", e)),
        }
    } else if let Some(project) = project {
        match db.resolve_project_target(project) {
            Ok(Some(project)) => match db.list_workspace_event_inbox(project.id) {
                Ok(events) => events,
                Err(e) => return tool_error(id, &format!("Failed to list event inbox: {}", e)),
            },
            Ok(None) => {
                warn!("workspace_events: project='{}' not found", project);
                return tool_error(id, &format!("Project '{}' not found", project));
            }
            Err(e) => return tool_error(id, &format!("Failed to resolve project: {}", e)),
        }
    } else {
        match db.list_workspace_events(source, status) {
            Ok(events) => events,
            Err(e) => return tool_error(id, &format!("Failed to list workspace events: {}", e)),
        }
    };

    info!("workspace_events: returning {} events", events.len());
    let result: Vec<_> = events.iter().map(event_json).collect();
    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
}

fn workspace_event_details(id: serde_json::Value, event_id: i64, db: &Db) -> JsonRpcResponse {
    info!("workspace_event_details: event_id={}", event_id);

    let event = match db.get_workspace_event(event_id) {
        Ok(Some(event)) => event,
        Ok(None) => {
            warn!("workspace_event_details: event_id={} not found", event_id);
            return tool_error(id, &format!("Event {} not found", event_id));
        }
        Err(e) => return tool_error(id, &format!("Failed to get event: {}", e)),
    };
    let targets = match db.list_event_targets(event_id) {
        Ok(targets) => targets,
        Err(e) => return tool_error(id, &format!("Failed to list event targets: {}", e)),
    };
    let artifacts = match db.list_event_artifacts(event_id) {
        Ok(artifacts) => artifacts,
        Err(e) => return tool_error(id, &format!("Failed to list event artifacts: {}", e)),
    };

    info!(
        "workspace_event_details: returning event_id={} targets={} artifacts={}",
        event_id,
        targets.len(),
        artifacts.len()
    );
    let result = serde_json::json!({
        "event": event_json(&event),
        "affected_services": targets.iter().map(|target| serde_json::json!({
            "id": target.id,
            "event_id": target.event_id,
            "affected_project_id": target.affected_project_id,
            "project": target.affected_project_id.map(|pid| project_slug(db, pid)),
            "relation_kind": target.relation_kind.as_str(),
            "status": target.status.as_str(),
            "created_at": target.created_at,
            "updated_at": target.updated_at
        })).collect::<Vec<_>>(),
        "affected_artifacts": artifacts.iter().map(|artifact| serde_json::json!({
            "id": artifact.id,
            "event_id": artifact.event_id,
            "affected_project_id": artifact.affected_project_id,
            "project": artifact.affected_project_id.map(|pid| project_slug(db, pid)),
            "shared_item_id": artifact.shared_item_id,
            "path": artifact.path_snapshot,
            "reaction": artifact.reaction.as_str(),
            "reason": artifact.reason,
            "status": artifact.status.as_str(),
            "created_at": artifact.created_at,
            "updated_at": artifact.updated_at
        })).collect::<Vec<_>>()
    });
    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
}

fn event_json(event: &crate::models::WorkspaceEvent) -> serde_json::Value {
    serde_json::json!({
        "id": event.id,
        "source_project_id": event.source_project_id,
        "source_project_slug": event.source_project_slug,
        "source_project_name": event.source_project_name,
        "group_id": event.group_id,
        "kind": event.kind.as_str(),
        "title": event.title,
        "body": event.body,
        "severity": event.severity.as_str(),
        "status": event.status.as_str(),
        "created_at": event.created_at,
        "updated_at": event.updated_at
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::NamedTempFile;

    fn test_db() -> Db {
        let tmp = NamedTempFile::new().unwrap();
        Db::open(tmp.path()).unwrap()
    }

    fn seed_db(db: &Db) -> (i64, i64) {
        let pid = db.create_project("test-proj", "/tmp/test-proj").unwrap();
        let gid = db.get_or_create_group("test-grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.share_file(pid, "src/main.rs", Some("main")).unwrap();
        db.add_project_note(pid, "project note content", Some("pnote"))
            .unwrap();
        db.add_group_note(gid, pid, "group note content", Some("gnote"))
            .unwrap();
        (pid, gid)
    }

    fn seed_event_graph(db: &Db) -> (i64, i64, i64) {
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth-mcp", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api-mcp", Some("api"))
            .unwrap();
        let group = db.get_or_create_group("platform").unwrap();
        db.add_project_to_group(auth, group).unwrap();
        db.add_project_to_group(api, group).unwrap();
        db.create_service_link(
            "api",
            "auth",
            crate::models::ServiceLinkKind::DependsOn,
            Some("JWT"),
        )
        .unwrap();
        db.share_file(api, "docs/auth.md", Some("auth-doc"))
            .unwrap();
        db.add_artifact_dependency(
            api,
            "docs/auth.md",
            "auth",
            crate::models::ArtifactDependencyKind::References,
            crate::models::ArtifactReaction::Update,
        )
        .unwrap();
        let event = db
            .create_workspace_event(
                "auth",
                crate::models::WorkspaceEventKind::ServiceChanged,
                crate::models::EventSeverity::Warning,
                "Auth changed",
                Some("Token contract changed"),
            )
            .unwrap();
        (api, group, event)
    }

    // --- handle_tool_call dispatch ---

    #[test]
    fn handle_unknown_tool() {
        let resp = handle_tool_call(json!(1), json!({"name": "nonexistent"}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn handle_tool_call_missing_name() {
        let resp = handle_tool_call(json!(1), json!({}));
        assert!(resp.error.is_some());
    }

    #[test]
    fn handle_tool_call_workspace_read_missing_params() {
        let resp = handle_tool_call(json!(1), json!({"name": "workspace_read", "arguments": {}}));
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn handle_tool_call_workspace_read_both_params_error() {
        let resp = handle_tool_call(
            json!(1),
            json!({"name": "workspace_read", "arguments": {"item_id": 1, "project_id": 1, "rel_path": "foo"}}),
        );
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn handle_tool_call_workspace_search_missing_query() {
        let resp = handle_tool_call(
            json!(1),
            json!({"name": "workspace_search", "arguments": {}}),
        );
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    // --- tool_result / tool_error formatting ---

    #[test]
    fn tool_result_format() {
        let resp = tool_result(json!(1), "hello".to_string());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
    }

    #[test]
    fn tool_error_format() {
        let resp = tool_error(json!(1), "bad thing");
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "bad thing");
    }

    // --- workspace_context ---

    #[test]
    fn workspace_context_empty_db() {
        let db = test_db();
        let resp = workspace_context(json!(1), &db);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(parsed["projects"].as_array().unwrap().is_empty());
        assert!(parsed["groups"].as_array().unwrap().is_empty());
    }

    #[test]
    fn workspace_context_with_data() {
        let db = test_db();
        seed_db(&db);
        let resp = workspace_context(json!(1), &db);
        assert!(resp.error.is_none());

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();

        let projects = parsed["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["name"], "test-proj");
        assert_eq!(projects[0]["groups"][0], "test-grp");
        // file + project note
        assert_eq!(projects[0]["shared_items"].as_array().unwrap().len(), 2);

        let groups = parsed["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let notes = groups[0]["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0]["preview"].as_str().unwrap().contains("group note"));
    }

    #[test]
    fn workspace_context_includes_artifact_dependencies() {
        let db = test_db();
        seed_event_graph(&db);
        let resp = workspace_context(json!(1), &db);
        assert!(resp.error.is_none());

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let projects = parsed["projects"].as_array().unwrap();
        let api = projects
            .iter()
            .find(|project| project["slug"] == "api")
            .unwrap();
        let item = api["shared_items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == "docs/auth.md")
            .unwrap();
        assert_eq!(item["dependencies"][0]["service"], "auth");
        assert_eq!(item["dependencies"][0]["kind"], "references");
        assert_eq!(item["dependencies"][0]["reaction"], "update");
    }

    #[test]
    fn workspace_service_graph_returns_project_group_links() {
        let db = test_db();
        seed_event_graph(&db);

        let resp = workspace_service_graph(json!(1), Some("api"), None, None, &db);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["scope"]["project"], "api");
        assert_eq!(parsed["scope"]["groups"][0]["name"], "platform");
        assert_eq!(parsed["links"][0]["from"], "api");
        assert_eq!(parsed["links"][0]["to"], "auth");
        assert_eq!(parsed["links"][0]["kind"], "depends_on");
    }

    #[test]
    fn workspace_service_graph_returns_all_project_group_links() {
        let db = test_db();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth-multi-mcp", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api-multi-mcp", Some("api"))
            .unwrap();
        let web = db
            .create_project_with_slug("Web", "/tmp/web-multi-mcp", Some("web"))
            .unwrap();
        let platform = db.get_or_create_group("platform").unwrap();
        let frontend = db.get_or_create_group("frontend").unwrap();
        db.add_project_to_group(auth, platform).unwrap();
        db.add_project_to_group(api, platform).unwrap();
        db.add_project_to_group(api, frontend).unwrap();
        db.add_project_to_group(web, frontend).unwrap();
        db.create_service_link(
            "api",
            "auth",
            crate::models::ServiceLinkKind::DependsOn,
            Some("JWT"),
        )
        .unwrap();
        db.create_service_link(
            "web",
            "api",
            crate::models::ServiceLinkKind::DependsOn,
            Some("API"),
        )
        .unwrap();

        let resp = workspace_service_graph(json!(1), Some("api"), None, None, &db);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["scope"]["project"], "api");
        assert_eq!(parsed["scope"]["groups"].as_array().unwrap().len(), 2);

        let links = parsed["links"].as_array().unwrap();
        assert_eq!(links.len(), 2);
        assert!(links.iter().any(|link| {
            link["from"] == serde_json::json!("api") && link["to"] == serde_json::json!("auth")
        }));
        assert!(links.iter().any(|link| {
            link["from"] == serde_json::json!("web") && link["to"] == serde_json::json!("api")
        }));
    }

    #[test]
    fn workspace_events_returns_project_inbox() {
        let db = test_db();
        seed_event_graph(&db);

        let resp = workspace_events(json!(1), Some("api"), None, None, None, &db);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed[0]["source_project_slug"], "auth");
        assert_eq!(parsed[0]["kind"], "service_changed");
        assert_eq!(parsed[0]["status"], "open");
    }

    #[test]
    fn workspace_event_details_returns_targets_and_artifacts() {
        let db = test_db();
        let (_api, _group, event) = seed_event_graph(&db);

        let resp = workspace_event_details(json!(1), event, &db);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["event"]["id"], event);
        assert_eq!(parsed["affected_services"][0]["project"], "api");
        assert_eq!(parsed["affected_artifacts"][0]["path"], "docs/auth.md");
        assert_eq!(parsed["affected_artifacts"][0]["reaction"], "update");
    }

    // --- workspace_read ---

    #[test]
    fn workspace_read_item_not_found() {
        let db = test_db();
        let resp = workspace_read(json!(1), 9999, &db, walk::WalkOptions::default());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("not found"));
    }

    #[test]
    fn workspace_read_note_returns_content() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        let note_id = db.add_project_note(pid, "my note text", None).unwrap();

        let resp = workspace_read(json!(1), note_id, &db, walk::WalkOptions::default());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(text, "my note text");
    }

    #[test]
    fn workspace_read_file_success() {
        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().to_string_lossy().to_string();
        std::fs::write(tmp.path().join("hello.txt"), "file content").unwrap();

        let db = test_db();
        let pid = db.create_project("p", &project_path).unwrap();
        let file_id = db.share_file(pid, "hello.txt", None).unwrap();

        let resp = workspace_read(json!(1), file_id, &db, walk::WalkOptions::default());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(text, "file content");
    }

    #[test]
    fn workspace_read_directory_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().to_string_lossy().to_string();
        let subdir = tmp.path().join("mydir");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("a.txt"), "").unwrap();
        std::fs::write(subdir.join("b.txt"), "").unwrap();

        let db = test_db();
        let pid = db.create_project("p", &project_path).unwrap();
        let dir_id = db.share_dir(pid, "mydir", None).unwrap();

        let resp = workspace_read(json!(1), dir_id, &db, walk::WalkOptions::default());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("a.txt"));
        assert!(text.contains("b.txt"));
    }

    #[test]
    fn workspace_read_missing_file_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().to_string_lossy().to_string();

        let db = test_db();
        let pid = db.create_project("p", &project_path).unwrap();
        let file_id = db.share_file(pid, "gone.txt", None).unwrap();

        let resp = workspace_read(json!(1), file_id, &db, walk::WalkOptions::default());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Cannot resolve path"));
    }

    // --- workspace_search ---

    #[test]
    fn workspace_search_returns_results() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        db.add_project_note(pid, "searchable content here", Some("lbl"))
            .unwrap();

        let resp = workspace_search(json!(1), "searchable", &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed[0]["content"]
                .as_str()
                .unwrap()
                .contains("searchable")
        );
    }

    #[test]
    fn workspace_search_empty_results() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        db.add_project_note(pid, "hello", None).unwrap();

        let resp = workspace_search(json!(1), "zzzzz", &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert!(parsed.is_empty());
    }

    // --- list_groups ---

    #[test]
    fn list_groups_empty() {
        let db = test_db();
        let resp = list_groups(json!(1), &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn list_groups_with_data() {
        let db = test_db();
        seed_db(&db);
        let resp = list_groups(json!(1), &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "test-grp");
        assert_eq!(parsed[0]["projects"].as_array().unwrap().len(), 1);
    }

    // --- list_projects ---

    #[test]
    fn list_projects_empty() {
        let db = test_db();
        let resp = list_projects(json!(1), &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn list_projects_with_data() {
        let db = test_db();
        seed_db(&db);
        let resp = list_projects(json!(1), &db);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "test-proj");
        assert_eq!(parsed[0]["groups"].as_array().unwrap().len(), 1);
    }
}
