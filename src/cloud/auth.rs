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
use tokio::sync::RwLock;
use uuid::Uuid;

const JWKS_TTL: Duration = Duration::from_secs(300);
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
        let response = self
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
        let body = response
            .bytes()
            .await
            .context("Failed to read JWKS response")?;
        if body.len() > JWKS_MAX_BYTES {
            bail!("JWKS response exceeds {JWKS_MAX_BYTES} bytes");
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
