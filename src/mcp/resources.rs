//! Read-only event resources and connection-local subscriptions.
use super::protocol::{JsonRpcRequest, JsonRpcResponse, McpError};
use super::tools::{McpScope, event_json, workspace_event_visible};
use crate::db::Db;
use log::warn;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) const EVENTS_URI: &str = "workspace://events";
const PROJECT_PREFIX: &str = "workspace://projects/";
const MAX_SUBSCRIPTIONS: usize = 128;

pub(crate) fn contents(uri: &str, events: &Value) -> Value {
    json!({"contents": [{
        "uri": uri, "mimeType": "application/json",
        "text": serde_json::to_string(events).expect("JSON value serializes")
    }]})
}

pub(crate) fn fingerprint(value: &Value) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(value).expect("JSON value serializes")).into()
}

pub(crate) fn updated(uri: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/resources/updated",
        "params": {"uri": uri}})
}

fn project_uri(slug: &str) -> String {
    format!("{PROJECT_PREFIX}{slug}/events")
}

fn resource(uri: &str, name: &str, description: &str) -> Value {
    json!({"uri": uri, "name": name, "description": description,
        "mimeType": "application/json"})
}

fn internal(error: anyhow::Error) -> McpError {
    warn!("Failed to read event resource: {error}");
    McpError::internal_error("Failed to read event resource")
}

fn read(db: &Db, scope: &McpScope, uri: &str) -> Result<Value, McpError> {
    let events = if uri == EVENTS_URI {
        db.list_workspace_events(None, None)
            .map_err(internal)?
            .into_iter()
            .filter(|event| workspace_event_visible(db, scope, event))
            .collect::<Vec<_>>()
    } else {
        let slug = uri
            .strip_prefix(PROJECT_PREFIX)
            .and_then(|value| value.strip_suffix("/events"))
            .filter(|slug| !slug.is_empty() && !slug.contains(['/', '?', '#', '%']))
            .ok_or_else(|| McpError::invalid_params("Unknown event resource URI"))?;
        let project = db
            .get_project_by_slug(slug)
            .map_err(internal)?
            .filter(|project| scope.allows_project(project.id))
            .ok_or_else(|| McpError::invalid_params("Event resource not found in MCP scope"))?;
        db.list_workspace_event_inbox(project.id)
            .map_err(internal)?
    };
    Ok(Value::Array(
        events.iter().map(|event| event_json(db, event)).collect(),
    ))
}

#[derive(Default)]
pub(super) struct EventResources {
    subscriptions: BTreeMap<String, [u8; 32]>,
    data_version: Option<i64>,
}

impl EventResources {
    pub fn handle(
        &mut self,
        request: &JsonRpcRequest,
        db: &Db,
        scope: &McpScope,
    ) -> JsonRpcResponse {
        let result = self.dispatch(request, db, scope);
        match result {
            Ok(result) => JsonRpcResponse::result(request.id.clone(), result),
            Err(error) => JsonRpcResponse::error(request.id.clone(), error),
        }
    }

    fn dispatch(
        &mut self,
        request: &JsonRpcRequest,
        db: &Db,
        scope: &McpScope,
    ) -> Result<Value, McpError> {
        if request.id.is_null()
            || !(request.id.is_string() || request.id.is_i64() || request.id.is_u64())
        {
            return Err(McpError::invalid_request(
                "Resource requests require a string or integer id",
            ));
        }
        if !request.params.is_null() && !request.params.is_object() {
            return Err(McpError::invalid_params("params must be an object"));
        }
        match request.method.as_str() {
            "resources/list" => {
                if request.params.get("cursor").is_some() {
                    return Err(McpError::invalid_params(
                        "Event resources do not accept a cursor",
                    ));
                }
                let mut resources = vec![resource(
                    EVENTS_URI,
                    "Workspace events",
                    "Event history visible in the configured MCP scope",
                )];
                for project in db.list_projects().map_err(internal)? {
                    if scope.allows_project(project.id) {
                        resources.push(resource(
                            &project_uri(&project.slug),
                            &format!("{} event inbox", project.slug),
                            "Open events affecting this project",
                        ));
                    }
                }
                Ok(json!({"resources": resources}))
            }
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" | "resources/subscribe" | "resources/unsubscribe" => {
                let uri = request
                    .params
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| McpError::invalid_params("uri is required"))?;
                if request.method == "resources/unsubscribe" {
                    self.subscriptions.remove(uri);
                    return Ok(json!({}));
                }
                let events = read(db, scope, uri)?;
                if request.method == "resources/read" {
                    return Ok(contents(uri, &events));
                }
                if self.subscriptions.len() >= MAX_SUBSCRIPTIONS
                    && !self.subscriptions.contains_key(uri)
                {
                    return Err(McpError::invalid_params(
                        "At most 128 event resource subscriptions per connection",
                    ));
                }
                // Do not reset an existing baseline: a repeated subscribe must not swallow an update.
                self.subscriptions
                    .entry(uri.to_owned())
                    .or_insert_with(|| fingerprint(&events));
                self.data_version = None;
                Ok(json!({}))
            }
            _ => Err(McpError::method_not_found(&request.method)),
        }
    }

    pub fn poll(&mut self, db: &Db, scope: &McpScope) -> Vec<Value> {
        if self.subscriptions.is_empty() {
            return Vec::new();
        }
        let version = match db.data_version() {
            Ok(version) if self.data_version != Some(version) => version,
            Ok(_) => return Vec::new(),
            Err(error) => {
                warn!("Event subscription database check failed: {error}");
                return Vec::new();
            }
        };
        let mut messages = Vec::new();
        let mut failed = false;
        self.subscriptions.retain(|uri, previous| {
            match read(db, scope, uri) {
                Ok(events) => {
                    let current = fingerprint(&events);
                    if *previous != current {
                        *previous = current;
                        messages.push(updated(uri));
                    }
                    true
                }
                Err(error) if error.code == -32602 => {
                    // The project disappeared. Invalidate the client's cached resource once.
                    messages.push(updated(uri));
                    false
                }
                Err(_) => {
                    failed = true;
                    true
                }
            }
        });
        if !failed {
            self.data_version = Some(version);
        }
        messages
    }
}
