use super::auth::{AccessClaims, AuthConfig, JwtValidator};
use super::models::{
    CloudPushError, CloudPushRequest, CloudPushResponse, MAX_CLOUD_SNAPSHOT_BYTES,
};
use super::snapshot::sha256_hex;
use super::store::{CloudStore, ReplaceSnapshotOutcome};
use anyhow::{Context as _, Result, bail};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use log::{error, info, warn};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tower_http::timeout::TimeoutLayer;

const MAX_CLOUD_REQUEST_BYTES: usize = MAX_CLOUD_SNAPSHOT_BYTES + 64 * 1024;

#[derive(Clone)]
pub struct CloudHttpState {
    pub store: CloudStore,
    pub auth: JwtValidator,
    pub public_mcp_uri: String,
    pub issuer: String,
}

pub struct CloudServerConfig {
    pub bind: SocketAddr,
    pub public_mcp_uri: reqwest::Url,
    pub database_url: String,
    pub auth: AuthConfig,
}

impl CloudServerConfig {
    pub fn new(
        bind: &str,
        public_mcp_uri: &str,
        database_url: &str,
        issuer: &str,
        audience: &str,
        jwks_uri: &str,
    ) -> Result<Self> {
        let bind = bind.parse().context("Invalid cloud bind address")?;
        let public_mcp_uri =
            reqwest::Url::parse(public_mcp_uri).context("Invalid public MCP URI")?;
        if public_mcp_uri.scheme() != "https" {
            bail!("Public MCP URI must use HTTPS");
        }
        if database_url.trim().is_empty() {
            bail!("Cloud PostgreSQL URL is required");
        }
        Ok(Self {
            bind,
            public_mcp_uri,
            database_url: database_url.to_owned(),
            auth: AuthConfig::new(issuer, audience, jwks_uri)?,
        })
    }
}

pub fn router(state: CloudHttpState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/api/v1/workspaces/{workspace_slug}/projects/{project_slug}/snapshot",
            put(push_snapshot),
        )
        .route(
            "/mcp",
            post(super::mcp::handle)
                .layer(DefaultBodyLimit::max(super::mcp::MAX_MCP_REQUEST_BYTES)),
        )
        .layer(DefaultBodyLimit::max(MAX_CLOUD_REQUEST_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .with_state(state)
}

pub async fn serve(config: CloudServerConfig) -> Result<()> {
    let store = CloudStore::connect(&config.database_url).await?;
    let state = CloudHttpState {
        store,
        auth: JwtValidator::new(config.auth.clone())?,
        public_mcp_uri: config.public_mcp_uri.to_string(),
        issuer: config.auth.issuer,
    };
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .context("Failed to bind cloud HTTP server")?;
    info!(
        "cloud HTTP server listening bind={} public_uri='{}'",
        config.bind, state.public_mcp_uri
    );
    axum::serve(listener, router(state))
        .await
        .context("Cloud HTTP server failed")
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<CloudHttpState>) -> Response {
    match state.store.ready().await {
        Ok(()) => Json(json!({ "status": "ready" })).into_response(),
        Err(error) => {
            warn!("cloud readiness failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable" })),
            )
                .into_response()
        }
    }
}

async fn protected_resource_metadata(State(state): State<CloudHttpState>) -> Json<Value> {
    Json(json!({
        "resource": state.public_mcp_uri,
        "authorization_servers": [state.issuer],
        "scopes_supported": ["ai-workspace:read", "ai-workspace:push", "ai-workspace:push-force"],
        "bearer_methods_supported": ["header"]
    }))
}

async fn push_snapshot(
    State(state): State<CloudHttpState>,
    Path((workspace_slug, project_slug)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<CloudPushRequest>,
) -> Response {
    let started = Instant::now();
    if validate_origin(&headers, &state.public_mcp_uri).is_err() {
        return cloud_error(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "Invalid Origin",
            None,
        );
    }
    let claims = match state.auth.authenticate(&headers).await {
        Ok(claims) => claims,
        Err(error) => {
            warn!("[FIX:cloud-auth] cloud push authentication rejected: {error}");
            return unauthorized_response(
                &state.public_mcp_uri,
                if request.force {
                    "ai-workspace:push ai-workspace:push-force"
                } else {
                    "ai-workspace:push"
                },
                "Token validation failed",
            );
        }
    };
    let response =
        push_authenticated(&state, &claims, &workspace_slug, &project_slug, request).await;
    audit_request(
        &claims,
        started,
        response.status(),
        json!({
            "operation": "snapshot_push", "scope": "project",
            "project_sha256": sha256_hex(project_slug.as_bytes())
        }),
    );
    response
}

async fn push_authenticated(
    state: &CloudHttpState,
    claims: &AccessClaims,
    workspace_slug: &str,
    project_slug: &str,
    request: CloudPushRequest,
) -> Response {
    for required in [
        Some("ai-workspace:push"),
        request.force.then_some("ai-workspace:push-force"),
    ]
    .into_iter()
    .flatten()
    {
        if claims.require_scope(required).is_err() {
            warn!(
                "[FIX:cloud-auth] cloud push rejected insufficient scope workspace_id={} required={}",
                claims.workspace_id, required
            );
            return insufficient_scope_response(&state.public_mcp_uri, required);
        }
    }
    let (counts, snapshot_hash) = match validate_push_request(
        claims,
        workspace_slug,
        project_slug,
        &request,
    ) {
        Ok(validated) => validated,
        Err(error) => {
            warn!(
                "[FIX:cloud-validation] cloud push validation rejected workspace_id={} project_slug='{}': {}",
                claims.workspace_id, project_slug, error
            );
            return cloud_error(
                StatusCode::BAD_REQUEST,
                "invalid_snapshot",
                &error.to_string(),
                None,
            );
        }
    };
    match state
        .store
        .replace_project_snapshot(
            claims.workspace_id,
            workspace_slug,
            &request.snapshot,
            &snapshot_hash,
            request.base_revision,
            request.force,
            &claims.sub,
        )
        .await
    {
        Ok(ReplaceSnapshotOutcome::Accepted {
            revision,
            snapshot_hash,
            no_op,
        }) => Json(CloudPushResponse {
            revision,
            snapshot_hash,
            counts,
            no_op,
        })
        .into_response(),
        Ok(ReplaceSnapshotOutcome::Conflict(current)) => cloud_error(
            StatusCode::CONFLICT,
            "revision_conflict",
            "Cloud snapshot revision conflict",
            Some(current),
        ),
        Err(error) => {
            error!(
                "cloud push failed workspace_id={} project_slug='{}': {}",
                claims.workspace_id, project_slug, error
            );
            cloud_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Cloud snapshot push failed",
                None,
            )
        }
    }
}

pub fn audit_request(claims: &AccessClaims, started: Instant, status: StatusCode, details: Value) {
    info!(
        "cloud audit {}",
        json!({
            "unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "subject_sha256": sha256_hex(claims.sub.as_bytes()),
            "workspace_id": claims.workspace_id,
            "status": status.as_u16(),
            "duration_ms": started.elapsed().as_millis(),
            "details": details
        })
    );
}

fn validate_push_request(
    claims: &AccessClaims,
    workspace_slug: &str,
    project_slug: &str,
    request: &CloudPushRequest,
) -> Result<(super::models::CloudSnapshotCounts, String)> {
    if claims.workspace_slug != workspace_slug {
        bail!("Token workspace does not match request path");
    }
    if request.snapshot.project.slug != project_slug {
        bail!("Snapshot project does not match request path");
    }
    request.snapshot.validate()?;
    let snapshot_hash = sha256_hex(&serde_json::to_vec(&request.snapshot)?);
    if request.snapshot_hash != snapshot_hash {
        bail!("Snapshot hash does not match payload");
    }
    Ok((request.snapshot.counts(), snapshot_hash))
}

fn cloud_error(
    status: StatusCode,
    code: &str,
    message: &str,
    current: Option<super::store::SnapshotStatus>,
) -> Response {
    (
        status,
        Json(CloudPushError {
            code: code.to_owned(),
            message: message.to_owned(),
            current_revision: current.as_ref().map(|value| value.revision),
            current_snapshot_hash: current.map(|value| value.snapshot_hash),
        }),
    )
        .into_response()
}

pub fn validate_origin(headers: &HeaderMap, public_uri: &str) -> Result<()> {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| {
        warn!("[FIX:cloud-origin] cloud request rejected malformed Origin header");
        anyhow::anyhow!("Invalid Origin")
    })?;
    let public = reqwest::Url::parse(public_uri)?;
    let expected = format!(
        "{}://{}{}",
        public.scheme(),
        public.host_str().unwrap_or_default(),
        public
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    if origin != expected {
        warn!("[FIX:cloud-origin] cloud request rejected mismatched Origin");
        bail!("Invalid Origin");
    }
    Ok(())
}

pub fn unauthorized_response(
    public_mcp_uri: &str,
    required_scope: &str,
    error_description: &str,
) -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response();
    let challenge = format!(
        "Bearer resource_metadata=\"{}\", error=\"invalid_token\", error_description=\"{}\", scope=\"{}\"",
        protected_resource_metadata_uri(public_mcp_uri),
        error_description.replace(['"', '\r', '\n'], ""),
        required_scope.replace(['"', '\r', '\n'], "")
    );
    match HeaderValue::from_str(&challenge) {
        Ok(value) => {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, value);
        }
        Err(error) => error!("Failed to construct WWW-Authenticate challenge: {error}"),
    }
    response
}

pub fn insufficient_scope_response(public_mcp_uri: &str, required_scope: &str) -> Response {
    let mut response = (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "insufficient_scope" })),
    )
        .into_response();
    let challenge = format!(
        "Bearer resource_metadata=\"{}\", error=\"insufficient_scope\", scope=\"{}\"",
        protected_resource_metadata_uri(public_mcp_uri),
        required_scope.replace(['"', '\r', '\n'], "")
    );
    match HeaderValue::from_str(&challenge) {
        Ok(value) => {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, value);
        }
        Err(error) => error!("Failed to construct WWW-Authenticate challenge: {error}"),
    }
    response
}

fn protected_resource_metadata_uri(public_mcp_uri: &str) -> String {
    let Ok(mut uri) = reqwest::Url::parse(public_mcp_uri) else {
        return public_mcp_uri.to_owned();
    };
    uri.set_path("/.well-known/oauth-protected-resource");
    uri.set_query(None);
    uri.set_fragment(None);
    uri.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::models::{
        CLOUD_SNAPSHOT_SCHEMA_VERSION, CloudDocument, CloudProject, CloudProjectSnapshot,
        CloudShare,
    };
    use crate::models::SharedItemKind;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    const TEST_RSA_KEY: &[u8] = include_bytes!("../../tests/fixtures/cloud_test_rsa.pem");
    const TEST_RSA_MODULUS: &str = "yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ";

    #[test]
    fn server_config_rejects_non_https_public_uri() {
        assert!(
            CloudServerConfig::new(
                "127.0.0.1:8080",
                "http://example.test/mcp",
                "postgres://runtime@db/cloud",
                "https://issuer.test",
                "cloud",
                "https://issuer.test/jwks"
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn health_reports_ok_without_external_services() {
        assert_eq!(health().await.0, json!({ "status": "ok" }));
    }

    #[test]
    fn bearer_challenge_points_to_origin_protected_resource_metadata() {
        let response = unauthorized_response(
            "https://cloud.example/mcp",
            "ai-workspace:read",
            "bad token",
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .unwrap(),
            "Bearer resource_metadata=\"https://cloud.example/.well-known/oauth-protected-resource\", error=\"invalid_token\", error_description=\"bad token\", scope=\"ai-workspace:read\""
        );
    }

    #[test]
    fn insufficient_scope_uses_403_and_advertises_required_scope() {
        let response = insufficient_scope_response(
            "https://cloud.example/mcp?ignored=true",
            "ai-workspace:read",
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .unwrap(),
            "Bearer resource_metadata=\"https://cloud.example/.well-known/oauth-protected-resource\", error=\"insufficient_scope\", scope=\"ai-workspace:read\""
        );
    }

    #[test]
    fn malformed_origin_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );

        assert!(validate_origin(&headers, "https://cloud.example/mcp").is_err());
    }

    #[test]
    fn push_validation_binds_token_route_project_and_hash() {
        let snapshot = CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: CloudProject {
                cloud_key: "project:demo".into(),
                name: "Demo".into(),
                slug: "demo".into(),
            },
            groups: vec![],
            shares: vec![],
            documents: vec![],
            notes: vec![],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        };
        let request = CloudPushRequest {
            base_revision: None,
            force: false,
            snapshot_hash: sha256_hex(&serde_json::to_vec(&snapshot).unwrap()),
            snapshot,
        };
        let claims = AccessClaims {
            sub: "user".into(),
            workspace_id: Uuid::nil(),
            workspace_slug: "team".into(),
            scope: "ai-workspace:push".into(),
            _exp: u64::MAX,
            _nbf: None,
        };
        assert!(validate_push_request(&claims, "team", "demo", &request).is_ok());
        assert!(validate_push_request(&claims, "other", "demo", &request).is_err());

        let mut tampered = request;
        tampered.snapshot.project.name = "Changed".into();
        assert!(validate_push_request(&claims, "team", "demo", &tampered).is_err());
    }

    #[derive(Serialize)]
    struct TestClaims<'a> {
        sub: &'a str,
        workspace_id: Uuid,
        workspace_slug: &'a str,
        scope: &'a str,
        iss: &'a str,
        aud: &'a str,
        exp: u64,
        nbf: u64,
    }

    fn test_rsa_der() -> Vec<u8> {
        let pem = std::str::from_utf8(TEST_RSA_KEY).unwrap();
        let mut output = Vec::new();
        let mut buffer = 0_u32;
        let mut bits = 0_u32;
        for byte in pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .flat_map(str::bytes)
            .filter(|byte| *byte != b'=')
        {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("invalid test PEM"),
            };
            buffer = (buffer << 6) | u32::from(value);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        output
    }

    fn test_token(workspace_id: Uuid, workspace_slug: &str, scope: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("cloud-test".into());
        let key = test_rsa_der();
        encode(
            &header,
            &TestClaims {
                sub: "cloud-http-test",
                workspace_id,
                workspace_slug,
                scope,
                iss: "https://issuer.test",
                aud: "cloud-test",
                exp: now + 300,
                nbf: now.saturating_sub(1),
            },
            &EncodingKey::from_rsa_der(&key),
        )
        .unwrap()
    }

    async fn hosted_request(
        client: &reqwest::Client,
        base_url: &str,
        token: &str,
        method: &str,
        tool: Option<&str>,
        version: &str,
        mut params: Value,
    ) -> reqwest::Response {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        let mut request = client
            .post(format!("{base_url}/mcp"))
            .bearer_auth(token)
            .header("MCP-Protocol-Version", version)
            .header("Mcp-Method", method);
        if let Some(tool) = tool {
            request = request.header("Mcp-Name", tool);
        }
        request
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": method, "params": params
            }))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn authenticated_http_push_and_hosted_mcp_search_round_trip() {
        let Some(database_url) = std::env::var("AI_WORKSPACE_CLOUD_TEST_DATABASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
        else {
            return;
        };

        let jwks_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jwks_address = jwks_listener.local_addr().unwrap();
        let jwks = Router::new().route(
            "/jwks",
            get(|| async {
                Json(json!({"keys": [{
                    "kty": "RSA", "kid": "cloud-test", "alg": "RS256", "use": "sig",
                    "n": TEST_RSA_MODULUS, "e": "AQAB"
                }]}))
            }),
        );
        let jwks_server = tokio::spawn(async move {
            axum::serve(jwks_listener, jwks).await.unwrap();
        });

        let workspace_id = Uuid::new_v4();
        let workspace_slug = format!("e2e-{}", &workspace_id.simple().to_string()[..12]);
        let state = CloudHttpState {
            store: CloudStore::connect(&database_url).await.unwrap(),
            auth: JwtValidator::new(
                AuthConfig::new(
                    "https://issuer.test",
                    "cloud-test",
                    &format!("http://{jwks_address}/jwks"),
                )
                .unwrap(),
            )
            .unwrap(),
            public_mcp_uri: "https://cloud.example/mcp".into(),
            issuer: "https://issuer.test".into(),
        };
        let cloud_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cloud_address = cloud_listener.local_addr().unwrap();
        let cloud_server = tokio::spawn(async move {
            axum::serve(cloud_listener, router(state)).await.unwrap();
        });
        let base_url = format!("http://{cloud_address}");
        let client = reqwest::Client::new();

        let health_response = client
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(health_response.status(), StatusCode::OK);
        assert_eq!(
            health_response.json::<Value>().await.unwrap(),
            json!({ "status": "ok" })
        );
        let ready_response = client
            .get(format!("{base_url}/readyz"))
            .send()
            .await
            .unwrap();
        assert_eq!(ready_response.status(), StatusCode::OK);

        let metadata: Value = client
            .get(format!("{base_url}/.well-known/oauth-protected-resource"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            metadata["scopes_supported"],
            json!([
                "ai-workspace:read",
                "ai-workspace:push",
                "ai-workspace:push-force"
            ])
        );

        let list_request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": super::super::mcp::PROTOCOL_VERSION,
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        });
        let unauthorized = client
            .post(format!("{base_url}/mcp"))
            .header("MCP-Protocol-Version", super::super::mcp::PROTOCOL_VERSION)
            .header("Mcp-Method", "tools/list")
            .json(&list_request)
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(
            unauthorized
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("https://cloud.example/.well-known/oauth-protected-resource")
        );

        let snapshot = CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: CloudProject {
                cloud_key: "project:demo".into(),
                name: "Demo".into(),
                slug: "demo".into(),
            },
            groups: vec![],
            shares: vec![CloudShare {
                cloud_key: "share:demo:README.md".into(),
                project_slug: "demo".into(),
                relative_path: "README.md".into(),
                kind: SharedItemKind::File,
                label: None,
            }],
            documents: vec![CloudDocument {
                cloud_key: "file:demo:README.md".into(),
                share_key: "share:demo:README.md".into(),
                project_slug: "demo".into(),
                relative_path: "README.md".into(),
                label: None,
                content: "cloudcontractmarker".into(),
            }],
            notes: vec![],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        };
        let push = CloudPushRequest {
            base_revision: None,
            force: false,
            snapshot_hash: sha256_hex(&serde_json::to_vec(&snapshot).unwrap()),
            snapshot,
        };
        let token = test_token(
            workspace_id,
            &workspace_slug,
            "ai-workspace:push ai-workspace:read",
        );

        // Real Codex uses the standard initialize handshake, without modern headers/_meta.
        let initialize = json!({
            "jsonrpc": "2.0", "id": 42, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "codex", "version": "0.153.2"}}
        });
        let initialized = json!({"jsonrpc":"2.0", "method":"notifications/initialized"});
        let push_only_token = test_token(workspace_id, &workspace_slug, "ai-workspace:push");
        for body in [&initialize, &initialized] {
            let unauthorized = client
                .post(format!("{base_url}/mcp"))
                .json(body)
                .send()
                .await
                .unwrap();
            assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
            let forbidden = client
                .post(format!("{base_url}/mcp"))
                .bearer_auth(&push_only_token)
                .json(body)
                .send()
                .await
                .unwrap();
            assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
            let bad_origin = client
                .post(format!("{base_url}/mcp"))
                .bearer_auth(&token)
                .header("Origin", "https://attacker.example")
                .json(body)
                .send()
                .await
                .unwrap();
            assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);
        }
        for version in ["2025-11-25", "2025-06-18"] {
            let mut initialize = initialize.clone();
            initialize["params"]["protocolVersion"] = json!(version);
            let response = client
                .post(format!("{base_url}/mcp"))
                .bearer_auth(&token)
                .header("Accept", "application/json, text/event-stream")
                .json(&initialize)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().get("MCP-Session-Id").is_none());
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["id"], 42);
            assert_eq!(body["result"]["protocolVersion"], version);
            assert_eq!(body["result"]["capabilities"], json!({"tools":{}}));
            assert_eq!(body["result"]["serverInfo"]["name"], "ai-workspace-cloud");
            assert!(body["result"].get("_meta").is_none());
            let response = client
                .post(format!("{base_url}/mcp"))
                .bearer_auth(&token)
                .header("MCP-Protocol-Version", version)
                .json(&initialized)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert!(response.bytes().await.unwrap().is_empty());
            for method in ["tools/list", "ping"] {
                let response = client
                    .post(format!("{base_url}/mcp"))
                    .bearer_auth(&token)
                    .header("MCP-Protocol-Version", version)
                    .json(&json!({"jsonrpc":"2.0", "id":43, "method":method}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body: Value = response.json().await.unwrap();
                assert_eq!(body["id"], 43);
                if method == "tools/list" {
                    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 7);
                } else {
                    assert_eq!(body["result"], json!({}));
                }
            }
        }
        assert_eq!(
            client
                .get(format!("{base_url}/mcp"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );

        let invalid_origin = client
            .post(format!("{base_url}/mcp"))
            .bearer_auth(&token)
            .header("MCP-Protocol-Version", super::super::mcp::PROTOCOL_VERSION)
            .header("Mcp-Method", "tools/list")
            .header(axum::http::header::ORIGIN, "https://attacker.example")
            .json(&list_request)
            .send()
            .await
            .unwrap();
        assert_eq!(invalid_origin.status(), StatusCode::FORBIDDEN);

        let invalid_content_type = client
            .post(format!("{base_url}/mcp"))
            .bearer_auth(&token)
            .header("MCP-Protocol-Version", super::super::mcp::PROTOCOL_VERSION)
            .header("Mcp-Method", "tools/list")
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(list_request.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            invalid_content_type.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );

        for (length, oversized) in [
            (super::super::mcp::MAX_MCP_REQUEST_BYTES, false),
            (super::super::mcp::MAX_MCP_REQUEST_BYTES + 1, true),
        ] {
            let response = client
                .post(format!("{base_url}/mcp"))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(vec![b' '; length])
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status() == StatusCode::PAYLOAD_TOO_LARGE,
                oversized
            );
        }

        let snapshot_url =
            format!("{base_url}/api/v1/workspaces/{workspace_slug}/projects/demo/snapshot");

        let exact_request_limit = client
            .put(&snapshot_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(vec![b' '; MAX_CLOUD_REQUEST_BYTES])
            .send()
            .await
            .unwrap();
        assert_ne!(exact_request_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let over_request_limit = client
            .put(&snapshot_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(vec![b' '; MAX_CLOUD_REQUEST_BYTES + 1])
            .send()
            .await
            .unwrap();
        assert_eq!(over_request_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let push_response = client
            .put(&snapshot_url)
            .bearer_auth(&token)
            .json(&push)
            .send()
            .await
            .unwrap();
        assert_eq!(push_response.status(), StatusCode::OK);
        let accepted: CloudPushResponse = push_response.json().await.unwrap();
        assert_eq!(accepted.revision, 1);
        assert!(!accepted.no_op);

        let mut too_many_documents = push.clone();
        too_many_documents.snapshot.documents = vec![
            too_many_documents.snapshot.documents[0]
                .clone();
            super::super::models::MAX_CLOUD_DOCUMENTS + 1
        ];
        too_many_documents.snapshot_hash =
            sha256_hex(&serde_json::to_vec(&too_many_documents.snapshot).unwrap());
        let snapshot_limit_response = client
            .put(&snapshot_url)
            .bearer_auth(&token)
            .json(&too_many_documents)
            .send()
            .await
            .unwrap();
        assert_eq!(snapshot_limit_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            snapshot_limit_response
                .json::<CloudPushError>()
                .await
                .unwrap()
                .code,
            "invalid_snapshot"
        );

        let retry_response = client
            .put(&snapshot_url)
            .bearer_auth(&token)
            .json(&push)
            .send()
            .await
            .unwrap();
        assert_eq!(retry_response.status(), StatusCode::OK);
        let retry: CloudPushResponse = retry_response.json().await.unwrap();
        assert_eq!(retry.revision, 1);
        assert!(retry.no_op);

        let mut changed = push.clone();
        changed.snapshot.documents[0].content = "cloudcontractmarker changed".into();
        changed.snapshot_hash = sha256_hex(&serde_json::to_vec(&changed.snapshot).unwrap());
        let conflict_response = client
            .put(&snapshot_url)
            .bearer_auth(&token)
            .json(&changed)
            .send()
            .await
            .unwrap();
        assert_eq!(conflict_response.status(), StatusCode::CONFLICT);
        let conflict: CloudPushError = conflict_response.json().await.unwrap();
        assert_eq!(conflict.code, "revision_conflict");
        assert_eq!(conflict.current_revision, Some(1));
        assert_eq!(
            conflict.current_snapshot_hash,
            Some(push.snapshot_hash.clone())
        );

        changed.force = true;
        // Force permission is required even for an otherwise harmless no-op retry.
        for (scope, missing_scope) in [
            (
                "ai-workspace:push ai-workspace:read",
                "ai-workspace:push-force",
            ),
            (
                "ai-workspace:push ai-workspace:push-force-extra",
                "ai-workspace:push-force",
            ),
            ("ai-workspace:push-force", "ai-workspace:push"),
        ] {
            let restricted_token = test_token(workspace_id, &workspace_slug, scope);
            for mut request in [push.clone(), changed.clone()] {
                request.force = true;
                let denied = client
                    .put(&snapshot_url)
                    .bearer_auth(&restricted_token)
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(denied.status(), StatusCode::FORBIDDEN);
                assert!(
                    denied.headers()[axum::http::header::WWW_AUTHENTICATE]
                        .to_str()
                        .unwrap()
                        .contains(&format!("scope=\"{missing_scope}\""))
                );
                assert_eq!(
                    denied.json::<Value>().await.unwrap()["error"],
                    "insufficient_scope"
                );
            }
        }
        let preserved = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("workspace_read"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({"name":"workspace_read", "arguments":{"document_key":"file:demo:README.md"}}),
        )
        .await;
        assert_eq!(preserved.status(), StatusCode::OK);
        assert_eq!(
            preserved.json::<Value>().await.unwrap()["result"]["structuredContent"]["content"],
            "cloudcontractmarker"
        );
        let force_token = test_token(
            workspace_id,
            &workspace_slug,
            "ai-workspace:push ai-workspace:push-force",
        );
        let forced_response = client
            .put(&snapshot_url)
            .bearer_auth(&force_token)
            .json(&changed)
            .send()
            .await
            .unwrap();
        assert_eq!(forced_response.status(), StatusCode::OK);
        let forced: CloudPushResponse = forced_response.json().await.unwrap();
        assert_eq!(forced.revision, 2);
        assert!(!forced.no_op);

        // A -> B -> exact original A must preserve B, including its server revision.
        let replay = client
            .put(&snapshot_url)
            .bearer_auth(&token)
            .json(&push)
            .send()
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        let replay: CloudPushError = replay.json().await.unwrap();
        assert_eq!(replay.current_revision, Some(2));
        assert_eq!(
            replay.current_snapshot_hash,
            Some(changed.snapshot_hash.clone())
        );
        let preserved = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("workspace_read"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({"name":"workspace_read", "arguments":{"document_key":"file:demo:README.md"}}),
        )
        .await;
        assert_eq!(preserved.status(), StatusCode::OK);
        assert_eq!(
            preserved.json::<Value>().await.unwrap()["result"]["structuredContent"]["content"],
            "cloudcontractmarker changed"
        );

        let excessive_query = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("workspace_search_fulltext"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({"name":"workspace_search_fulltext", "arguments":{"query":"x".repeat(1025)}}),
        )
        .await;
        assert_eq!(excessive_query.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            excessive_query.json::<Value>().await.unwrap()["error"]["code"],
            -32602
        );

        let discovery = hosted_request(
            &client,
            &base_url,
            &token,
            "server/discover",
            None,
            super::super::mcp::PROTOCOL_VERSION,
            json!({}),
        )
        .await;
        assert_eq!(discovery.status(), StatusCode::OK);
        assert_eq!(
            discovery.json::<Value>().await.unwrap()["result"]["cacheScope"],
            "private"
        );

        let catalog = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/list",
            None,
            super::super::mcp::PROTOCOL_VERSION,
            json!({}),
        )
        .await;
        assert_eq!(catalog.status(), StatusCode::OK);
        assert_eq!(
            catalog.json::<Value>().await.unwrap()["result"]["cacheScope"],
            "private"
        );

        let push_only_token = test_token(workspace_id, &workspace_slug, "ai-workspace:push");
        let under_scoped = hosted_request(
            &client,
            &base_url,
            &push_only_token,
            "tools/list",
            None,
            super::super::mcp::PROTOCOL_VERSION,
            json!({}),
        )
        .await;
        assert_eq!(under_scoped.status(), StatusCode::FORBIDDEN);

        let mismatch = client
            .post(format!("{base_url}/mcp"))
            .bearer_auth(&token)
            .header(
                "MCP-Protocol-Version",
                super::super::mcp::PROTOCOL_VERSION,
            )
            .header("Mcp-Method", "server/discover")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": super::super::mcp::PROTOCOL_VERSION,
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            mismatch.json::<Value>().await.unwrap()["error"]["code"],
            -32020
        );

        let unsupported = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/list",
            None,
            "2099-01-01",
            json!({}),
        )
        .await;
        assert_eq!(unsupported.status(), StatusCode::BAD_REQUEST);
        let unsupported: Value = unsupported.json().await.unwrap();
        assert_eq!(unsupported["error"]["code"], -32022);
        assert_eq!(
            unsupported["error"]["data"]["supported"],
            json!([
                super::super::mcp::PROTOCOL_VERSION,
                "2025-11-25",
                "2025-06-18"
            ])
        );

        let unknown_method = hosted_request(
            &client,
            &base_url,
            &token,
            "unknown/method",
            None,
            super::super::mcp::PROTOCOL_VERSION,
            json!({}),
        )
        .await;
        assert_eq!(unknown_method.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            unknown_method.json::<Value>().await.unwrap()["error"]["code"],
            -32601
        );

        let unknown_tool = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("project_file_write"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({"name": "project_file_write", "arguments": {}}),
        )
        .await;
        assert_eq!(unknown_tool.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            unknown_tool.json::<Value>().await.unwrap()["error"]["code"],
            -32602
        );

        let response = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("workspace_search_fulltext"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({
                "name": "workspace_search_fulltext",
                "arguments": {"query": "cloudcontractmarker"}
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["result"]["resultType"], "complete");
        assert!(
            body["result"]["structuredContent"]
                .to_string()
                .contains("cloudcontractmarker")
        );

        let other_workspace_id = Uuid::new_v4();
        let other_workspace_slug =
            format!("other-{}", &other_workspace_id.simple().to_string()[..12]);
        let other_token = test_token(
            other_workspace_id,
            &other_workspace_slug,
            "ai-workspace:read",
        );
        for version in ["2025-11-25", "2025-06-18"] {
            for (call_token, contains_document) in [(&token, true), (&other_token, false)] {
                let response = client.post(format!("{base_url}/mcp"))
                    .bearer_auth(call_token).header("MCP-Protocol-Version", version)
                    .json(&json!({"jsonrpc":"2.0", "id":44, "method":"tools/call",
                        "params":{"name":"workspace_search_fulltext", "arguments":{"query":"cloudcontractmarker"}}}))
                    .send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body: Value = response.json().await.unwrap();
                let text = body["result"]["content"][0]["text"].as_str().unwrap();
                assert_eq!(text.contains("cloudcontractmarker"), contains_document);
                assert!(body["result"].get("structuredContent").is_none());
                assert!(body["result"].get("resultType").is_none());
            }
            let forbidden_tool = client
                .post(format!("{base_url}/mcp"))
                .bearer_auth(&token)
                .header("MCP-Protocol-Version", version)
                .json(&json!({"jsonrpc":"2.0", "id":45, "method":"tools/call",
                    "params":{"name":"project_file_write", "arguments":{}}}))
                .send()
                .await
                .unwrap();
            assert_eq!(forbidden_tool.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                forbidden_tool.json::<Value>().await.unwrap()["error"]["code"],
                -32602
            );
        }
        let cross_tenant = hosted_request(
            &client,
            &base_url,
            &other_token,
            "tools/call",
            Some("workspace_search_fulltext"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({
                "name": "workspace_search_fulltext",
                "arguments": {"query": "cloudcontractmarker"}
            }),
        )
        .await;
        assert_eq!(cross_tenant.status(), StatusCode::OK);
        assert_eq!(
            cross_tenant.json::<Value>().await.unwrap()["result"]["structuredContent"],
            json!([])
        );

        changed.snapshot.project.name =
            "x".repeat(super::super::store::MAX_CLOUD_CONTEXT_BYTES as usize);
        changed.snapshot_hash = sha256_hex(&serde_json::to_vec(&changed.snapshot).unwrap());
        let large_push = client
            .put(&snapshot_url)
            .bearer_auth(&force_token)
            .json(&changed)
            .send()
            .await
            .unwrap();
        assert_eq!(large_push.status(), StatusCode::OK);
        for tool in [
            "workspace_context",
            "workspace_service_graph",
            "workspace_events",
        ] {
            let limited = hosted_request(
                &client,
                &base_url,
                &token,
                "tools/call",
                Some(tool),
                super::super::mcp::PROTOCOL_VERSION,
                json!({"name":tool,"arguments":{}}),
            )
            .await;
            assert_eq!(limited.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let error: Value = limited.json().await.unwrap();
            assert_eq!(error["error"]["code"], -32602);
            assert!(
                error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("use search")
            );
        }
        let exact_read = hosted_request(
            &client,
            &base_url,
            &token,
            "tools/call",
            Some("workspace_read"),
            super::super::mcp::PROTOCOL_VERSION,
            json!({"name":"workspace_read", "arguments":{"document_key":"file:demo:README.md"}}),
        )
        .await;
        assert_eq!(exact_read.status(), StatusCode::OK);
        assert_eq!(
            exact_read.json::<Value>().await.unwrap()["result"]["structuredContent"]["content"],
            "cloudcontractmarker changed"
        );

        cloud_server.abort();
        jwks_server.abort();
    }
}
