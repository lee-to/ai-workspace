use log::{debug, error, info};
use std::path::Path;

use super::protocol::{JsonRpcResponse, McpError};
use crate::db::Db;
use crate::walk;

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
            let item_id = arguments.get("item_id").and_then(|v| v.as_i64());
            let project_id = arguments.get("project_id").and_then(|v| v.as_i64());
            let path = arguments
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Error if both item_id and project_id+path provided
            if item_id.is_some() && (project_id.is_some() || path.is_some()) {
                return JsonRpcResponse::error(
                    id,
                    McpError::invalid_params("Provide either item_id OR project_id+path, not both"),
                );
            }

            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };

            if let Some(iid) = item_id {
                workspace_read(id, iid, &db)
            } else if let (Some(pid), Some(p)) = (project_id, path) {
                workspace_read_by_path(id, pid, &p, &db)
            } else {
                JsonRpcResponse::error(
                    id,
                    McpError::invalid_params(
                        "Missing required parameters: provide item_id OR project_id+path",
                    ),
                )
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
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let max_depth = arguments
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|d| d as usize);
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            project_tree(id, project_id, path.as_deref(), max_depth, &db)
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
            let db = match open_db() {
                Ok(db) => db,
                Err(e) => return tool_error(id, &e),
            };
            project_grep(id, project_id, &pattern, glob.as_deref(), &db)
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

fn workspace_context(id: serde_json::Value, db: &Db) -> JsonRpcResponse {
    info!("workspace_context: gathering metadata");

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
        let project_groups = db.get_groups_for_project(p.id).unwrap_or_default();
        let items = db.get_shared_items_for_project(p.id).unwrap_or_default();
        projects_arr.push(serde_json::json!({
            "id": p.id,
            "name": p.name,
            "path": p.path,
            "groups": project_groups.iter().map(|g| &g.name).collect::<Vec<_>>(),
            "shared_items": items.iter().map(|i| serde_json::json!({
                "id": i.id,
                "kind": i.kind.as_str(),
                "path": i.path,
                "label": i.label,
            })).collect::<Vec<_>>()
        }));
    }

    let groups_arr = context["groups"].as_array_mut().unwrap();
    for g in &groups {
        let group_projects = db.get_projects_for_group(g.id).unwrap_or_default();
        let notes = db.get_all_items_for_group(g.id).unwrap_or_default();
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
            "projects": group_projects.iter().map(|p| &p.name).collect::<Vec<_>>(),
            "notes": note_items
        }));
    }

    let text = serde_json::to_string_pretty(&context).unwrap_or_default();
    tool_result(id, text)
}

fn workspace_read(id: serde_json::Value, item_id: i64, db: &Db) -> JsonRpcResponse {
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

    // Limit file reads to 10 MB to prevent OOM
    const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

    debug!("Reading file: {}", canonical.display());
    if canonical.is_dir() {
        match std::fs::read_dir(&canonical) {
            Ok(entries) => {
                let listing: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                tool_result(id, listing.join("\n"))
            }
            Err(e) => {
                error!("Failed to read dir {}: {}", canonical.display(), e);
                tool_error(id, "Failed to read directory")
            }
        }
    } else {
        match std::fs::metadata(&canonical) {
            Ok(meta) if meta.len() > MAX_READ_SIZE => tool_error(
                id,
                &format!(
                    "File too large ({} bytes, max {})",
                    meta.len(),
                    MAX_READ_SIZE
                ),
            ),
            _ => match std::fs::read_to_string(&canonical) {
                Ok(content) => tool_result(id, content),
                Err(e) => {
                    error!("Failed to read file {}: {}", canonical.display(), e);
                    tool_error(id, "Failed to read file")
                }
            },
        }
    }
}

fn workspace_read_by_path(
    id: serde_json::Value,
    project_id: i64,
    path: &str,
    db: &Db,
) -> JsonRpcResponse {
    info!(
        "workspace_read_by_path: project_id={}, path={}",
        project_id, path
    );

    let (_canonical_root, canonical) = match resolve_project_path(project_id, Some(path), db) {
        Ok(v) => v,
        Err(e) => return tool_error(id, &e),
    };

    // Same 10MB limit as workspace_read
    const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

    debug!("Reading file: {}", canonical.display());
    if canonical.is_dir() {
        match std::fs::read_dir(&canonical) {
            Ok(entries) => {
                let listing: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                tool_result(id, listing.join("\n"))
            }
            Err(e) => {
                error!("Failed to read dir {}: {}", canonical.display(), e);
                tool_error(id, "Failed to read directory")
            }
        }
    } else {
        match std::fs::metadata(&canonical) {
            Ok(meta) if meta.len() > MAX_READ_SIZE => tool_error(
                id,
                &format!(
                    "File too large ({} bytes, max {})",
                    meta.len(),
                    MAX_READ_SIZE
                ),
            ),
            _ => match std::fs::read_to_string(&canonical) {
                Ok(content) => tool_result(id, content),
                Err(e) => {
                    error!("Failed to read file {}: {}", canonical.display(), e);
                    tool_error(id, "Failed to read file")
                }
            },
        }
    }
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

fn list_groups(id: serde_json::Value, db: &Db) -> JsonRpcResponse {
    info!("list_groups");

    let groups = match db.list_groups() {
        Ok(g) => g,
        Err(e) => return tool_error(id, &format!("Failed to list groups: {}", e)),
    };

    let result: Vec<_> = groups
        .iter()
        .map(|g| {
            let projects = db.get_projects_for_group(g.id).unwrap_or_default();
            serde_json::json!({
                "id": g.id,
                "name": g.name,
                "projects": projects.iter().map(|p| serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "path": p.path
                })).collect::<Vec<_>>()
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
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let project = db
        .get_project_by_id(project_id)
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| format!("Project {} not found", project_id))?;

    let root = Path::new(&project.path);
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve project path: {}", e))?;

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
) -> JsonRpcResponse {
    info!(
        "project_tree: project_id={}, path={:?}, max_depth={:?}",
        project_id, path, max_depth
    );

    let (canonical_root, _target) = match resolve_project_path(project_id, path, db) {
        Ok(v) => v,
        Err(e) => return tool_error(id, &e),
    };

    let entries = walk::walk_project_tree(&canonical_root, path, max_depth);

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
) -> JsonRpcResponse {
    info!(
        "project_grep: project_id={}, pattern={}, glob={:?}",
        project_id, pattern, glob
    );

    let (canonical_root, _) = match resolve_project_path(project_id, None, db) {
        Ok(v) => v,
        Err(e) => return tool_error(id, &e),
    };

    let matches = match walk::grep_project(&canonical_root, pattern, glob) {
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

    let projects = match db.list_projects() {
        Ok(p) => p,
        Err(e) => return tool_error(id, &format!("Failed to list projects: {}", e)),
    };

    let result: Vec<_> = projects
        .iter()
        .map(|p| {
            let groups = db.get_groups_for_project(p.id).unwrap_or_default();
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "path": p.path,
                "groups": groups.iter().map(|g| serde_json::json!({
                    "id": g.id,
                    "name": g.name
                })).collect::<Vec<_>>()
            })
        })
        .collect();

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    tool_result(id, text)
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
            json!({"name": "workspace_read", "arguments": {"item_id": 1, "project_id": 1, "path": "foo"}}),
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

    // --- workspace_read ---

    #[test]
    fn workspace_read_item_not_found() {
        let db = test_db();
        let resp = workspace_read(json!(1), 9999, &db);
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

        let resp = workspace_read(json!(1), note_id, &db);
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

        let resp = workspace_read(json!(1), file_id, &db);
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

        let resp = workspace_read(json!(1), dir_id, &db);
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

        let resp = workspace_read(json!(1), file_id, &db);
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
