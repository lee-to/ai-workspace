use super::models::{
    CloudPushError, CloudPushRequest, CloudPushResponse, MAX_CLOUD_SNAPSHOT_BYTES, validate_slug,
};
use super::snapshot::BuiltCloudSnapshot;
use crate::db::Db;
use anyhow::{Context as _, Result, bail};
use log::{debug, error, info, warn};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use std::io::Read as _;
use std::net::IpAddr;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_ENVELOPE_BYTES: usize = MAX_CLOUD_SNAPSHOT_BYTES + 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub struct CloudClientConfig {
    endpoint: reqwest::Url,
    endpoint_key: String,
    workspace_slug: String,
    token: String,
}

impl std::fmt::Debug for CloudClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudClientConfig")
            .field("endpoint_host", &self.endpoint.host_str())
            .field("workspace_slug", &self.workspace_slug)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl CloudClientConfig {
    pub fn new(endpoint: &str, workspace_slug: &str, token: &str) -> Result<Self> {
        if endpoint.trim().is_empty() {
            bail!("Cloud URL is required");
        }
        if token.trim().is_empty() {
            bail!("AI_WORKSPACE_CLOUD_TOKEN is required");
        }
        validate_slug(workspace_slug).context("Invalid cloud workspace slug")?;

        let mut endpoint =
            reqwest::Url::parse(endpoint).context("Cloud URL must be a valid absolute URL")?;
        let host = endpoint
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Cloud URL must include a host"))?;
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && is_loopback) {
            bail!("Cloud URL must use HTTPS (HTTP is allowed only for loopback testing)");
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            bail!("Cloud URL must not contain credentials, query parameters, or a fragment");
        }
        let path = endpoint.path().trim_end_matches('/').to_owned();
        let normalized_path = if path.is_empty() {
            "/".to_owned()
        } else {
            format!("{path}/")
        };
        endpoint.set_path(&normalized_path);
        let endpoint_key = endpoint.as_str().trim_end_matches('/').to_owned();

        Ok(Self {
            endpoint,
            endpoint_key,
            workspace_slug: workspace_slug.to_owned(),
            token: token.to_owned(),
        })
    }

    pub fn endpoint_key(&self) -> &str {
        &self.endpoint_key
    }

    pub fn workspace_slug(&self) -> &str {
        &self.workspace_slug
    }

    fn endpoint_host(&self) -> &str {
        self.endpoint.host_str().unwrap_or("unknown")
    }

    fn snapshot_url(&self, project_slug: &str) -> Result<reqwest::Url> {
        validate_slug(project_slug).context("Invalid cloud project slug")?;
        self.endpoint
            .join(&format!(
                "api/v1/workspaces/{}/projects/{project_slug}/snapshot",
                self.workspace_slug
            ))
            .context("Failed to build cloud snapshot URL")
    }
}

pub fn push_snapshot(
    db: &Db,
    config: &CloudClientConfig,
    built: &BuiltCloudSnapshot,
    force: bool,
) -> Result<CloudPushResponse> {
    push_snapshot_with_timeout(db, config, built, force, REQUEST_TIMEOUT)
}

fn push_snapshot_with_timeout(
    db: &Db,
    config: &CloudClientConfig,
    built: &BuiltCloudSnapshot,
    force: bool,
    request_timeout: Duration,
) -> Result<CloudPushResponse> {
    let started = Instant::now();
    let project_slug = &built.snapshot.project.slug;
    let previous =
        db.get_cloud_sync_state(config.endpoint_key(), config.workspace_slug(), project_slug)?;
    let request = CloudPushRequest {
        base_revision: previous.as_ref().map(|state| state.revision),
        force,
        snapshot_hash: built.snapshot_hash.clone(),
        snapshot: built.snapshot.clone(),
    };
    let request_body = serde_json::to_vec(&request)?;
    if request_body.len() > MAX_REQUEST_ENVELOPE_BYTES {
        warn!(
            "cloud push rejected before network workspace_slug='{}' project_slug='{}' payload_bytes={}",
            config.workspace_slug(),
            project_slug,
            request_body.len()
        );
        bail!("Cloud push request exceeds {MAX_REQUEST_ENVELOPE_BYTES} bytes");
    }

    info!(
        "cloud push start endpoint_host='{}' workspace_slug='{}' project_slug='{}' base_revision={:?} documents={}",
        config.endpoint_host(),
        config.workspace_slug(),
        project_slug,
        request.base_revision,
        request.snapshot.documents.len()
    );
    debug!(
        "cloud push payload_bytes={} hash_prefix='{}' force={}",
        request_body.len(),
        &built.snapshot_hash[..built.snapshot_hash.len().min(12)],
        force
    );

    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        .context("Failed to initialize cloud HTTP client")?;
    let response = client
        .put(config.snapshot_url(project_slug)?)
        .bearer_auth(&config.token)
        .header("Idempotency-Key", &built.snapshot_hash)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request_body)
        .send()
        .map_err(|error| {
            let error = error.without_url();
            error!(
                "cloud push transport failure endpoint_host='{}' workspace_slug='{}' project_slug='{}' error={}",
                config.endpoint_host(),
                config.workspace_slug(),
                project_slug,
                error
            );
            error
        })
        .context("Cloud push request failed")?;
    let status = response.status();
    debug!(
        "cloud push response endpoint_host='{}' status={}",
        config.endpoint_host(),
        status
    );
    let response_body = read_bounded_response(response)?;

    if !status.is_success() {
        return Err(status_error(status, &response_body, project_slug));
    }
    let accepted: CloudPushResponse =
        serde_json::from_slice(&response_body).context("Cloud returned an invalid response")?;
    if accepted.revision <= 0 {
        bail!("Cloud returned an invalid revision");
    }
    if accepted.snapshot_hash != built.snapshot_hash {
        bail!("Cloud returned a snapshot hash that does not match the uploaded snapshot");
    }

    db.save_cloud_sync_state(
        config.endpoint_key(),
        config.workspace_slug(),
        project_slug,
        accepted.revision,
        &accepted.snapshot_hash,
    )?;
    debug!(
        "cloud push state transition project_slug='{}' old_revision={:?} new_revision={} no_op={}",
        project_slug,
        previous.map(|state| state.revision),
        accepted.revision,
        accepted.no_op
    );
    info!(
        "cloud push complete endpoint_host='{}' workspace_slug='{}' project_slug='{}' revision={} no_op={} duration_ms={}",
        config.endpoint_host(),
        config.workspace_slug(),
        project_slug,
        accepted.revision,
        accepted.no_op,
        started.elapsed().as_millis()
    );
    Ok(accepted)
}

fn read_bounded_response(response: Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .context("Failed to read cloud response")?;
    if body.len() > MAX_RESPONSE_BYTES {
        bail!("Cloud response exceeds {MAX_RESPONSE_BYTES} bytes");
    }
    Ok(body)
}

fn status_error(status: StatusCode, body: &[u8], project_slug: &str) -> anyhow::Error {
    let details = serde_json::from_slice::<CloudPushError>(body).ok();
    match status {
        StatusCode::UNAUTHORIZED => {
            anyhow::anyhow!("Cloud authentication failed; check AI_WORKSPACE_CLOUD_TOKEN")
        }
        StatusCode::FORBIDDEN => {
            anyhow::anyhow!("Cloud token lacks access to the requested workspace")
        }
        StatusCode::CONFLICT => {
            let revision = details.as_ref().and_then(|error| error.current_revision);
            let hash = details
                .as_ref()
                .and_then(|error| error.current_snapshot_hash.as_deref())
                .unwrap_or("unknown");
            warn!(
                "cloud push conflict project_slug='{}' remote_revision={:?} remote_hash_prefix='{}'",
                project_slug,
                revision,
                &hash[..hash.len().min(12)]
            );
            anyhow::anyhow!(
                "Cloud snapshot conflict (remote revision: {}, hash: {}); review the remote state and retry deliberately with --force",
                revision
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                hash
            )
        }
        StatusCode::PAYLOAD_TOO_LARGE => {
            anyhow::anyhow!("Cloud rejected the snapshot because it is too large")
        }
        StatusCode::TOO_MANY_REQUESTS => {
            warn!("cloud push throttled project_slug='{}'", project_slug);
            anyhow::anyhow!("Cloud push was throttled; retry later")
        }
        status if status.is_server_error() => {
            warn!(
                "cloud push recoverable server failure project_slug='{}' status={}",
                project_slug, status
            );
            anyhow::anyhow!("Cloud service failed with HTTP {}", status.as_u16())
        }
        _ => anyhow::anyhow!("Cloud rejected the snapshot with HTTP {}", status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::models::CloudSnapshotCounts;
    use crate::cloud::snapshot::build_project_snapshot;
    use crate::models::Project;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    fn fixture() -> (tempfile::TempDir, Db, Project, BuiltCloudSnapshot) {
        let root = tempfile::tempdir().unwrap();
        let db = Db::open(&root.path().join("workspace.db")).unwrap();
        let id = db
            .create_project_with_slug("API", root.path().to_str().unwrap(), Some("api"))
            .unwrap();
        let project = db.get_project_by_id(id).unwrap().unwrap();
        let built = build_project_snapshot(&db, &project, false).unwrap();
        (root, db, project, built)
    }

    fn serve_once(
        status: u16,
        body: String,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let length = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut request_body = vec![0; length];
            reader.read_exact(&mut request_body).unwrap();
            request.push_str(&String::from_utf8_lossy(&request_body));
            sender.send(request).unwrap();
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (format!("http://{address}"), receiver, handle)
    }

    #[test]
    fn config_requires_https_except_loopback_and_redacts_token() {
        assert!(CloudClientConfig::new("", "team", "token").is_err());
        assert!(CloudClientConfig::new("https://cloud.example", "", "token").is_err());
        assert!(CloudClientConfig::new("https://cloud.example", "team", "").is_err());
        assert!(CloudClientConfig::new("http://cloud.example", "team", "token").is_err());
        let config = CloudClientConfig::new("http://127.0.0.1:1234", "team", "secret").unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn successful_push_updates_local_revision_state() {
        let (_root, db, project, built) = fixture();
        let response = serde_json::to_string(&CloudPushResponse {
            revision: 1,
            snapshot_hash: built.snapshot_hash.clone(),
            counts: CloudSnapshotCounts::default(),
            no_op: false,
        })
        .unwrap();
        let (endpoint, request, server) = serve_once(200, response);
        let config = CloudClientConfig::new(&endpoint, "team", "top-secret").unwrap();

        let accepted = push_snapshot(&db, &config, &built, false).unwrap();
        assert_eq!(accepted.revision, 1);
        let state = db
            .get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
            .unwrap()
            .unwrap();
        assert_eq!(state.revision, 1);
        let request = request.recv().unwrap();
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer top-secret")
        );
        assert!(request.contains("\"base_revision\":null"));
        server.join().unwrap();
    }

    #[test]
    fn repeated_no_op_push_reuses_saved_revision() {
        let (_root, db, project, built) = fixture();
        let response = serde_json::to_string(&CloudPushResponse {
            revision: 7,
            snapshot_hash: built.snapshot_hash.clone(),
            counts: CloudSnapshotCounts::default(),
            no_op: true,
        })
        .unwrap();
        let (endpoint, request, server) = serve_once(200, response);
        let config = CloudClientConfig::new(&endpoint, "team", "top-secret").unwrap();
        db.save_cloud_sync_state(
            config.endpoint_key(),
            "team",
            &project.slug,
            7,
            &built.snapshot_hash,
        )
        .unwrap();

        let accepted = push_snapshot(&db, &config, &built, false).unwrap();
        assert!(accepted.no_op);
        assert_eq!(accepted.revision, 7);
        let request = request.recv().unwrap();
        assert!(request.contains("\"base_revision\":7"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("idempotency-key: {}", built.snapshot_hash))
        );
        let state = db
            .get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
            .unwrap()
            .unwrap();
        assert_eq!(state.revision, 7);
        assert_eq!(state.snapshot_hash, built.snapshot_hash);
        server.join().unwrap();
    }

    #[test]
    fn oversized_request_is_rejected_without_saving_state() {
        let (_root, db, project, mut built) = fixture();
        built.snapshot.project.name = "x".repeat(MAX_REQUEST_ENVELOPE_BYTES);
        let config = CloudClientConfig::new("http://127.0.0.1:1", "team", "top-secret").unwrap();

        let error = push_snapshot(&db, &config, &built, false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("Cloud push request exceeds {MAX_REQUEST_ENVELOPE_BYTES} bytes")
        );
        assert!(!error.contains("top-secret"));
        assert!(
            db.get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn request_timeout_does_not_save_state_or_expose_token() {
        let (_root, db, project, built) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let config =
            CloudClientConfig::new(&format!("http://{address}"), "team", "top-secret").unwrap();

        let error =
            push_snapshot_with_timeout(&db, &config, &built, false, Duration::from_millis(25))
                .unwrap_err();
        assert!(error.chain().any(|source| {
            source
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        }));
        assert!(!format!("{error:#}").contains("top-secret"));
        assert!(
            db.get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
                .unwrap()
                .is_none()
        );
        server.join().unwrap();
    }

    #[test]
    fn conflict_does_not_change_local_state_or_expose_token() {
        let (_root, db, project, built) = fixture();
        let body = serde_json::to_string(&CloudPushError {
            code: "revision_conflict".into(),
            message: "conflict".into(),
            current_revision: Some(7),
            current_snapshot_hash: Some("b".repeat(64)),
        })
        .unwrap();
        let (endpoint, _request, server) = serve_once(409, body);
        let config = CloudClientConfig::new(&endpoint, "team", "top-secret").unwrap();

        let error = push_snapshot(&db, &config, &built, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--force"));
        assert!(!error.contains("top-secret"));
        assert!(
            db.get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
                .unwrap()
                .is_none()
        );
        server.join().unwrap();
    }

    #[test]
    fn http_errors_are_actionable_and_do_not_expose_the_token() {
        for (status, expected) in [
            (401, "authentication failed"),
            (403, "lacks access"),
            (413, "too large"),
            (429, "throttled"),
            (503, "HTTP 503"),
        ] {
            let (_root, db, _project, built) = fixture();
            let (endpoint, _request, server) = serve_once(status, "{}".into());
            let config = CloudClientConfig::new(&endpoint, "team", "top-secret").unwrap();

            let error = push_snapshot(&db, &config, &built, false)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(expected),
                "unexpected {status} error: {error}"
            );
            assert!(!error.contains("top-secret"));
            server.join().unwrap();
        }
    }

    #[test]
    fn oversized_response_is_rejected_without_saving_state() {
        let (_root, db, project, built) = fixture();
        let (endpoint, _request, server) = serve_once(200, "x".repeat(MAX_RESPONSE_BYTES + 1));
        let config = CloudClientConfig::new(&endpoint, "team", "top-secret").unwrap();

        let error = push_snapshot(&db, &config, &built, false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("Cloud response exceeds {MAX_RESPONSE_BYTES} bytes")
        );
        assert!(!error.contains("top-secret"));
        assert!(
            db.get_cloud_sync_state(config.endpoint_key(), "team", &project.slug)
                .unwrap()
                .is_none()
        );
        server.join().unwrap();
    }
}
