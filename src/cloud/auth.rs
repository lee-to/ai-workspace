use anyhow::{Context as _, Result, bail};
use axum::http::HeaderMap;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use log::{debug, error, warn};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

const JWKS_TTL: Duration = Duration::from_secs(300);
const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const JWKS_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks_uri: reqwest::Url,
}

impl AuthConfig {
    pub fn new(issuer: &str, audience: &str, jwks_uri: &str) -> Result<Self> {
        if issuer.trim().is_empty() || audience.trim().is_empty() {
            bail!("Cloud OIDC issuer and audience are required");
        }
        let jwks_uri = reqwest::Url::parse(jwks_uri).context("Invalid OIDC JWKS URI")?;
        let is_loopback = jwks_uri.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if jwks_uri.scheme() != "https" && !(jwks_uri.scheme() == "http" && is_loopback) {
            bail!("OIDC JWKS URI must use HTTPS (HTTP is allowed only for loopback testing)");
        }
        Ok(Self {
            issuer: issuer.trim_end_matches('/').to_owned(),
            audience: audience.to_owned(),
            jwks_uri,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AccessClaims {
    pub sub: String,
    pub workspace_id: Uuid,
    pub workspace_slug: String,
    #[serde(default)]
    pub scope: String,
    #[serde(rename = "exp")]
    pub _exp: u64,
    #[serde(rename = "nbf")]
    pub _nbf: Option<u64>,
}

impl AccessClaims {
    pub fn require_scope(&self, required: &str) -> Result<()> {
        if self.scope.split_whitespace().any(|scope| scope == required) {
            Ok(())
        } else {
            bail!("Token lacks required scope: {required}")
        }
    }
}

struct CachedKeys {
    loaded_at: Instant,
    keys: HashMap<String, Jwk>,
}

#[derive(Clone)]
pub struct JwtValidator {
    config: AuthConfig,
    client: reqwest::Client,
    cache: Arc<RwLock<Option<CachedKeys>>>,
    last_refresh_attempt: Arc<Mutex<Option<Instant>>>,
}

impl JwtValidator {
    pub fn new(config: AuthConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .context("Failed to initialize JWKS HTTP client")?;
        Ok(Self {
            config,
            client,
            cache: Arc::new(RwLock::new(None)),
            last_refresh_attempt: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<AccessClaims> {
        let authorization = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("Missing bearer token"))?;
        let token = authorization
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Invalid bearer authorization"))?;
        let header = decode_header(token).context("Invalid JWT header")?;
        let key_id = header.kid.context("JWT key ID is required")?;
        let algorithm = match header.alg {
            Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => header.alg,
            _ => bail!("Unsupported JWT signing algorithm"),
        };

        let mut jwk = self.cached_key(&key_id).await;
        if jwk.is_none() {
            self.refresh_keys().await?;
            jwk = self.cached_key(&key_id).await;
        }
        let jwk = jwk.ok_or_else(|| anyhow::anyhow!("JWT key ID is not trusted"))?;
        let key = DecodingKey::from_jwk(&jwk).context("Invalid trusted JWT key")?;
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = decode::<AccessClaims>(token, &key, &validation)
            .map_err(|error| {
                warn!(
                    "cloud authentication rejected key_id='{}': {}",
                    key_id, error
                );
                error
            })
            .context("Bearer token validation failed")?
            .claims;
        if claims.sub.trim().is_empty() || claims.workspace_slug.trim().is_empty() {
            bail!("Token subject and workspace claims are required");
        }
        debug!(
            "cloud authentication accepted workspace_id={} workspace_slug='{}' key_id='{}' scopes={}",
            claims.workspace_id,
            claims.workspace_slug,
            key_id,
            claims.scope.split_whitespace().count()
        );
        Ok(claims)
    }

    async fn cached_key(&self, key_id: &str) -> Option<Jwk> {
        let cache = self.cache.read().await;
        cache.as_ref().and_then(|cache| {
            (cache.loaded_at.elapsed() < JWKS_TTL)
                .then(|| cache.keys.get(key_id).cloned())
                .flatten()
        })
    }

    async fn refresh_keys(&self) -> Result<()> {
        // Serialize refreshes and bound unknown-kid traffic, including failed attempts.
        let mut last_attempt = self.last_refresh_attempt.lock().await;
        if last_attempt.is_some_and(|attempt| attempt.elapsed() < JWKS_REFRESH_INTERVAL) {
            return Ok(());
        }
        *last_attempt = Some(Instant::now());
        let mut response = self
            .client
            .get(self.config.jwks_uri.clone())
            .send()
            .await
            .map_err(|error| {
                let error = error.without_url();
                error!("JWKS refresh transport failure: {}", error);
                error
            })?
            .error_for_status()
            .context("JWKS endpoint returned an error")?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed to read JWKS response")?
        {
            if chunk.len() > JWKS_MAX_BYTES - body.len() {
                bail!("JWKS response exceeds {JWKS_MAX_BYTES} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        let set: JwkSet = serde_json::from_slice(&body).context("Invalid JWKS response")?;
        let keys = set
            .keys
            .into_iter()
            .filter_map(|key| key.common.key_id.clone().map(|key_id| (key_id, key)))
            .collect::<HashMap<_, _>>();
        if keys.is_empty() {
            bail!("JWKS response contains no keyed signing keys");
        }
        debug!("JWKS refresh complete keys={}", keys.len());
        *self.cache.write().await = Some(CachedKeys {
            loaded_at: Instant::now(),
            keys,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn oversized_jwks_is_rejected_before_response_finishes() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (finish, finished) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            // No final chunk: the validator must reject without waiting for EOF.
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                JWKS_MAX_BYTES + 1
            )
            .unwrap();
            stream.write_all(&vec![b' '; JWKS_MAX_BYTES + 1]).unwrap();
            finished.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let mut validator = JwtValidator::new(
            AuthConfig::new(
                "https://issuer.test",
                "test",
                &format!("http://{address}/jwks"),
            )
            .unwrap(),
        )
        .unwrap();
        validator.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let error = validator.refresh_keys().await.unwrap_err();
        finish.send(()).unwrap();
        server.join().unwrap();
        assert!(
            error.to_string().contains("JWKS response exceeds"),
            "{error:#}"
        );
        assert!(validator.cache.read().await.is_none());
    }

    #[tokio::test]
    async fn concurrent_and_repeated_jwks_misses_share_one_refresh() {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let provider = Arc::new(RwLock::new((StatusCode::OK, "known")));
        let response = provider.clone();
        let app = Router::new().route("/jwks", get(move || {
            let counter = counter.clone();
            let response = response.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let (status, key_id) = *response.read().await;
                (status, Json(json!({"keys": [{"kty": "RSA", "kid": key_id, "n": "AQAB", "e": "AQAB"}]})))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let validator = JwtValidator::new(
            AuthConfig::new(
                "https://issuer.test",
                "test",
                &format!("http://{address}/jwks"),
            )
            .unwrap(),
        )
        .unwrap();

        // RS256 headers with different unknown kids; neither token has a valid signature.
        let first = HeaderMap::from_iter([(
            axum::http::header::AUTHORIZATION,
            "Bearer eyJhbGciOiJSUzI1NiIsImtpZCI6InVua25vd24tYSJ9.e30.AAAA"
                .parse()
                .unwrap(),
        )]);
        let second = HeaderMap::from_iter([(
            axum::http::header::AUTHORIZATION,
            "Bearer eyJhbGciOiJSUzI1NiIsImtpZCI6InVua25vd24tYiJ9.e30.AAAA"
                .parse()
                .unwrap(),
        )]);
        let (first_result, second_result) = tokio::join!(
            validator.authenticate(&first),
            validator.authenticate(&second)
        );
        assert!(
            first_result
                .unwrap_err()
                .to_string()
                .contains("not trusted")
        );
        assert!(
            second_result
                .unwrap_err()
                .to_string()
                .contains("not trusted")
        );
        assert!(validator.authenticate(&first).await.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(validator.cached_key("known").await.is_some());

        // Rotation refreshes the complete key set after the short retry interval.
        *provider.write().await = (StatusCode::OK, "rotated");
        *validator.last_refresh_attempt.lock().await = Some(Instant::now() - JWKS_REFRESH_INTERVAL);
        validator.refresh_keys().await.unwrap();
        assert!(validator.cached_key("rotated").await.is_some());
        assert!(validator.cached_key("known").await.is_none());

        // Outages retain fresh cached keys, throttle retries, and never extend TTL.
        *provider.write().await = (StatusCode::SERVICE_UNAVAILABLE, "rotated");
        *validator.last_refresh_attempt.lock().await = Some(Instant::now() - JWKS_REFRESH_INTERVAL);
        assert!(validator.refresh_keys().await.is_err());
        assert!(validator.cached_key("rotated").await.is_some());
        validator.refresh_keys().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        validator.cache.write().await.as_mut().unwrap().loaded_at = Instant::now() - JWKS_TTL;
        assert!(validator.cached_key("rotated").await.is_none());
        *validator.last_refresh_attempt.lock().await = Some(Instant::now() - JWKS_REFRESH_INTERVAL);
        assert!(validator.refresh_keys().await.is_err());
        assert!(validator.cached_key("rotated").await.is_none());
        assert_eq!(requests.load(Ordering::SeqCst), 4);
        server.abort();
    }

    #[test]
    fn auth_config_requires_https_and_claim_scope_is_exact() {
        assert!(
            AuthConfig::new(
                "https://issuer.example",
                "api",
                "http://issuer.example/jwks"
            )
            .is_err()
        );
        let claims = AccessClaims {
            sub: "user".into(),
            workspace_id: Uuid::nil(),
            workspace_slug: "team".into(),
            scope: "ai-workspace:read other".into(),
            _exp: u64::MAX,
            _nbf: None,
        };
        assert!(claims.require_scope("ai-workspace:read").is_ok());
        assert!(claims.require_scope("ai-workspace:push").is_err());
    }
}
