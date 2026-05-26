mod protocol;
mod tools;

use anyhow::Result;
use log::{debug, error, info};
use std::io::{self, BufRead, Write};

use protocol::{JsonRpcRequest, JsonRpcResponse, McpError};

pub use tools::{McpScope, McpScopeKind, McpScopeRequest, resolve_scope};

pub fn serve(scope: McpScope) -> Result<()> {
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
            Ok(req) => handle_request_with_scope(req, &scope),
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

#[cfg(test)]
fn handle_request(req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    handle_request_with_scope(req, &McpScope::global())
}

fn handle_request_with_scope(req: JsonRpcRequest, scope: &McpScope) -> Option<JsonRpcResponse> {
    debug!("Handling method: {}", req.method);

    match req.method.as_str() {
        "initialize" => Some(handle_initialize(req.id)),
        "tools/list" => Some(handle_tools_list(req.id)),
        "tools/call" => Some(tools::handle_tool_call_scoped(req.id, req.params, scope)),
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
                    "description": "Get workspace metadata within the configured MCP server scope: projects, groups, shared items list (no file content)",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": [],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "workspace_read",
                    "description": "Read an in-scope shared item by item_id, or read an in-scope project path only when it is inside shared scopes. Set AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1 to allow project-wide path reads inside the MCP scope.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "item_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "The shared item ID to read (mutually exclusive with project_id+rel_path)"
                            },
                            "project_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Project ID to read from (use with rel_path)"
                            },
                            "rel_path": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Relative project path to read. By default this must be an explicitly shared file or inside a shared directory."
                            },
                            "include_hidden": {
                                "type": "boolean",
                                "description": "Include hidden/dotfile paths (default: false)"
                            },
                            "include_sensitive": {
                                "type": "boolean",
                                "description": "Include credential-like paths such as .env, .ssh, .aws, *.pem, and *.key (default: false)"
                            }
                        },
                        "additionalProperties": false
                    }
                },
                {
                    "name": "workspace_search",
                    "description": "Full-text search over shared notes within the configured MCP server scope",
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
                    "description": "List groups and member projects visible within the configured MCP server scope. Project paths are omitted unless AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                },
                {
                    "name": "list_projects",
                    "description": "List projects visible within the configured MCP server scope. Project paths are omitted unless AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {},
                        "required": []
                    }
                },
                {
                    "name": "project_tree",
                    "description": "List the shared file tree of an in-scope project by default, respecting .gitignore. Set AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1 for full project tree access inside the MCP scope.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "The project ID"
                            },
                            "subdir": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Optional subdirectory to list. By default it must intersect shared scopes."
                            },
                            "max_depth": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Maximum traversal depth (1 = immediate children only, default: unlimited)"
                            },
                            "include_hidden": {
                                "type": "boolean",
                                "description": "Include hidden/dotfile paths (default: false)"
                            },
                            "include_sensitive": {
                                "type": "boolean",
                                "description": "Include credential-like paths such as .env, .ssh, .aws, *.pem, and *.key (default: false)"
                            }
                        },
                        "required": ["project_id"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "project_grep",
                    "description": "Search shared files in an in-scope project for a regex pattern by default, respecting .gitignore. Set AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1 for full project search inside the MCP scope.",
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
                            },
                            "include_hidden": {
                                "type": "boolean",
                                "description": "Include hidden/dotfile paths (default: false)"
                            },
                            "include_sensitive": {
                                "type": "boolean",
                                "description": "Include credential-like paths such as .env, .ssh, .aws, *.pem, and *.key (default: false)"
                            }
                        },
                        "required": ["project_id", "pattern"]
                    }
                },
                {
                    "name": "workspace_search_fulltext",
                    "description": "Full-text search over indexed shared .md files within the configured MCP server scope (SQLite FTS5, bm25-ranked, unicode61 tokenizer). Hidden/dotfile and credential-like .md paths are always excluded; this tool has no include_hidden/include_sensitive opt-in.",
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
                        "required": ["query"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "workspace_service_graph",
                    "description": "Inspect directional service links visible within the configured MCP server scope",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Optional project id, slug, or registered path whose group graph should be returned"
                            },
                            "project_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Optional project ID whose group graph should be returned"
                            },
                            "group_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Optional group ID whose service graph should be returned"
                            }
                        },
                        "additionalProperties": false
                    }
                },
                {
                    "name": "workspace_events",
                    "description": "List workspace events visible within the configured MCP server scope or an in-scope project's open event inbox",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Optional project id, slug, or registered path for inbox mode"
                            },
                            "project_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Optional project ID for inbox mode"
                            },
                            "source": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Optional source service slug filter for list mode"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["open", "closed"],
                                "description": "Optional event status filter for list mode"
                            }
                        },
                        "additionalProperties": false
                    }
                },
                {
                    "name": "workspace_event_details",
                    "description": "Get an in-scope event with affected services and affected artifacts filtered to the configured MCP server scope",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "event_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Workspace event ID"
                            }
                        },
                        "required": ["event_id"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_status",
                    "description": "Return Rust CodeGraph health and counts for one in-scope project. By default counts only currently shared file/dir scopes; set AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1 to expose full-project indexed metadata inside the MCP scope.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Project ID"
                            },
                            "project": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Project id, slug, or registered path"
                            }
                        },
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_search",
                    "description": "Search indexed Rust symbols by text, kind, language, and file path without scanning project files. Results are limited to the configured MCP scope and current shared file/dir visibility unless project-wide tools are enabled.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "integer", "minimum": 1 },
                            "project": { "type": "string", "minLength": 1 },
                            "query": {
                                "type": "string",
                                "description": "Optional FTS text query over symbol names, qualified names, docstrings, and signatures"
                            },
                            "kind": {
                                "type": "string",
                                "enum": ["file", "module", "struct", "enum", "trait", "impl", "function", "method", "const", "type_alias", "import"]
                            },
                            "language": {
                                "type": "string",
                                "description": "Language filter; MVP indexes rust"
                            },
                            "file_path": {
                                "type": "string",
                                "description": "Project-relative source path filter"
                            },
                            "limit": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 200,
                                "default": 20
                            }
                        },
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_node",
                    "description": "Return one visible indexed Rust symbol by node_id. Source snippets are optional, bounded, and subject to the current MCP scope/shared-path policy.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "integer", "minimum": 1 },
                            "project": { "type": "string", "minLength": 1 },
                            "node_id": { "type": "string", "minLength": 1 },
                            "include_source": {
                                "type": "boolean",
                                "description": "Include a compact source snippet around the symbol (default: false)"
                            }
                        },
                        "required": ["node_id"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_callers",
                    "description": "Return visible incoming Rust calls edges for a symbol node_id. Output is bounded and metadata-only by default.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "integer", "minimum": 1 },
                            "project": { "type": "string", "minLength": 1 },
                            "node_id": { "type": "string", "minLength": 1 },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 }
                        },
                        "required": ["node_id"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_callees",
                    "description": "Return visible outgoing Rust calls edges for a symbol node_id. Output is bounded and metadata-only by default.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "integer", "minimum": 1 },
                            "project": { "type": "string", "minLength": 1 },
                            "node_id": { "type": "string", "minLength": 1 },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 }
                        },
                        "required": ["node_id"],
                        "additionalProperties": false
                    }
                },
                {
                    "name": "codegraph_context",
                    "description": "Return compact visible Rust symbols, entry points, related calls, and snippets for a task description. Prefer this before grep when CodeGraph is populated.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "integer", "minimum": 1 },
                            "project": { "type": "string", "minLength": 1 },
                            "task": {
                                "type": "string",
                                "minLength": 1,
                                "description": "Task or question to search symbol context for"
                            },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 20, "default": 8 }
                        },
                        "required": ["task"],
                        "additionalProperties": false
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
    fn handle_tools_list_returns_seventeen_tools() {
        let resp = handle_tools_list(json!(1));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 17);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"workspace_context"));
        assert!(names.contains(&"workspace_read"));
        assert!(names.contains(&"workspace_search"));
        assert!(names.contains(&"list_groups"));
        assert!(names.contains(&"list_projects"));
        assert!(names.contains(&"project_tree"));
        assert!(names.contains(&"project_grep"));
        assert!(names.contains(&"workspace_search_fulltext"));
        assert!(names.contains(&"workspace_service_graph"));
        assert!(names.contains(&"workspace_events"));
        assert!(names.contains(&"workspace_event_details"));
        assert!(names.contains(&"codegraph_status"));
        assert!(names.contains(&"codegraph_search"));
        assert!(names.contains(&"codegraph_node"));
        assert!(names.contains(&"codegraph_callers"));
        assert!(names.contains(&"codegraph_callees"));
        assert!(names.contains(&"codegraph_context"));
    }

    #[test]
    fn workspace_read_schema_uses_claude_api_compatible_top_level_keywords() {
        let resp = handle_tools_list(json!(1));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        let workspace_read = tools
            .iter()
            .find(|tool| tool["name"] == "workspace_read")
            .expect("workspace_read tool should be present");
        let input_schema = &workspace_read["inputSchema"];

        assert!(input_schema["oneOf"].is_null());
        assert!(input_schema["anyOf"].is_null());
        assert!(input_schema["allOf"].is_null());
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
