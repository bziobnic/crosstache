//! GitHub Actions OIDC → Azure AD workload identity federation.
//!
//! Lets `xv` authenticate in CI with **no stored secret and no `azure/login`
//! step**. The exchange is the standard federated-credential flow:
//!
//! 1. Ask the Actions runtime for an OIDC ID token, using the
//!    `ACTIONS_ID_TOKEN_REQUEST_URL` / `ACTIONS_ID_TOKEN_REQUEST_TOKEN` pair
//!    that GitHub injects when a job declares `permissions: id-token: write`.
//!    The audience is `api://AzureADTokenExchange`, which is what a federated
//!    credential on the app registration expects.
//! 2. Present that token to Azure AD as a `client_assertion` in a
//!    `client_credentials` grant. Azure AD validates the issuer, subject
//!    (`repo:owner/name:ref:...`), and audience against the app registration's
//!    federated credential, then returns an access token.
//!
//! Nothing here is GitHub-specific beyond step 1's env-var protocol; the
//! assertion exchange in step 2 is generic OIDC federation.
//!
//! ## Why this is not just `azure/login`
//!
//! `azure/login` sets `AZURE_*` env vars that `EnvironmentCredential` then
//! picks up, so federation "works" today only as a side effect of another
//! action having run first. Doing the exchange in-process means the CI recipe
//! is one step instead of two, `xv` reports federation failures with its own
//! diagnostics, and nothing depends on env vars set by a third party.
//!
//! ## Testability
//!
//! Both HTTP calls go through [`TokenHttp`], so the exchange logic — request
//! shape, error mapping, expiry handling — is unit-tested against a scripted
//! transport with no network. See the tests at the bottom of this file.

use std::sync::Arc;

use async_trait::async_trait;
use azure_core::auth::{AccessToken, TokenCredential};
use serde::Deserialize;

use crate::error::{CrosstacheError, Result};

/// Audience Azure AD requires on a federated ID token.
const AZURE_AUDIENCE: &str = "api://AzureADTokenExchange";

/// The client-assertion type for a JWT bearer assertion (RFC 7523).
const ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// Env var GitHub sets to the ID-token request endpoint.
pub const ENV_REQUEST_URL: &str = "ACTIONS_ID_TOKEN_REQUEST_URL";

/// Env var GitHub sets to the bearer token for that endpoint.
pub const ENV_REQUEST_TOKEN: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";

/// Minimal HTTP surface the exchange needs, so tests can script both calls
/// without a server.
#[async_trait]
pub trait TokenHttp: Send + Sync + std::fmt::Debug {
    /// `GET url` with `Authorization: Bearer <bearer>`; returns the body.
    async fn get_with_bearer(&self, url: &str, bearer: &str) -> Result<String>;

    /// `POST url` with a form body; returns the (status, body) pair so the
    /// caller can map Azure AD's error envelope.
    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)>;
}

/// Real transport over the shared `reqwest` client.
#[derive(Debug)]
pub struct ReqwestTokenHttp {
    client: reqwest::Client,
}

impl ReqwestTokenHttp {
    /// Build a transport using the project's standard timeouts and user agent.
    pub fn new() -> Result<Self> {
        use crate::utils::network::{create_http_client, NetworkConfig};
        Ok(Self {
            client: create_http_client(&NetworkConfig::default())?,
        })
    }
}

#[async_trait]
impl TokenHttp for ReqwestTokenHttp {
    async fn get_with_bearer(&self, url: &str, bearer: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .map_err(|e| {
                CrosstacheError::authentication(format!(
                    "failed to request a GitHub OIDC token: {e}"
                ))
            })?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(CrosstacheError::authentication(format!(
                "GitHub OIDC token request failed with HTTP {}: {}",
                status.as_u16(),
                truncate(&body)
            )));
        }
        Ok(body)
    }

    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
        let resp = self.client.post(url).form(form).send().await.map_err(|e| {
            CrosstacheError::authentication(format!("federated token exchange failed: {e}"))
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Ok((status, body))
    }
}

/// Where the ID token comes from and which app registration it federates into.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Azure AD tenant ID.
    pub tenant_id: String,
    /// Application (client) ID with a federated credential configured.
    pub client_id: String,
    /// GitHub's ID-token request endpoint.
    pub request_url: String,
    /// Bearer token for that endpoint.
    pub request_token: String,
}

impl OidcConfig {
    /// Read the configuration from the environment.
    ///
    /// Returns a diagnostic error naming exactly what is missing, since the two
    /// common misconfigurations (no `id-token: write` permission, no configured
    /// client/tenant) look identical from the outside otherwise.
    pub fn from_env(tenant_id: Option<&str>, client_id: Option<&str>) -> Result<Self> {
        let request_url = std::env::var(ENV_REQUEST_URL).map_err(|_| {
            CrosstacheError::authentication(format!(
                "{ENV_REQUEST_URL} is not set, so no OIDC token can be requested. In GitHub \
                 Actions this means the job is missing the required permission — add:\n  \
                 permissions:\n    id-token: write\nto the job (or workflow). Outside GitHub \
                 Actions, use a different credential type."
            ))
        })?;
        let request_token = std::env::var(ENV_REQUEST_TOKEN).map_err(|_| {
            CrosstacheError::authentication(format!(
                "{ENV_REQUEST_TOKEN} is not set, so the OIDC token request cannot be \
                 authenticated. This normally accompanies {ENV_REQUEST_URL}; if one is present \
                 without the other, the workflow environment has been altered."
            ))
        })?;

        let tenant_id = tenant_id
            .map(str::to_string)
            .or_else(|| std::env::var("AZURE_TENANT_ID").ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CrosstacheError::authentication(
                    "OIDC authentication needs a tenant ID. Set tenant_id in your config or \
                     AZURE_TENANT_ID in the environment."
                        .to_string(),
                )
            })?;

        let client_id = client_id
            .map(str::to_string)
            .or_else(|| std::env::var("AZURE_CLIENT_ID").ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CrosstacheError::authentication(
                    "OIDC authentication needs the client ID of an app registration with a \
                     federated credential. Set AZURE_CLIENT_ID."
                        .to_string(),
                )
            })?;

        Ok(Self {
            tenant_id,
            client_id,
            request_url,
            request_token,
        })
    }

    /// Whether the ambient environment can support an OIDC exchange at all.
    ///
    /// Used to decide whether OIDC belongs in an automatic credential chain,
    /// without producing an error when it does not.
    pub fn available_in_env() -> bool {
        std::env::var(ENV_REQUEST_URL).is_ok() && std::env::var(ENV_REQUEST_TOKEN).is_ok()
    }
}

/// A [`TokenCredential`] that federates a GitHub OIDC token into Azure AD.
#[derive(Debug)]
pub struct GithubOidcCredential {
    config: OidcConfig,
    http: Arc<dyn TokenHttp>,
}

impl GithubOidcCredential {
    /// Build with the real HTTP transport.
    pub fn new(config: OidcConfig) -> Result<Self> {
        Ok(Self {
            config,
            http: Arc::new(ReqwestTokenHttp::new()?),
        })
    }

    /// Build with a caller-supplied transport, so the exchange can be driven
    /// against a scripted [`TokenHttp`] without a network.
    #[cfg(test)]
    pub fn with_http(config: OidcConfig, http: Arc<dyn TokenHttp>) -> Self {
        Self { config, http }
    }

    /// Fetch the GitHub ID token for the Azure AD audience.
    async fn fetch_id_token(&self) -> Result<String> {
        // GitHub's endpoint already carries query parameters, so the audience
        // must be appended with `&`, not `?`.
        let separator = if self.config.request_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{separator}audience={}",
            self.config.request_url,
            urlencoding::encode(AZURE_AUDIENCE)
        );

        let body = self
            .http
            .get_with_bearer(&url, &self.config.request_token)
            .await?;

        #[derive(Deserialize)]
        struct IdTokenResponse {
            value: Option<String>,
        }

        let parsed: IdTokenResponse = serde_json::from_str(&body).map_err(|e| {
            CrosstacheError::authentication(format!(
                "GitHub OIDC token response was not valid JSON: {e}"
            ))
        })?;

        parsed.value.filter(|v| !v.is_empty()).ok_or_else(|| {
            CrosstacheError::authentication(
                "GitHub returned an OIDC response with no token value.".to_string(),
            )
        })
    }

    /// Exchange the ID token for an Azure AD access token scoped to `scopes`.
    async fn exchange(&self, scopes: &[&str], assertion: &str) -> Result<AccessToken> {
        let scope = scopes.join(" ");
        let url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.config.tenant_id
        );

        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("scope", scope.as_str()),
            ("grant_type", "client_credentials"),
            ("client_assertion_type", ASSERTION_TYPE),
            ("client_assertion", assertion),
        ];

        let (status, body) = self.http.post_form(&url, &form).await?;

        if status != 200 {
            return Err(CrosstacheError::authentication(format!(
                "Azure AD rejected the federated credential (HTTP {status}): {}\n  \
                 Check that the app registration '{}' has a federated credential whose subject \
                 matches this workflow (issuer https://token.actions.githubusercontent.com, \
                 audience {AZURE_AUDIENCE}) and that tenant '{}' is correct.",
                describe_aad_error(&body),
                self.config.client_id,
                self.config.tenant_id
            )));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            /// Seconds until expiry. Azure AD always sends it; treated as
            /// required so a missing value cannot silently mint a token that
            /// looks permanently valid.
            expires_in: Option<i64>,
        }

        let parsed: TokenResponse = serde_json::from_str(&body).map_err(|e| {
            CrosstacheError::authentication(format!(
                "Azure AD token response was not valid JSON: {e}"
            ))
        })?;

        let expires_in = parsed.expires_in.ok_or_else(|| {
            CrosstacheError::authentication(
                "Azure AD token response omitted expires_in; refusing to treat the token as \
                 valid for an unknown period."
                    .to_string(),
            )
        })?;

        let expires_on = time::OffsetDateTime::now_utc() + time::Duration::seconds(expires_in);
        Ok(AccessToken::new(parsed.access_token, expires_on))
    }
}

#[async_trait]
impl TokenCredential for GithubOidcCredential {
    async fn get_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        let assertion = self.fetch_id_token().await.map_err(to_azure_error)?;
        self.exchange(scopes, &assertion)
            .await
            .map_err(to_azure_error)
    }

    async fn clear_cache(&self) -> azure_core::Result<()> {
        // Nothing is cached here: each call re-requests a short-lived ID token.
        // Azure SDK clients wrap credentials in their own caching layer.
        Ok(())
    }
}

/// Map a crosstache error into the Azure SDK's error type.
fn to_azure_error(e: CrosstacheError) -> azure_core::Error {
    azure_core::Error::new(azure_core::error::ErrorKind::Credential, e)
}

/// Pull `error_description` out of an Azure AD error envelope when present.
///
/// Azure AD's descriptions carry the actionable detail (e.g. `AADSTS700213: No
/// matching federated identity record found`), which a bare status code does
/// not. Falls back to the truncated raw body.
fn describe_aad_error(body: &str) -> String {
    #[derive(Deserialize)]
    struct AadError {
        error_description: Option<String>,
        error: Option<String>,
    }
    match serde_json::from_str::<AadError>(body) {
        Ok(parsed) => parsed
            .error_description
            .or(parsed.error)
            // Descriptions are multi-line with a trailing correlation block;
            // the first line is the part worth surfacing.
            .map(|d| d.lines().next().unwrap_or_default().to_string())
            .unwrap_or_else(|| truncate(body)),
        Err(_) => truncate(body),
    }
}

/// Bound an untrusted body before it reaches an error message.
fn truncate(body: &str) -> String {
    const MAX: usize = 300;
    let trimmed = body.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let cut = trimmed
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= MAX)
        .last()
        .unwrap_or(0);
    format!("{}…", &trimmed[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scripted transport recording what it was asked for.
    #[derive(Debug, Default)]
    struct FakeHttp {
        id_token_body: String,
        exchange_status: u16,
        exchange_body: String,
        seen_get_url: Mutex<Option<String>>,
        seen_get_bearer: Mutex<Option<String>>,
        seen_post_url: Mutex<Option<String>>,
        seen_form: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl TokenHttp for FakeHttp {
        async fn get_with_bearer(&self, url: &str, bearer: &str) -> Result<String> {
            *self.seen_get_url.lock().unwrap() = Some(url.to_string());
            *self.seen_get_bearer.lock().unwrap() = Some(bearer.to_string());
            Ok(self.id_token_body.clone())
        }

        async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
            *self.seen_post_url.lock().unwrap() = Some(url.to_string());
            *self.seen_form.lock().unwrap() = form
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            Ok((self.exchange_status, self.exchange_body.clone()))
        }
    }

    fn config() -> OidcConfig {
        OidcConfig {
            tenant_id: "tenant-123".into(),
            client_id: "client-abc".into(),
            request_url: "https://pipelines.example.com/token?api-version=2.0".into(),
            request_token: "runtime-bearer".into(),
        }
    }

    fn credential(http: Arc<FakeHttp>) -> GithubOidcCredential {
        GithubOidcCredential::with_http(config(), http)
    }

    #[tokio::test]
    async fn successful_exchange_returns_a_token_with_expiry() {
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"github.jwt.here"}"#.into(),
            exchange_status: 200,
            exchange_body: r#"{"access_token":"aad-token","expires_in":3599}"#.into(),
            ..Default::default()
        });
        let cred = credential(Arc::clone(&http));

        let token = cred
            .get_token(&["https://vault.azure.net/.default"])
            .await
            .expect("exchange should succeed");

        assert_eq!(token.token.secret(), "aad-token");
        let remaining = token.expires_on - time::OffsetDateTime::now_utc();
        assert!(
            remaining.whole_seconds() > 3500 && remaining.whole_seconds() <= 3599,
            "expiry should track expires_in, got {remaining}"
        );
    }

    #[tokio::test]
    async fn id_token_request_uses_the_azure_audience_and_runtime_bearer() {
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"jwt"}"#.into(),
            exchange_status: 200,
            exchange_body: r#"{"access_token":"t","expires_in":60}"#.into(),
            ..Default::default()
        });
        let cred = credential(Arc::clone(&http));
        cred.get_token(&["scope/.default"]).await.unwrap();

        let url = http.seen_get_url.lock().unwrap().clone().unwrap();
        // Appended with `&` because GitHub's URL already has a query string —
        // using `?` would produce an invalid URL and a confusing 400.
        assert!(
            url.starts_with("https://pipelines.example.com/token?api-version=2.0&audience="),
            "{url}"
        );
        assert!(
            url.contains("api%3A%2F%2FAzureADTokenExchange"),
            "audience must be URL-encoded: {url}"
        );
        assert_eq!(
            http.seen_get_bearer.lock().unwrap().clone().unwrap(),
            "runtime-bearer"
        );
    }

    #[tokio::test]
    async fn exchange_posts_a_correct_client_credentials_assertion() {
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"github.jwt.here"}"#.into(),
            exchange_status: 200,
            exchange_body: r#"{"access_token":"t","expires_in":60}"#.into(),
            ..Default::default()
        });
        let cred = credential(Arc::clone(&http));
        cred.get_token(&["https://vault.azure.net/.default", "extra"])
            .await
            .unwrap();

        assert_eq!(
            http.seen_post_url.lock().unwrap().clone().unwrap(),
            "https://login.microsoftonline.com/tenant-123/oauth2/v2.0/token"
        );

        let form = http.seen_form.lock().unwrap().clone();
        let get = |k: &str| {
            form.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing form field {k} in {form:?}"))
        };
        assert_eq!(get("client_id"), "client-abc");
        assert_eq!(get("grant_type"), "client_credentials");
        assert_eq!(get("client_assertion_type"), ASSERTION_TYPE);
        // The GitHub ID token is the assertion — never a client secret.
        assert_eq!(get("client_assertion"), "github.jwt.here");
        // Multiple scopes are space-joined per OAuth 2.0.
        assert_eq!(get("scope"), "https://vault.azure.net/.default extra");
        assert!(
            !form.iter().any(|(k, _)| k == "client_secret"),
            "a federated exchange must never send a client secret: {form:?}"
        );
    }

    #[tokio::test]
    async fn azure_ad_rejection_surfaces_the_actionable_description() {
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"jwt"}"#.into(),
            exchange_status: 400,
            exchange_body: r#"{"error":"invalid_request","error_description":"AADSTS700213: No matching federated identity record found for presented assertion subject.\r\nTrace ID: abc\r\nCorrelation ID: def"}"#.into(),
            ..Default::default()
        });
        let cred = credential(Arc::clone(&http));
        let err = cred.get_token(&["s"]).await.expect_err("should fail");
        let msg = format!("{err:#}");

        assert!(msg.contains("AADSTS700213"), "{msg}");
        // The trace/correlation noise is dropped; the first line is the signal.
        assert!(!msg.contains("Trace ID"), "{msg}");
        // And the message must say what to fix.
        assert!(msg.contains("federated credential"), "{msg}");
        assert!(msg.contains("client-abc"), "{msg}");
    }

    #[tokio::test]
    async fn missing_id_token_value_is_an_error() {
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":""}"#.into(),
            exchange_status: 200,
            exchange_body: "{}".into(),
            ..Default::default()
        });
        let err = credential(http)
            .get_token(&["s"])
            .await
            .expect_err("should fail");
        assert!(format!("{err:#}").contains("no token value"), "{err:#}");
    }

    #[tokio::test]
    async fn token_response_without_expires_in_is_rejected() {
        // A token with unknown lifetime would be cached as if never expiring.
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"jwt"}"#.into(),
            exchange_status: 200,
            exchange_body: r#"{"access_token":"t"}"#.into(),
            ..Default::default()
        });
        let err = credential(http)
            .get_token(&["s"])
            .await
            .expect_err("should fail");
        assert!(format!("{err:#}").contains("expires_in"), "{err:#}");
    }

    #[tokio::test]
    async fn malformed_json_is_reported_not_panicked() {
        let http = Arc::new(FakeHttp {
            id_token_body: "<html>502</html>".into(),
            exchange_status: 200,
            exchange_body: "{}".into(),
            ..Default::default()
        });
        let err = credential(http)
            .get_token(&["s"])
            .await
            .expect_err("should fail");
        assert!(format!("{err:#}").contains("not valid JSON"), "{err:#}");
    }

    #[test]
    fn audience_appends_with_question_mark_when_url_has_no_query() {
        // Guards the other branch of the separator choice.
        let mut cfg = config();
        cfg.request_url = "https://example.com/token".into();
        let http = Arc::new(FakeHttp {
            id_token_body: r#"{"value":"jwt"}"#.into(),
            exchange_status: 200,
            exchange_body: r#"{"access_token":"t","expires_in":60}"#.into(),
            ..Default::default()
        });
        let cred = GithubOidcCredential::with_http(cfg, Arc::clone(&http) as Arc<dyn TokenHttp>);
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { cred.get_token(&["s"]).await.unwrap() });
        let url = http.seen_get_url.lock().unwrap().clone().unwrap();
        assert!(
            url.starts_with("https://example.com/token?audience="),
            "{url}"
        );
    }

    #[test]
    fn describe_aad_error_handles_non_json_and_bare_error() {
        assert_eq!(
            describe_aad_error("plain text failure"),
            "plain text failure"
        );
        assert_eq!(
            describe_aad_error(r#"{"error":"unauthorized_client"}"#),
            "unauthorized_client"
        );
    }

    #[test]
    fn truncate_bounds_long_bodies_on_char_boundaries() {
        let long = "é".repeat(400);
        let out = truncate(&long);
        assert!(out.len() <= 310, "len {}", out.len());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn from_env_names_the_missing_permission() {
        // Env-var-dependent: assert on the message shape without mutating the
        // process environment (which would race other tests).
        let err = OidcConfig::from_env(Some("t"), Some("c"));
        if std::env::var(ENV_REQUEST_URL).is_err() {
            let msg = err
                .expect_err("should fail without the runtime env")
                .to_string();
            assert!(msg.contains("id-token: write"), "{msg}");
        }
    }

    #[test]
    fn available_in_env_is_false_outside_actions() {
        if std::env::var(ENV_REQUEST_URL).is_err() {
            assert!(!OidcConfig::available_in_env());
        }
    }
}
