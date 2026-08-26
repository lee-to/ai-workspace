use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

impl JsonRpcResponse {
    pub fn result(id: serde_json::Value, result: serde_json::Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, error: McpError) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct McpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl McpError {
    pub fn parse_error(msg: &str) -> Self {
        McpError {
            code: -32700,
            message: format!("Parse error: {}", msg),
            data: None,
        }
    }

    pub fn invalid_request(msg: &str) -> Self {
        McpError {
            code: -32600,
            message: format!("Invalid request: {}", msg),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        McpError {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn invalid_params(msg: &str) -> Self {
        McpError {
            code: -32602,
            message: format!("Invalid params: {}", msg),
            data: None,
        }
    }

    #[allow(dead_code)]
    pub fn internal_error(msg: &str) -> Self {
        McpError {
            code: -32603,
            message: format!("Internal error: {}", msg),
            data: None,
        }
    }

    pub fn header_mismatch(msg: &str) -> Self {
        McpError {
            code: -32020,
            message: format!("Header mismatch: {msg}"),
            data: None,
        }
    }

    pub fn unsupported_protocol_version(requested: &str, supported: &[&str]) -> Self {
        McpError {
            code: -32022,
            message: "Unsupported protocol version".into(),
            data: Some(serde_json::json!({
                "supported": supported,
                "requested": requested,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_rpc_response_result() {
        let resp = JsonRpcResponse::result(json!(1), json!({"ok": true}));
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, json!(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_response_error() {
        let resp = JsonRpcResponse::error(json!(2), McpError::parse_error("bad"));
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32700);
        assert!(err.message.contains("bad"));
    }

    #[test]
    fn mcp_error_codes() {
        assert_eq!(McpError::parse_error("x").code, -32700);
        assert_eq!(McpError::invalid_request("x").code, -32600);
        assert_eq!(McpError::method_not_found("x").code, -32601);
        assert_eq!(McpError::invalid_params("x").code, -32602);
        assert_eq!(McpError::internal_error("x").code, -32603);
        assert_eq!(McpError::header_mismatch("x").code, -32020);
        let unsupported = McpError::unsupported_protocol_version("old", &["new"]);
        assert_eq!(unsupported.code, -32022);
        assert_eq!(unsupported.data.unwrap()["supported"], json!(["new"]));
    }

    #[test]
    fn json_rpc_response_serialization_skips_none() {
        let resp = JsonRpcResponse::result(json!(1), json!("ok"));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(!s.contains("error"));

        let resp = JsonRpcResponse::error(json!(1), McpError::internal_error("x"));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(!s.contains("result"));
    }

    #[test]
    fn json_rpc_request_deserialization() {
        let json_str = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.method, "test");
        assert_eq!(req.id, json!(1));
    }

    #[test]
    fn json_rpc_request_missing_optional_fields() {
        let json_str = r#"{"method":"notify"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.method, "notify");
        assert!(req.jsonrpc.is_none());
        assert_eq!(req.params, json!(null));
    }
}
