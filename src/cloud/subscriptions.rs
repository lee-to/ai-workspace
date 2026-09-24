//! Modern MCP resource notifications over an authenticated SSE response.
use super::auth::AccessClaims;
use super::store::CloudStore;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, McpError};
use crate::mcp::resources::{EVENTS_URI, fingerprint, updated};
use anyhow::{Result, bail};
use axum::http::StatusCode;
use axum::response::{
    IntoResponse, Response, Sse,
    sse::{Event, KeepAlive},
};
use futures_util::stream;
use log::warn;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use uuid::Uuid;

// Bound concurrent database polling and release capacity when the HTTP body is dropped.
static STREAM_SLOTS: Semaphore = Semaphore::const_new(64);
const MAX_STREAM_LIFETIME: Duration = Duration::from_secs(300);

pub(super) fn resource_catalog() -> Value {
    json!({"resources": [{
        "uri": EVENTS_URI, "name": "Workspace events",
        "description": "Synchronized event history for the authenticated workspace",
        "mimeType": "application/json"
    }], "ttlMs": 300_000, "cacheScope": "private"})
}

pub(super) fn validate_uri(params: &Value) -> Result<()> {
    if params.get("uri").and_then(Value::as_str) != Some(EVENTS_URI) {
        bail!("Resource argument uri must be {EVENTS_URI}");
    }
    Ok(())
}

fn requested_events(params: &Value) -> Result<bool> {
    let notifications = params
        .get("notifications")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Subscription notifications filter is required"))?;
    for (key, value) in notifications {
        match key.as_str() {
            "toolsListChanged" | "promptsListChanged" | "resourcesListChanged"
                if value.is_boolean() => {}
            "resourceSubscriptions" => {
                let uris = value.as_array().filter(|uris| uris.len() <= 128)
                    .ok_or_else(|| anyhow::anyhow!("Subscription resourceSubscriptions must be an array of at most 128 URIs"))?;
                if uris.iter().any(|uri| uri.as_str() != Some(EVENTS_URI)) {
                    bail!("Subscription resource URI is not available");
                }
            }
            _ => bail!("Subscription notification filter is invalid"),
        }
    }
    Ok(notifications
        .get("resourceSubscriptions")
        .and_then(Value::as_array)
        .is_some_and(|uris| !uris.is_empty()))
}

fn acknowledged(id: &Value, events: bool) -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged",
    "params": {
        "_meta": {"io.modelcontextprotocol/subscriptionId": id},
        "notifications": if events { json!({"resourceSubscriptions": [EVENTS_URI]}) } else { json!({}) }
    }})
}

fn completion(id: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {
        "resultType": "complete", "_meta": {
            "io.modelcontextprotocol/subscriptionId": id,
            "io.modelcontextprotocol/protocolVersion": super::mcp::PROTOCOL_VERSION
        }
    }})
}

struct EventStream {
    store: CloudStore,
    workspace_id: Uuid,
    id: Value,
    events: bool,
    initial: bool,
    finished: bool,
    previous: [u8; 32],
    versions: Vec<(String, i64)>,
    interval: Interval,
    deadline: Instant,
    _permit: SemaphorePermit<'static>,
}

impl EventStream {
    async fn next_message(&mut self) -> Option<Value> {
        if self.finished {
            return None;
        }
        if self.initial {
            self.initial = false;
            return Some(acknowledged(&self.id, self.events));
        }
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(self.deadline) => {
                    self.finished = true;
                    return Some(completion(&self.id));
                }
                _ = self.interval.tick() => {}
            }
            if !self.events {
                continue;
            }
            let check_deadline = self.deadline.min(Instant::now() + Duration::from_secs(20));
            match tokio::time::timeout_at(check_deadline, self.check_update()).await {
                Ok(Ok(Some(message))) => return Some(message),
                Ok(Ok(None)) => {}
                result => {
                    if let Ok(Err(error)) = result {
                        warn!(
                            "Cloud event subscription failed workspace_id={}: {error}",
                            self.workspace_id
                        );
                    }
                    self.finished = true;
                    if Instant::now() >= self.deadline {
                        return Some(completion(&self.id));
                    }
                    return Some(
                        serde_json::to_value(JsonRpcResponse::error(
                            self.id.clone(),
                            McpError::internal_error(
                                "Event subscription interrupted; reconnect and read the resource",
                            ),
                        ))
                        .unwrap(),
                    );
                }
            }
        }
    }

    async fn check_update(&mut self) -> Result<Option<Value>> {
        let versions = self
            .store
            .event_snapshot_versions(self.workspace_id)
            .await?;
        if versions == self.versions {
            return Ok(None);
        }
        let current = fingerprint(&json!(self.store.events(self.workspace_id).await?));
        self.versions = versions;
        if current == self.previous {
            return Ok(None);
        }
        self.previous = current;
        let mut notification = updated(EVENTS_URI);
        notification["params"]["_meta"] =
            json!({"io.modelcontextprotocol/subscriptionId": self.id});
        Ok(Some(notification))
    }
}

pub(super) async fn listen(
    store: &CloudStore,
    claims: &AccessClaims,
    request: &JsonRpcRequest,
    public_mcp_uri: &str,
) -> Result<Response> {
    if !(request.id.is_string() || request.id.is_i64() || request.id.is_u64()) {
        bail!("Subscription request requires a string or integer id");
    }
    let events = requested_events(&request.params)?;
    let permit = match STREAM_SLOTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return Ok(super::mcp::rpc_http_error(
                StatusCode::TOO_MANY_REQUESTS,
                request.id.clone(),
                McpError::internal_error("Too many active event subscriptions"),
            ));
        }
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let lifetime = Duration::from_secs(claims._exp)
        .saturating_sub(now)
        .min(MAX_STREAM_LIFETIME);
    if lifetime.is_zero() {
        return Ok(super::http::unauthorized_response(
            public_mcp_uri,
            "ai-workspace:read",
            "Token expired",
        ));
    }
    let deadline = Instant::now() + lifetime;
    // Read versions before data: a commit between these queries causes another
    // check, never a missed update. The same ordering is used during polling.
    let (versions, previous) = if events {
        (
            store.event_snapshot_versions(claims.workspace_id).await?,
            fingerprint(&json!(store.events(claims.workspace_id).await?)),
        )
    } else {
        (Vec::new(), fingerprint(&json!([])))
    };
    let mut interval = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let state = EventStream {
        store: store.clone(),
        workspace_id: claims.workspace_id,
        id: request.id.clone(),
        events,
        initial: true,
        finished: false,
        previous,
        versions,
        interval,
        deadline,
        _permit: permit,
    };
    let stream = stream::unfold(state, |mut state| async move {
        let message = state.next_message().await?;
        Some((
            Ok::<_, Infallible>(Event::default().event("message").data(message.to_string())),
            state,
        ))
    });
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::super::models::{
        CLOUD_SNAPSHOT_SCHEMA_VERSION, CloudEvent, CloudProject, CloudProjectSnapshot,
        event_fingerprint_input, keys,
    };
    use super::super::snapshot::sha256_hex;
    use super::super::store::ReplaceSnapshotOutcome;
    use super::*;
    use crate::models::{EventSeverity, EventStatus, WorkspaceEventKind};
    use futures_util::StreamExt;

    #[test]
    fn subscription_filters_reject_unknown_resources_and_malformed_types() {
        for params in [
            json!({}),
            json!({"notifications": []}),
            json!({"notifications": {"resourceSubscriptions": "workspace://events"}}),
            json!({"notifications": {"resourceSubscriptions": ["workspace://other/events"]}}),
            json!({"notifications": {"toolsListChanged": "true"}}),
            json!({"notifications": {"customEvents": true}}),
        ] {
            assert!(requested_events(&params).is_err(), "{params}");
        }
        assert!(!requested_events(&json!({"notifications": {"toolsListChanged": true}})).unwrap());
        assert!(
            requested_events(&json!({"notifications": {"resourceSubscriptions": [EVENTS_URI]}}))
                .unwrap()
        );
    }

    fn snapshot(with_event: bool) -> CloudProjectSnapshot {
        let mut snapshot = CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: CloudProject {
                cloud_key: "project:auth".into(),
                name: "Auth".into(),
                slug: "auth".into(),
            },
            groups: vec![],
            shares: vec![],
            documents: vec![],
            notes: vec![],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        };
        if with_event {
            let mut event = CloudEvent {
                cloud_key: String::new(),
                source_project_slug: "auth".into(),
                source_project_name: "Auth".into(),
                group_slugs: vec![],
                kind: WorkspaceEventKind::ServiceChanged,
                title: "Contract changed".into(),
                body: None,
                severity: EventSeverity::Info,
                status: EventStatus::Open,
                created_at: "2026-09-24T00:00:00Z".into(),
                updated_at: "2026-09-24T00:00:00Z".into(),
                targets: vec![],
                artifacts: vec![],
            };
            event.cloud_key = keys::event(
                "auth",
                &sha256_hex(&event_fingerprint_input(&event).unwrap()),
                0,
            )
            .unwrap();
            snapshot.events.push(event);
        }
        snapshot
    }

    async fn push(
        store: &CloudStore,
        workspace_id: Uuid,
        snapshot: &CloudProjectSnapshot,
        revision: Option<i64>,
    ) -> i64 {
        let outcome = store
            .replace_project_snapshot(
                workspace_id,
                &format!("subscription-{}", workspace_id.simple()),
                snapshot,
                &sha256_hex(&serde_json::to_vec(snapshot).unwrap()),
                revision,
                false,
                "subscription-test",
            )
            .await
            .unwrap();
        match outcome {
            ReplaceSnapshotOutcome::Accepted { revision, .. } => revision,
            other => panic!("Unexpected push outcome: {other:?}"),
        }
    }

    async fn message(stream: &mut axum::body::BodyDataStream) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = std::str::from_utf8(&frame).unwrap();
        let data = frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        serde_json::from_str(data).unwrap()
    }

    #[tokio::test]
    async fn postgres_sse_delivers_tenant_changes_and_ends_at_expiry() {
        let Ok(url) = std::env::var("AI_WORKSPACE_CLOUD_TEST_DATABASE_URL") else {
            return;
        };
        let store = CloudStore::connect(&url).await.unwrap();
        // Separate connection pool represents a push handled by another replica.
        let writer = CloudStore::connect(&url).await.unwrap();
        let workspace_id = Uuid::new_v4();
        let mut revision = push(&writer, workspace_id, &snapshot(false), None).await;
        let claims = AccessClaims {
            sub: "subscription-test".into(),
            workspace_id,
            workspace_slug: format!("subscription-{}", workspace_id.simple()),
            scope: "ai-workspace:read".into(),
            _exp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 60,
            _nbf: None,
        };
        let request = JsonRpcRequest {
            jsonrpc: Some("2.0".into()),
            id: json!("watch"),
            method: "subscriptions/listen".into(),
            params: json!({"notifications": {"resourceSubscriptions": [EVENTS_URI], "toolsListChanged": true}}),
        };
        let response = listen(&store, &claims, &request, "https://cloud.example/mcp")
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut stream = response.into_body().into_data_stream();
        let ack = message(&mut stream).await;
        assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
        assert_eq!(
            ack["params"]["notifications"],
            json!({"resourceSubscriptions": [EVENTS_URI]})
        );
        assert_eq!(
            ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
            "watch"
        );

        push(&writer, Uuid::new_v4(), &snapshot(true), None).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1300), stream.next())
                .await
                .is_err()
        );
        let mut unrelated_change = snapshot(false);
        unrelated_change.project.name = "Renamed".into();
        revision = push(&writer, workspace_id, &unrelated_change, Some(revision)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1300), stream.next())
                .await
                .is_err()
        );

        revision = push(&writer, workspace_id, &snapshot(true), Some(revision)).await;
        let notification = message(&mut stream).await;
        assert_eq!(notification["method"], "notifications/resources/updated");
        assert_eq!(notification["params"]["uri"], EVENTS_URI);
        assert_eq!(
            notification["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
            "watch"
        );
        assert_eq!(store.events(workspace_id).await.unwrap().len(), 1);

        let mut closed = snapshot(true);
        closed.events[0].status = EventStatus::Closed;
        revision = push(&writer, workspace_id, &closed, Some(revision)).await;
        assert_eq!(
            message(&mut stream).await["method"],
            "notifications/resources/updated"
        );
        push(&writer, workspace_id, &snapshot(false), Some(revision)).await;
        assert_eq!(
            message(&mut stream).await["method"],
            "notifications/resources/updated"
        );
        drop(stream); // Cancels polling and releases the stream slot.

        let mut expiring = claims;
        expiring._exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 2;
        let response = listen(&store, &expiring, &request, "https://cloud.example/mcp")
            .await
            .unwrap();
        let mut stream = response.into_body().into_data_stream();
        assert_eq!(
            message(&mut stream).await["method"],
            "notifications/subscriptions/acknowledged"
        );
        let end = message(&mut stream).await;
        assert_eq!(end["id"], "watch");
        assert_eq!(end["result"]["resultType"], "complete");
        assert!(stream.next().await.is_none());
    }
}
