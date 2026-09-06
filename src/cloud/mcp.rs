use super::auth::AccessClaims;
use super::http::{
    CloudHttpState, audit_request, insufficient_scope_response, unauthorized_response,
    validate_origin,
};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, McpError};
use anyhow::{Result, bail};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use log::{error, warn};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: &str = "2026-07-28";
pub const MAX_MCP_REQUEST_BYTES: usize = 64 * 1024;
const MAX_MCP_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEARCH_QUERY_BYTES: usize = 1024;

pub async fn handle(
    State(state): State<CloudHttpState>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    let started = Instant::now();
    if validate_origin(&headers, &state.public_mcp_uri).is_err() {
        return rpc_http_error(
            StatusCode::FORBIDDEN,
            request.id,
            McpError::invalid_params("Invalid Origin"),
        );
    }
    if let Err(error) = validate_envelope(&headers, &request) {
        warn!("[FIX:cloud-protocol] hosted MCP envelope rejected: {error}");
        let rpc_error = match error {
            EnvelopeError::InvalidRequest(message) => McpError::invalid_request(&message),
            EnvelopeError::InvalidParams(message) => McpError::invalid_params(&message),
            EnvelopeError::HeaderMismatch(message) => McpError::header_mismatch(&message),
            EnvelopeError::UnsupportedVersion(requested) => {
                McpError::unsupported_protocol_version(&requested, &[PROTOCOL_VERSION])
            }
        };
        return rpc_http_error(StatusCode::BAD_REQUEST, request.id, rpc_error);
    }
    let claims = match state.auth.authenticate(&headers).await {
        Ok(claims) => claims,
        Err(error) => {
            warn!("[FIX:cloud-auth] hosted MCP authentication rejected: {error}");
            return unauthorized_response(
                &state.public_mcp_uri,
                "ai-workspace:read",
                "Token validation failed",
            );
        }
    };
    let audit = audit_details(&request);
    if claims.require_scope("ai-workspace:read").is_err() {
        warn!(
            "[FIX:cloud-auth] hosted MCP rejected insufficient scope workspace_id={}",
            claims.workspace_id
        );
        let response = insufficient_scope_response(&state.public_mcp_uri, "ai-workspace:read");
        audit_request(&claims, started, response.status(), audit);
        return response;
    }
    let id = request.id.clone();
    let dispatched =
        tokio::time::timeout(Duration::from_secs(20), dispatch(&state, &claims, &request)).await;
    let response = match dispatched {
        Err(_) => rpc_http_error(
            StatusCode::REQUEST_TIMEOUT,
            id,
            McpError::internal_error("Hosted MCP operation timed out"),
        ),
        Ok(result) => match result {
            Ok(result) => result_response(id, result),
            Err(error) => {
                let message = error.to_string();
                if error.is::<super::store::ContextLimitExceeded>() {
                    warn!(
                        "[FIX:cloud-context] hosted MCP collection exceeds storage budget workspace_id={}",
                        claims.workspace_id
                    );
                    rpc_http_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        id,
                        McpError::invalid_params(&format!(
                            "{message}; use search or an exact document/event key"
                        )),
                    )
                } else if message.starts_with("Unknown or unavailable hosted tool:") {
                    warn!(
                        "hosted MCP unsafe or unknown tool rejected id={}",
                        safe_id(&id)
                    );
                    rpc_http_error(
                        StatusCode::BAD_REQUEST,
                        id,
                        McpError::invalid_params(&message),
                    )
                } else if message.starts_with("Unsupported hosted MCP method:") {
                    rpc_http_error(
                        StatusCode::NOT_FOUND,
                        id,
                        McpError::method_not_found(&message),
                    )
                } else if message.starts_with("Tool argument")
                    || message.starts_with("Tool name is required")
                {
                    rpc_http_error(
                        StatusCode::BAD_REQUEST,
                        id,
                        McpError::invalid_params(&message),
                    )
                } else {
                    error!("hosted MCP request failed id={}: {error}", safe_id(&id));
                    rpc_http_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        id,
                        McpError::internal_error("Hosted MCP request failed"),
                    )
                }
            }
        },
    };
    audit_request(&claims, started, response.status(), audit);
    response
}

fn audit_details(request: &JsonRpcRequest) -> Value {
    let method = match request.method.as_str() {
        "server/discover" | "tools/list" | "tools/call" => request.method.as_str(),
        _ => "unknown",
    };
    let name = request
        .params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tool = match name {
        "workspace_context"
        | "workspace_read"
        | "workspace_search"
        | "workspace_search_fulltext"
        | "workspace_service_graph"
        | "workspace_events"
        | "workspace_event_details"
            if method == "tools/call" =>
        {
            name
        }
        _ => "none_or_unknown",
    };
    let key = match tool {
        "workspace_read" => Some("document_key"),
        "workspace_event_details" => Some("event_key"),
        _ => None,
    };
    json!({
        "operation": "mcp", "method": method, "tool": tool,
        "scope": key.unwrap_or("workspace"),
        "target_sha256": key.and_then(|key| request.params["arguments"][key].as_str())
            .map(|value| super::snapshot::sha256_hex(value.as_bytes()))
    })
}

fn result_response(id: Value, result: Value) -> Response {
    let response = JsonRpcResponse::result(id.clone(), stamp(result));
    let body = serde_json::to_vec(&response).expect("JSON value serializes");
    if body.len() > MAX_MCP_RESPONSE_BYTES {
        warn!("[FIX:cloud-limits] hosted MCP response exceeds byte limit");
        return rpc_http_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            id,
            McpError::invalid_params(
                "Hosted MCP response exceeds 8 MiB; use search or an exact document/event key",
            ),
        );
    }
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[derive(Debug)]
enum EnvelopeError {
    InvalidRequest(String),
    InvalidParams(String),
    HeaderMismatch(String),
    UnsupportedVersion(String),
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message)
            | Self::InvalidParams(message)
            | Self::HeaderMismatch(message) => formatter.write_str(message),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported MCP protocol version: {version}")
            }
        }
    }
}

fn validate_envelope(
    headers: &HeaderMap,
    request: &JsonRpcRequest,
) -> std::result::Result<(), EnvelopeError> {
    if request.jsonrpc.as_deref() != Some("2.0") {
        return Err(EnvelopeError::InvalidRequest("jsonrpc must be 2.0".into()));
    }
    let header_version = required_header(headers, "MCP-Protocol-Version")?;
    if required_header(headers, "Mcp-Method")? != request.method {
        return Err(EnvelopeError::HeaderMismatch(
            "Mcp-Method header does not match request method".into(),
        ));
    }
    let meta = request
        .params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| EnvelopeError::InvalidParams("params._meta is required".into()))?;
    let body_version = meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| EnvelopeError::InvalidParams("Body protocol version is required".into()))?;
    if header_version != body_version {
        return Err(EnvelopeError::HeaderMismatch(
            "Header and body protocol versions do not match".into(),
        ));
    }
    if header_version != PROTOCOL_VERSION {
        return Err(EnvelopeError::UnsupportedVersion(header_version.into()));
    }
    if !meta
        .get("io.modelcontextprotocol/clientCapabilities")
        .is_some_and(Value::is_object)
    {
        return Err(EnvelopeError::InvalidParams(
            "Client capabilities metadata is required".into(),
        ));
    }
    if let Some(client) = meta.get("io.modelcontextprotocol/clientInfo") {
        let client = client.as_object().ok_or_else(|| {
            EnvelopeError::InvalidParams("Client metadata must be an object".into())
        })?;
        if client.get("name").and_then(Value::as_str).is_none()
            || client.get("version").and_then(Value::as_str).is_none()
        {
            return Err(EnvelopeError::InvalidParams(
                "Client metadata requires name and version".into(),
            ));
        }
    }
    if request.method == "tools/call" {
        let tool = request
            .params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                EnvelopeError::InvalidParams("tools/call requires a tool name".into())
            })?;
        if required_header(headers, "Mcp-Name")? != tool {
            return Err(EnvelopeError::HeaderMismatch(
                "Mcp-Name header does not match tool name".into(),
            ));
        }
    }
    Ok(())
}

async fn dispatch(
    state: &CloudHttpState,
    claims: &AccessClaims,
    request: &JsonRpcRequest,
) -> Result<Value> {
    match request.method.as_str() {
        "server/discover" => Ok(discovery()),
        "tools/list" => Ok(tool_catalog()),
        "tools/call" => call_tool(state, claims, &request.params).await,
        method => bail!("Unsupported hosted MCP method: {method}"),
    }
}

async fn call_tool(state: &CloudHttpState, claims: &AccessClaims, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Tool name is required"))?;
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    let value = match name {
        "workspace_context" => super::store::context_response(
            state.store.workspace_context(claims.workspace_id).await?,
        ),
        "workspace_read" => {
            let key = required_string(arguments, "document_key")?;
            state
                .store
                .read_document(claims.workspace_id, key)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Cloud document not found"))?
        }
        "workspace_search" => json!(
            state
                .store
                .search_documents(
                    claims.workspace_id,
                    required_string(arguments, "query")?,
                    Some("note"),
                    limit(arguments),
                )
                .await?
        ),
        "workspace_search_fulltext" => json!(
            state
                .store
                .search_documents(
                    claims.workspace_id,
                    required_string(arguments, "query")?,
                    Some("markdown"),
                    limit(arguments),
                )
                .await?
        ),
        "workspace_service_graph" => {
            json!(state.store.service_graph(claims.workspace_id).await?)
        }
        "workspace_events" => json!(state.store.events(claims.workspace_id).await?),
        "workspace_event_details" => {
            let key = required_string(arguments, "event_key")?;
            state
                .store
                .event_details(claims.workspace_id, key)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Cloud event not found"))?
        }
        _ => bail!("Unknown or unavailable hosted tool: {name}"),
    };
    Ok(call_result(value))
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    let value = value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Tool argument {name} is required"))?;
    if name == "query" && value.len() > MAX_SEARCH_QUERY_BYTES {
        bail!("Tool argument {name} exceeds {MAX_SEARCH_QUERY_BYTES} bytes");
    }
    Ok(value)
}

fn limit(arguments: &Value) -> i64 {
    arguments.get("limit").and_then(Value::as_i64).unwrap_or(20)
}

fn call_result(value: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&value).expect("JSON value serializes")
        }],
        "structuredContent": value
    })
}

fn stamp(mut result: Value) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("resultType".into(), json!("complete"));
        object.insert(
            "_meta".into(),
            json!({
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                "io.modelcontextprotocol/serverInfo": server_info()
            }),
        );
    }
    result
}

fn server_info() -> Value {
    json!({ "name": "ai-workspace-cloud", "version": env!("CARGO_PKG_VERSION") })
}

fn discovery() -> Value {
    json!({
        "supportedVersions": [PROTOCOL_VERSION],
        "capabilities": { "tools": {} },
        "instructions": "Read synchronized workspace context. Hosted tools never access local files or execute commands.",
        "ttlMs": 300_000,
        "cacheScope": "private",
        "_meta": { "io.modelcontextprotocol/serverInfo": server_info() }
    })
}

fn tool_catalog() -> Value {
    let no_args = json!({"type":"object", "properties":{}, "additionalProperties":false});
    let query = json!({
        "type":"object",
        "properties": {"query":{"type":"string","minLength":1,"maxLength":1024,"description":"Maximum 1024 UTF-8 bytes"}, "limit":{"type":"integer","minimum":1,"maximum":100}},
        "required":["query"], "additionalProperties":false
    });
    json!({
        "tools": [
            tool("workspace_context", "Read synchronized workspace context", no_args.clone()),
            tool("workspace_read", "Read a synchronized document by document_key", json!({"type":"object","properties":{"document_key":{"type":"string","minLength":1}},"required":["document_key"],"additionalProperties":false})),
            tool("workspace_search", "Search synchronized notes", query.clone()),
            tool("workspace_search_fulltext", "Search synchronized Markdown", query),
            tool("workspace_service_graph", "Read synchronized service links", no_args.clone()),
            tool("workspace_events", "List synchronized events", no_args),
            tool("workspace_event_details", "Read an event by event_key", json!({"type":"object","properties":{"event_key":{"type":"string","minLength":1}},"required":["event_key"],"additionalProperties":false}))
        ],
        "ttlMs": 300_000,
        "cacheScope": "private"
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<&'a str, EnvelopeError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| EnvelopeError::HeaderMismatch(format!("Missing or invalid {name} header")))
}

fn rpc_http_error(status: StatusCode, id: Value, error: McpError) -> Response {
    (status, Json(JsonRpcResponse::error(id, error))).into_response()
}

fn safe_id(id: &Value) -> String {
    match id {
        Value::String(value) => value.chars().take(64).collect(),
        Value::Number(value) => value.to_string(),
        _ => "null".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: Some("2.0".into()),
            id: json!(1),
            method: method.into(),
            params,
        }
    }

    fn headers(method: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("MCP-Protocol-Version", PROTOCOL_VERSION.parse().unwrap());
        headers.insert("Mcp-Method", method.parse().unwrap());
        headers
    }

    #[test]
    fn tool_strings_reject_excessive_search_work() {
        for key in ["document_key", "event_key"] {
            assert!(required_string(&json!({key: "x".repeat(1025)}), key).is_ok());
        }
        assert!(required_string(&json!({"query": "x".repeat(1024)}), "query").is_ok());
        assert!(required_string(&json!({"query": "x".repeat(1025)}), "query").is_err());
        assert!(required_string(&json!({"query": "é".repeat(513)}), "query").is_err());
    }

    #[tokio::test]
    async fn response_size_is_checked_after_json_escaping_and_metadata() {
        let base = serde_json::to_vec(&JsonRpcResponse::result(
            json!(1),
            stamp(json!({"text": ""})),
        ))
        .unwrap()
        .len();
        let exact = result_response(
            json!(1),
            json!({"text": "x".repeat(MAX_MCP_RESPONSE_BYTES - base)}),
        );
        assert_eq!(exact.status(), StatusCode::OK);
        let body = axum::body::to_bytes(exact.into_body(), MAX_MCP_RESPONSE_BYTES)
            .await
            .unwrap();
        assert_eq!(body.len(), MAX_MCP_RESPONSE_BYTES);
        for text in [
            "x".repeat(MAX_MCP_RESPONSE_BYTES - base + 1),
            "\"".repeat(MAX_MCP_RESPONSE_BYTES / 2),
        ] {
            let response = result_response(json!(1), json!({"text": text}));
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["id"], 1);
            assert!(body["error"]["message"].as_str().unwrap().contains("8 MiB"));
        }
    }

    #[test]
    fn audit_records_scope_and_tool_without_raw_keys_queries_or_ids() {
        let request = request(
            "tools/call",
            json!({
                "name": "workspace_read",
                "arguments": {"document_key": "file:demo:private.md", "query": "secret-query"}
            }),
        );
        let details = audit_details(&request);
        assert_eq!(details["tool"], "workspace_read");
        assert_eq!(details["scope"], "document_key");
        assert_eq!(
            details["target_sha256"],
            super::super::snapshot::sha256_hex(b"file:demo:private.md")
        );
        assert!(!details.to_string().contains("private.md"));
        assert!(!details.to_string().contains("secret-query"));
        let mut unknown = request;
        unknown.method = "untrusted-method".into();
        unknown.params["name"] = json!("untrusted-tool");
        assert!(!audit_details(&unknown).to_string().contains("untrusted"));
    }

    #[test]
    fn envelope_requires_matching_modern_metadata_and_tool_header() {
        let params = json!({
            "name": "workspace_context",
            "arguments": {},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        let request = request("tools/call", params);
        let mut headers = headers("tools/call");
        assert!(validate_envelope(&headers, &request).is_err());
        headers.insert("Mcp-Name", "workspace_context".parse().unwrap());
        assert!(validate_envelope(&headers, &request).is_ok());
        headers.insert("Mcp-Name", "project_file_write".parse().unwrap());
        assert!(validate_envelope(&headers, &request).is_err());
    }

    #[test]
    fn catalog_is_exact_and_call_results_have_content() {
        let catalog = tool_catalog();
        let names = catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "workspace_context",
                "workspace_read",
                "workspace_search",
                "workspace_search_fulltext",
                "workspace_service_graph",
                "workspace_events",
                "workspace_event_details"
            ]
        );
        assert!(call_result(json!({"ok":true}))["content"].is_array());
        let discovery_result = discovery();
        assert_eq!(discovery_result["supportedVersions"][0], PROTOCOL_VERSION);
        assert_eq!(discovery_result["cacheScope"], "private");
        assert_eq!(catalog["cacheScope"], "private");
        assert_eq!(stamp(catalog)["resultType"], "complete");
        assert_eq!(
            stamp(call_result(json!({"ok":true})))["resultType"],
            "complete"
        );
        assert_eq!(stamp(discovery_result)["resultType"], "complete");
    }

    #[test]
    fn envelope_distinguishes_header_mismatch_and_unsupported_version() {
        let mut params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        let rpc_request = request("tools/list", params.clone());
        let mut request_headers = headers("server/discover");
        assert!(matches!(
            validate_envelope(&request_headers, &rpc_request),
            Err(EnvelopeError::HeaderMismatch(_))
        ));

        request_headers.insert("Mcp-Method", "tools/list".parse().unwrap());
        request_headers.insert("MCP-Protocol-Version", "2099-01-01".parse().unwrap());
        params["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
        let request = request("tools/list", params);
        assert!(matches!(
            validate_envelope(&request_headers, &request),
            Err(EnvelopeError::UnsupportedVersion(version)) if version == "2099-01-01"
        ));
    }
}
