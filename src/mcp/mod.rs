mod protocol;
mod tools;

use anyhow::Result;
use log::{debug, error, info};
use std::io::{self, BufRead, Write};

use protocol::{JsonRpcRequest, JsonRpcResponse, McpError};

pub fn serve() -> Result<()> {
    info!("MCP server starting (stdio transport)");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        debug!("Received: {}", line);

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => handle_request(req),
            Err(e) => {
                error!("Failed to parse request: {}", e);
                Some(JsonRpcResponse::error(
                    serde_json::Value::Null,
                    McpError::parse_error(&e.to_string()),
                ))
            }
        };

        if let Some(response) = response {
            let response_str = serde_json::to_string(&response)?;
            debug!("Sending: {}", response_str);
            writeln!(stdout, "{}", response_str)?;
            stdout.flush()?;
        }
    }

    info!("MCP server shutting down");
    Ok(())
}

fn handle_request(req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    debug!("Handling method: {}", req.method);

    match req.method.as_str() {
        "initialize" => Some(handle_initialize(req.id)),
        "tools/list" => Some(handle_tools_list(req.id)),
        "tools/call" => Some(tools::handle_tool_call(req.id, req.params)),
        _ if req.method.starts_with("notifications/") => {
            debug!("Ignoring notification: {}", req.method);
            None
        }
        _ => {
            error!("Unknown method: {}", req.method);
            Some(JsonRpcResponse::error(
                req.id,
                McpError::method_not_found(&req.method),
            ))
        }
    }
}

fn handle_initialize(id: serde_json::Value) -> JsonRpcResponse {
    info!("MCP initialize");
    JsonRpcResponse::result(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "ai-workspace",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: serde_json::Value) -> JsonRpcResponse {
    info!("MCP tools/list");
    JsonRpcResponse::result(
        id,
        serde_json::json!({
            "tools": [
                {
                    "name": "workspace_context",
                    "description": "Get workspace metadata: projects, groups, shared items list (no file content)",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                },
                {
                    "name": "workspace_read",
                    "description": "Read a shared file by item_id OR by project_id+path. Provide one or the other, not both.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "item_id": {
                                "type": "integer",
                                "description": "The shared item ID to read (mutually exclusive with project_id+path)"
                            },
                            "project_id": {
                                "type": "integer",
                                "description": "Project ID to read from (use with path)"
                            },
                            "path": {
                                "type": "string",
                                "description": "Relative path within the project (use with project_id)"
                            }
                        }
                    }
                },
                {
                    "name": "workspace_search",
                    "description": "Full-text search over shared notes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "FTS5 search query"
                            }
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "list_groups",
                    "description": "List all groups with their member projects",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                },
                {
                    "name": "list_projects",
                    "description": "List all projects with their groups",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                },
                {
                    "name": "project_tree",
                    "description": "List the file tree of a project, respecting .gitignore",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": {
                                "type": "integer",
                                "description": "The project ID"
                            },
                            "path": {
                                "type": "string",
                                "description": "Optional subdirectory to list (relative to project root)"
                            },
                            "max_depth": {
                                "type": "integer",
                                "description": "Maximum traversal depth (1 = immediate children only, default: unlimited)"
                            }
                        },
                        "required": ["project_id"]
                    }
                },
                {
                    "name": "project_grep",
                    "description": "Search project files for a regex pattern, respecting .gitignore",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": {
                                "type": "integer",
                                "description": "The project ID"
                            },
                            "pattern": {
                                "type": "string",
                                "description": "Regex pattern to search for"
                            },
                            "glob": {
                                "type": "string",
                                "description": "Optional glob to filter files (e.g. \"*.rs\")"
                            }
                        },
                        "required": ["project_id", "pattern"]
                    }
                },
                {
                    "name": "workspace_search_fulltext",
                    "description": "Full-text search over indexed shared .md files (SQLite FTS5, bm25-ranked, unicode61 tokenizer)",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "FTS5 query (supports phrase \"...\" and AND/OR/NOT)"
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Max number of results (default: 20)"
                            }
                        },
                        "required": ["query"]
                    }
                }
            ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(method: &str) -> JsonRpcRequest {
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": {}
        }))
        .unwrap()
    }

    #[test]
    fn handle_initialize_returns_capabilities() {
        let resp = handle_initialize(json!(1));
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "ai-workspace");
    }

    #[test]
    fn handle_tools_list_returns_eight_tools() {
        let resp = handle_tools_list(json!(1));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"workspace_context"));
        assert!(names.contains(&"workspace_read"));
        assert!(names.contains(&"workspace_search"));
        assert!(names.contains(&"list_groups"));
        assert!(names.contains(&"list_projects"));
        assert!(names.contains(&"project_tree"));
        assert!(names.contains(&"project_grep"));
        assert!(names.contains(&"workspace_search_fulltext"));
    }

    #[test]
    fn handle_request_initialize() {
        let req = make_request("initialize");
        let resp = handle_request(req);
        assert!(resp.is_some());
        let resp = resp.unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn handle_request_tools_list() {
        let req = make_request("tools/list");
        let resp = handle_request(req);
        assert!(resp.is_some());
    }

    #[test]
    fn handle_request_notification_returns_none() {
        let req = make_request("notifications/initialized");
        let resp = handle_request(req);
        assert!(resp.is_none());
    }

    #[test]
    fn handle_request_unknown_method() {
        let req = make_request("unknown/method");
        let resp = handle_request(req);
        assert!(resp.is_some());
        let resp = resp.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
