use super::{BridgeRequest, CliReply, CliRequest, Instance};
use crate::db::Db;
use crate::execution::{self, Context};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, McpError};
use crate::mcp::resources::EventResources;
use crate::mcp::{McpScope, McpScopeRequest};
use anyhow::{Context as _, Result, bail};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot, watch};

const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
const MAX_SESSIONS: usize = 256;

struct Session {
    scope: McpScope,
    context: Context,
    resources: EventResources,
    sender: broadcast::Sender<Value>,
    pending: Vec<Value>,
    bridge: bool,
    version: String,
    // Timestamp on the worker's available-time clock, not wall-clock time.
    touched: Duration,
}

impl Session {
    fn is_live(&self, available_time: Duration) -> bool {
        available_time.saturating_sub(self.touched)
            < Duration::from_secs(if self.bridge { 30 } else { 3600 })
            || self.sender.receiver_count() > 0
    }
}

enum Work {
    Stop,
    Cli(CliRequest, oneshot::Sender<CliReply>),
    Open(
        BridgeRequest,
        oneshot::Sender<std::result::Result<String, String>>,
    ),
    Rpc(Option<String>, JsonRpcRequest, oneshot::Sender<RpcReply>),
    Poll(String, oneshot::Sender<Option<Vec<Value>>>),
    Subscribe(String, oneshot::Sender<Option<broadcast::Receiver<Value>>>),
    Close(String, oneshot::Sender<bool>),
}

struct RpcReply {
    response: Option<JsonRpcResponse>,
    session: Option<String>,
    status: StatusCode,
}

#[derive(Clone)]
struct HttpState {
    instance: Instance,
    authority: String,
    worker: SyncSender<Work>,
}

impl HttpState {
    // Do not block an async runtime thread if the bounded worker queue is full.
    async fn send(&self, work: Work) -> std::result::Result<(), StatusCode> {
        let sender = self.worker.clone();
        let sent = tokio::task::spawn_blocking(move || sender.send(work).is_ok())
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        sent.then_some(()).ok_or(StatusCode::SERVICE_UNAVAILABLE)
    }
}

pub(super) fn serve(bind: &str, idle_timeout: u64, request: McpScopeRequest) -> Result<()> {
    let bind: std::net::SocketAddr = bind.parse().context("Invalid local listen address")?;
    if !bind.ip().is_loopback() {
        bail!("The local owner must bind to a loopback address");
    }
    let db_path = crate::db::owner::canonical_database_path()?;
    let _guard = super::acquire_server(&db_path)?;
    let mut context = Context::capture()?;
    context.vars.insert(
        "AI_WORKSPACE_DB".into(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Db::open(&db_path)?;
    let scope = execution::with(&context, || crate::mcp::resolve_scope(&db, request))
        .map_err(anyhow::Error::msg)?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let (shutdown, mut stopped) = watch::channel(false);
    let worker =
        std::thread::spawn(move || worker(receiver, db, scope, context, idle_timeout, shutdown));
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind(bind).await?;
            let address = listener.local_addr()?;
            let instance = Instance {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").into(),
                url: format!("http://{address}/"),
            token: super::local_token(&db_path)?,
            };
            super::publish(&db_path, &instance)?;
            eprintln!(
                "Local MCP owner PID {} listening at {}mcp; credentials: {}",
                instance.pid,
                instance.url,
                super::descriptor(&db_path).display()
            );
            let state = HttpState {
                instance,
                authority: address.to_string(),
                worker: sender.clone(),
            };
            let router = Router::new()
                .route("/status", get(status))
                .route("/mcp", post(rpc).get(events).delete(close))
                .route("/bridge", post(open_bridge))
                .route("/poll", post(poll))
                .route("/cli", post(cli))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
                .layer(middleware::from_fn_with_state(state.clone(), authorize))
                .with_state(state);
            let stop_sender = sender.clone();
            axum::serve(listener, router).with_graceful_shutdown(async move {
            tokio::select! {
                _ = stopped.changed() => {},
                _ = tokio::signal::ctrl_c() => {
                    let _ = tokio::task::spawn_blocking(move || stop_sender.send(Work::Stop).is_ok()).await;
                },
            }
        }).await?;
            Ok::<_, anyhow::Error>(())
        });
    // Drop all queue senders before joining. On server setup errors no worker
    // or exclusive lock may survive the failed foreground launch.
    drop(sender);
    let _ = worker.join();
    let _ = std::fs::remove_file(super::descriptor(&db_path));
    result
}

fn fixed_context(mut context: Context, base: &Context) -> Context {
    context.vars.insert(
        "AI_WORKSPACE_DB".into(),
        base.vars["AI_WORKSPACE_DB"].clone(),
    );
    context
}

fn new_session(
    scope: McpScope,
    context: Context,
    bridge: bool,
    version: String,
    available_time: Duration,
) -> Session {
    let (sender, _) = broadcast::channel(128);
    Session {
        scope,
        context,
        resources: EventResources::default(),
        sender,
        pending: Vec::new(),
        bridge,
        version,
        touched: available_time,
    }
}

fn within(scope: &McpScope, base: &McpScope) -> bool {
    match (scope, base) {
        (_, McpScope::Global) => true,
        (McpScope::Project { project_id, .. }, _) => base.allows_project(*project_id),
        (
            McpScope::Group { group_id, .. },
            McpScope::Group {
                group_id: base_id, ..
            },
        ) => group_id == base_id,
        _ => false,
    }
}

fn worker(
    receiver: mpsc::Receiver<Work>,
    db: Db,
    base_scope: McpScope,
    base: Context,
    idle_timeout: u64,
    shutdown: watch::Sender<bool>,
) {
    struct StopOnExit(watch::Sender<bool>);
    impl Drop for StopOnExit {
        fn drop(&mut self) {
            let _ = self.0.send(true);
        }
    }
    let _stop_on_exit = StopOnExit(shutdown.clone());
    let mut sessions = HashMap::<String, Session>::new();
    let mut available_time = Duration::ZERO;
    let mut last_activity = Duration::ZERO;
    let mut next_poll = Instant::now() + Duration::from_secs(1);
    loop {
        let waiting = Instant::now();
        let message = receiver.recv_timeout(next_poll.saturating_duration_since(Instant::now()));
        // Heartbeats queue behind CLI/tools and subscription maintenance. None
        // of that busy time may consume a client's lease (or owner idle time).
        available_time = available_time.saturating_add(waiting.elapsed());
        match message {
            Ok(work) => {
                last_activity = available_time;
                match work {
                    Work::Stop => break,
                    Work::Cli(request, reply) => {
                        let context = fixed_context(request.context, &base);
                        let (result, output) = execution::with(&context, || {
                            execution::capture_output(|| match request.app.command {
                                crate::cli::Command::Serve { .. }
                                | crate::cli::Command::LocalStatus
                                | crate::cli::Command::Update
                                | crate::cli::Command::Cloud {
                                    command: crate::cli::CloudCommand::Serve { .. },
                                } => bail!("This command cannot run inside the local owner"),
                                command => crate::cli::run(command, request.app.config),
                            })
                        });
                        let _ = reply.send(CliReply {
                            output,
                            error: result.err().map(|error| format!("{error:#}")),
                        });
                    }
                    Work::Open(request, reply) => {
                        let context = fixed_context(request.context, &base);
                        let result = execution::with(&context, || {
                            crate::mcp::resolve_scope(&db, request.scope)
                        })
                        .and_then(|scope| {
                            let current_base =
                                base_scope.refreshed(&db).map_err(|e| e.to_string())?;
                            if !within(&scope, &current_base) {
                                return Err("Requested scope exceeds local server scope".into());
                            }
                            if sessions.len() >= MAX_SESSIONS {
                                return Err("Too many local MCP connections".into());
                            }
                            let id = uuid::Uuid::new_v4().to_string();
                            sessions.insert(
                                id.clone(),
                                new_session(
                                    scope,
                                    context,
                                    true,
                                    "2024-11-05".into(),
                                    available_time,
                                ),
                            );
                            Ok(id)
                        });
                        let _ = reply.send(result);
                    }
                    Work::Rpc(id, request, reply) => {
                        let result = rpc_work(
                            &mut sessions,
                            &db,
                            &base_scope,
                            &base,
                            id,
                            request,
                            available_time,
                        );
                        let _ = reply.send(result);
                    }
                    Work::Poll(id, reply) => {
                        let result = sessions.get_mut(&id).filter(|s| s.bridge).map(|session| {
                            session.touched = available_time;
                            std::mem::take(&mut session.pending)
                        });
                        let _ = reply.send(result);
                    }
                    Work::Subscribe(id, reply) => {
                        let _ = reply.send(sessions.get_mut(&id).filter(|s| !s.bridge).map(|s| {
                            s.touched = available_time;
                            s.sender.subscribe()
                        }));
                    }
                    Work::Close(id, reply) => {
                        let _ = reply.send(sessions.remove(&id).is_some());
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_poll {
            sessions.retain(|_, session| session.is_live(available_time));
            for session in sessions.values_mut() {
                execution::with(&session.context, || {
                    if let (Ok(scope), Ok(current_base)) =
                        (session.scope.refreshed(&db), base_scope.refreshed(&db))
                        && within(&scope, &current_base)
                    {
                        for update in session.resources.poll(&db, &scope) {
                            if session.bridge {
                                if !session.pending.contains(&update) {
                                    session.pending.push(update);
                                }
                            } else {
                                let _ = session.sender.send(update);
                            }
                        }
                    }
                });
            }
            next_poll = Instant::now() + Duration::from_secs(1);
            if idle_timeout > 0
                && sessions.is_empty()
                && available_time.saturating_sub(last_activity) >= Duration::from_secs(idle_timeout)
            {
                let _ = shutdown.send(true);
                break;
            }
        }
    }
}

fn rpc_work(
    sessions: &mut HashMap<String, Session>,
    db: &Db,
    base_scope: &McpScope,
    base: &Context,
    mut id: Option<String>,
    request: JsonRpcRequest,
    available_time: Duration,
) -> RpcReply {
    let error = |status, message| RpcReply {
        response: Some(JsonRpcResponse::error(
            request.id.clone(),
            McpError::invalid_request(message),
        )),
        session: None,
        status,
    };
    if id.is_none() {
        if request.method != "initialize" {
            return error(
                StatusCode::BAD_REQUEST,
                "Initialize a local MCP session first",
            );
        }
        if sessions.len() >= MAX_SESSIONS {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many local MCP connections",
            );
        }
        let requested = request.params["protocolVersion"]
            .as_str()
            .unwrap_or(VERSIONS[0]);
        let version = if VERSIONS.contains(&requested) {
            requested
        } else {
            VERSIONS[0]
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            session_id.clone(),
            new_session(
                base_scope.clone(),
                base.clone(),
                false,
                version.into(),
                available_time,
            ),
        );
        id = Some(session_id);
    }
    let session_id = id.unwrap();
    let Some(session) = sessions.get_mut(&session_id) else {
        return error(
            StatusCode::NOT_FOUND,
            "Unknown local MCP session; initialize again",
        );
    };
    if !session.bridge
        && (request.jsonrpc.as_deref() != Some("2.0")
            || request.method.starts_with("notifications/") != request.id.is_null()
            || (!request.id.is_null()
                && !request.id.is_string()
                && !request.id.is_i64()
                && !request.id.is_u64()))
    {
        return error(StatusCode::BAD_REQUEST, "Invalid JSON-RPC envelope");
    }
    session.touched = available_time;
    let initialize = request.method == "initialize";
    let mut response = execution::with(&session.context, || {
        match session.scope.refreshed(db).and_then(|scope| {
            let current_base = base_scope.refreshed(db)?;
            if !within(&scope, &current_base) {
                bail!("Connection is outside the current local server scope");
            }
            Ok(scope)
        }) {
            Ok(scope) if request.method.starts_with("resources/") => {
                Some(session.resources.handle(&request, db, &scope))
            }
            Ok(scope) => crate::mcp::handle_request_with_scope(request, &scope),
            Err(error) => Some(JsonRpcResponse::error(
                request.id,
                McpError::internal_error(&error.to_string()),
            )),
        }
    });
    if initialize
        && let Some(result) = response
            .as_mut()
            .and_then(|response| response.result.as_mut())
    {
        result["protocolVersion"] = json!(session.version);
    }
    RpcReply {
        status: if response.is_some() {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        response,
        session: Some(session_id),
    }
}

async fn authorize(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let host_ok = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            host == state.authority || host == state.authority.replacen("127.0.0.1", "localhost", 1)
        });
    let origin_ok = headers.get("origin").is_none_or(|origin| {
        origin
            .to_str()
            .is_ok_and(|origin| origin == format!("http://{}", state.authority))
    });
    if !host_ok || !origin_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.strip_prefix("Bearer ") == Some(state.instance.token.as_str()));
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn status(State(state): State<HttpState>) -> Json<Value> {
    Json(json!({"pid": state.instance.pid, "version": state.instance.version}))
}

fn session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Mcp-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn rpc(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match serde_json::from_slice::<JsonRpcRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            return Json(JsonRpcResponse::error(
                Value::Null,
                McpError::parse_error(&error.to_string()),
            ))
            .into_response();
        }
    };
    if let Some(version) = headers.get("MCP-Protocol-Version")
        && !VERSIONS.iter().any(|supported| version == *supported)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(JsonRpcResponse::error(
                request.id,
                McpError::unsupported_protocol_version(version.to_str().unwrap_or(""), VERSIONS),
            )),
        )
            .into_response();
    }
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state
        .send(Work::Rpc(session_id(&headers), request, sender))
        .await
    {
        return response.into_response();
    }
    let Ok(reply) = receiver.await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut response = match reply.response {
        Some(rpc) => (reply.status, Json(rpc)).into_response(),
        None => reply.status.into_response(),
    };
    if let Some(session) = reply.session {
        response
            .headers_mut()
            .insert("Mcp-Session-Id", HeaderValue::from_str(&session).unwrap());
    }
    response
}

async fn cli(State(state): State<HttpState>, Json(request): Json<CliRequest>) -> Response {
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state.send(Work::Cli(request, sender)).await {
        return response.into_response();
    }
    match receiver.await {
        Ok(reply) => Json(reply).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn open_bridge(
    State(state): State<HttpState>,
    Json(request): Json<BridgeRequest>,
) -> Response {
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state.send(Work::Open(request, sender)).await {
        return response.into_response();
    }
    match receiver.await {
        Ok(Ok(id)) => Json(id).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn poll(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let Some(id) = session_id(&headers) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state.send(Work::Poll(id, sender)).await {
        return response.into_response();
    }
    match receiver.await {
        Ok(Some(updates)) => Json(updates).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn close(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let Some(id) = session_id(&headers) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state.send(Work::Close(id, sender)).await {
        return response.into_response();
    }
    match receiver.await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn events(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let Some(id) = session_id(&headers) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (sender, receiver) = oneshot::channel();
    if let Err(response) = state.send(Work::Subscribe(id, sender)).await {
        return response.into_response();
    }
    let Ok(Some(receiver)) = receiver.await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Ok(value) => Some((
                Ok::<_, Infallible>(Event::default().event("message").data(value.to_string())),
                receiver,
            )),
            Err(_) => None,
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_sessions_still_expire_when_the_worker_is_available() {
        let context = Context {
            cwd: std::env::current_dir().unwrap(),
            vars: HashMap::new(),
        };
        let mut bridge = new_session(
            McpScope::Global,
            context.clone(),
            true,
            "2024-11-05".into(),
            Duration::from_secs(10),
        );
        assert!(bridge.is_live(Duration::from_secs(39)));
        assert!(!bridge.is_live(Duration::from_secs(40)));
        bridge.touched = Duration::from_secs(40);
        assert!(bridge.is_live(Duration::from_secs(69)));
        assert!(!bridge.is_live(Duration::from_secs(70)));
        let http = new_session(
            McpScope::Global,
            context,
            false,
            VERSIONS[0].into(),
            Duration::ZERO,
        );
        let listener = http.sender.subscribe();
        assert!(http.is_live(Duration::from_secs(3600)));
        drop(listener);
        assert!(!http.is_live(Duration::from_secs(3600)));
    }
}
