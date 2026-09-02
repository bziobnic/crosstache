//! Agent identity resolution.
//!
//! An ordered chain of resolvers, first match wins:
//!
//! 1. GitHub Actions OIDC   (`verified`)
//! 2. Entra workload identity (`verified`)
//! 3. AWS role   — declared but UNSUPPORTED (no STS client in this build)
//! 4. SPIFFE     — declared but UNSUPPORTED (no Workload API client)
//! 5. `XV_AGENT_ID` explicit environment assertion (`unverified`)
//!
//! GitHub resolution makes one authenticated request to the Actions runtime
//! OIDC endpoint and builds the identity from that token's claims. Entra
//! resolution exchanges the projected assertion through an explicit Azure
//! workload credential and validates the returned access-token identity. See
//! the honesty notes on each resolver, and `docs/agent-identity.md`, for
//! exactly what "verified" does and does not mean here.
//!
//! The result is cached for the lifetime of the process (see
//! [`current_resolution`]): resolution runs at most once.

#[cfg(test)]
use std::collections::HashMap;
use std::sync::OnceLock;

use azure_core::auth::TokenCredential;
use base64::Engine;
use serde::Deserialize;

use super::identity::{AgentIdentity, IdentitySource};

// GitHub Actions OIDC detection reuses the env-var protocol constants from the
// Azure OIDC federation path, so the two agree on what "an OIDC context" is.
use crate::backend::azure::oidc::{
    GithubOidcCredential, OidcConfig, AZURE_AUDIENCE, ENV_REQUEST_TOKEN, ENV_REQUEST_URL,
};

const GITHUB_ISSUER: &str = "https://token.actions.githubusercontent.com";
const MAX_FEDERATED_TOKEN_BYTES: usize = 1024 * 1024;

/// Read-only view of the process environment, so resolvers can be driven from a
/// synthetic environment in tests without mutating the real process env (which
/// would race other tests).
pub trait Env: Sync {
    /// Return the value of `key`, or `None` if unset or empty.
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }
}

/// A [`HashMap`]-backed environment for tests.
#[cfg(test)]
pub struct MapEnv(pub HashMap<String, String>);

#[cfg(test)]
impl MapEnv {
    /// Build from `(key, value)` pairs.
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

#[cfg(test)]
impl Env for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).filter(|v| !v.is_empty()).cloned()
    }
}

/// Why a single resolver did not produce an identity, for the fail-closed
/// diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverAttempt {
    /// The source this resolver would have produced.
    pub source: IdentitySource,
    /// A human-readable reason it did not, and what it needs — the text a user
    /// hitting fail-closed reads to fix their environment.
    pub reason: String,
}

/// The outcome of running the resolver chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// An identity was resolved.
    Resolved(Box<AgentIdentity>),
    /// No resolver produced an identity. Carries each resolver's reason so the
    /// fail-closed path can tell the user exactly what to set.
    Unresolved(Vec<ResolverAttempt>),
}

/// What one resolver produced.
enum ResolverResult {
    /// An identity (context fields not yet attached).
    Resolved(AgentIdentity),
    /// This resolver's context is not present. `reason` says what it needs.
    NotPresent(String),
    /// This source is declared but unsupported in this build. `reason` says why.
    Unsupported(String),
}

type ResolverFn = fn(&dyn Env) -> ResolverResult;

/// Run the ordered resolver chain against `env`, attaching the shared context
/// env vars to whatever resolves. This synthetic-environment entry point does
/// not fetch a GitHub token; production resolution does so in
/// [`current_resolution`].
pub fn resolve_with_env(env: &dyn Env) -> Resolution {
    resolve_with_env_and_tokens(env, None, None)
}

fn resolve_with_env_and_tokens(
    env: &dyn Env,
    github_token: Option<Result<&str, String>>,
    entra_access_token: Option<Result<&str, String>>,
) -> Resolution {
    let mut attempts = Vec::new();
    match resolve_github_oidc(env, github_token) {
        ResolverResult::Resolved(mut identity) => {
            return finish_resolution(&mut identity, env, &mut attempts);
        }
        ResolverResult::NotPresent(reason) | ResolverResult::Unsupported(reason) => {
            attempts.push(ResolverAttempt {
                source: IdentitySource::GithubOidc,
                reason,
            });
            if env.get(ENV_REQUEST_URL).is_some() && env.get(ENV_REQUEST_TOKEN).is_some() {
                return Resolution::Unresolved(attempts);
            }
        }
    }

    match resolve_entra(env, entra_access_token) {
        ResolverResult::Resolved(mut identity) => {
            return finish_resolution(&mut identity, env, &mut attempts);
        }
        ResolverResult::NotPresent(reason) | ResolverResult::Unsupported(reason) => {
            attempts.push(ResolverAttempt {
                source: IdentitySource::EntraWorkloadIdentity,
                reason,
            });
            if entra_context_present(env) {
                return Resolution::Unresolved(attempts);
            }
        }
    }

    let resolvers: [(IdentitySource, ResolverFn); 3] = [
        (IdentitySource::AwsRole, resolve_aws_unsupported),
        (IdentitySource::Spiffe, resolve_spiffe_unsupported),
        (IdentitySource::EnvAssertion, resolve_env_assertion),
    ];

    for (source, resolver) in resolvers {
        match resolver(env) {
            ResolverResult::Resolved(mut identity) => {
                return finish_resolution(&mut identity, env, &mut attempts);
            }
            ResolverResult::NotPresent(reason) | ResolverResult::Unsupported(reason) => {
                attempts.push(ResolverAttempt { source, reason });
            }
        }
    }
    Resolution::Unresolved(attempts)
}

fn finish_resolution(
    identity: &mut AgentIdentity,
    env: &dyn Env,
    attempts: &mut Vec<ResolverAttempt>,
) -> Resolution {
    attach_context(identity, env);
    match identity.validate() {
        Ok(()) => Resolution::Resolved(Box::new(identity.clone())),
        Err(error) => {
            attempts.push(ResolverAttempt {
                source: identity.source,
                reason: format!("identity or audit context was rejected: {error}"),
            });
            Resolution::Unresolved(attempts.clone())
        }
    }
}

/// Attach the shared correlation/context env vars to a resolved identity.
///
/// These do not decide *which* identity resolved; they annotate it. `purpose`
/// among them is audit-only — see [`AgentIdentity::purpose`].
fn attach_context(identity: &mut AgentIdentity, env: &dyn Env) {
    identity.session_id = env.get("XV_AGENT_SESSION");
    identity.invoking_principal = env.get("XV_AGENT_PRINCIPAL");
    identity.purpose = env.get("XV_AGENT_PURPOSE");
    identity.delegation_chain = env
        .get("XV_AGENT_DELEGATION")
        .map(|chain| {
            chain
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
}

/// GitHub Actions OIDC.
///
/// Detection reuses the Actions id-token request env vars
/// (`ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN`) that the runner injects when a job
/// declares `permissions: id-token: write` — the same signal the Azure OIDC
/// federation path keys on. The identity attributes come from the OIDC token's
/// `repository`, `job_workflow_ref` (or `workflow`), and `ref` claims.
///
/// Honesty: production fetches the token over TLS from GitHub's authenticated
/// Actions runtime endpoint, reusing `backend::azure::oidc`, but does not
/// cryptographically verify its JWT signature locally. "Verified" therefore
/// means the provider endpoint authenticated the request and returned the
/// identity token, not that xv performed independent signature verification.
fn resolve_github_oidc(env: &dyn Env, token: Option<Result<&str, String>>) -> ResolverResult {
    let has_oidc = env.get(ENV_REQUEST_URL).is_some() && env.get(ENV_REQUEST_TOKEN).is_some();
    if !has_oidc {
        return ResolverResult::NotPresent(format!(
            "GitHub Actions OIDC: needs {ENV_REQUEST_URL} and {ENV_REQUEST_TOKEN} (present only \
             when a job declares `permissions: id-token: write`)."
        ));
    }
    let token = match token {
        Some(Ok(token)) => token,
        Some(Err(error)) => {
            return ResolverResult::NotPresent(format!(
                "GitHub Actions OIDC: the runtime token request failed: {error}"
            ));
        }
        None => {
            return ResolverResult::NotPresent(
                "GitHub Actions OIDC: the runtime context was detected but no OIDC token was fetched."
                    .to_string(),
            );
        }
    };
    match github_identity_from_jwt(token) {
        Ok(id) => ResolverResult::Resolved(id),
        Err(error) => ResolverResult::NotPresent(format!(
            "GitHub Actions OIDC: the returned token could not identify the workflow: {error}"
        )),
    }
}

/// Decode identity claims from a token obtained from GitHub's authenticated
/// Actions runtime endpoint.
///
/// This deliberately does not verify the JWT signature locally. The trust
/// boundary is the same as `tenant_id_from_jwt`: the token is accepted only
/// when it was just returned over TLS by the authenticated provider endpoint;
/// this parser is not an authenticator for arbitrary caller-supplied JWTs.
fn github_identity_from_jwt(token: &str) -> Result<AgentIdentity, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("invalid JWT format (expected three dot-separated parts)".into());
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .map_err(|e| format!("invalid base64url payload: {e}"))?;
    #[derive(Deserialize)]
    struct Claims {
        iss: String,
        aud: Audience,
        exp: i64,
        repository: String,
        #[serde(default)]
        job_workflow_ref: Option<String>,
        #[serde(default)]
        workflow: Option<String>,
        #[serde(rename = "ref")]
        git_ref: String,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Audience {
        One(String),
        Many(Vec<String>),
    }
    let claims: Claims =
        serde_json::from_slice(&payload).map_err(|e| format!("invalid JSON claims: {e}"))?;
    if claims.iss != GITHUB_ISSUER {
        return Err(format!("unexpected issuer {:?}", claims.iss));
    }
    let audience_matches = match &claims.aud {
        Audience::One(value) => value == AZURE_AUDIENCE,
        Audience::Many(values) => values.iter().any(|value| value == AZURE_AUDIENCE),
    };
    if !audience_matches {
        return Err("token audience is not the requested Azure token-exchange audience".into());
    }
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err("token has expired".into());
    }
    if claims.repository.is_empty() || claims.git_ref.is_empty() {
        return Err("repository and ref claims must be non-empty".into());
    }
    let workflow = claims
        .job_workflow_ref
        .filter(|value| !value.is_empty())
        .or_else(|| claims.workflow.filter(|value| !value.is_empty()))
        .ok_or_else(|| "token has neither job_workflow_ref nor workflow claim".to_string())?;
    let prefix = format!("{}/", claims.repository);
    let workflow = workflow.strip_prefix(&prefix).unwrap_or(&workflow);
    let suffix = if workflow.contains('@') {
        workflow.to_string()
    } else {
        format!("{workflow}@{}", claims.git_ref)
    };
    Ok(AgentIdentity::new(
        IdentitySource::GithubOidc,
        format!("github:{}:{suffix}", claims.repository),
    ))
}

/// Entra (Azure AD) workload identity.
///
/// The environment only detects a candidate. Production performs an explicit
/// `WorkloadIdentityCredential` exchange against an allowlisted Azure
/// authority, then this resolver requires the returned access token's tenant,
/// client, issuer, and expiry claims to match. Synthetic tests must inject an
/// already-exchanged token; environment presence alone never resolves.
fn resolve_entra(env: &dyn Env, access_token: Option<Result<&str, String>>) -> ResolverResult {
    let client_id = env.get("AZURE_CLIENT_ID");
    let tenant_id = env.get("AZURE_TENANT_ID");
    let token_file = env.get("AZURE_FEDERATED_TOKEN_FILE");
    match (client_id, tenant_id, token_file) {
        (Some(client_id), Some(tenant_id), Some(_)) => {
            let token = match access_token {
                Some(Ok(token)) => token,
                Some(Err(error)) => {
                    return ResolverResult::NotPresent(format!(
                        "Entra workload identity: federated token exchange failed: {error}"
                    ));
                }
                None => {
                    return ResolverResult::NotPresent(
                        "Entra workload identity: the projected credential context was detected, but no successfully exchanged Azure access token was supplied. Environment presence alone is not verified identity."
                            .to_string(),
                    );
                }
            };
            match entra_identity_from_access_token(token, &tenant_id, &client_id) {
                Ok(identity) => ResolverResult::Resolved(identity),
                Err(error) => ResolverResult::NotPresent(format!(
                    "Entra workload identity: exchanged access token was rejected: {error}"
                )),
            }
        }
        _ => ResolverResult::NotPresent(
            "Entra workload identity: needs AZURE_TENANT_ID, AZURE_CLIENT_ID, and AZURE_FEDERATED_TOKEN_FILE (the \
             projected federated-credential token, e.g. from Azure workload identity)."
                .to_string(),
        ),
    }
}

fn entra_context_present(env: &dyn Env) -> bool {
    env.get("AZURE_CLIENT_ID").is_some()
        && env.get("AZURE_TENANT_ID").is_some()
        && env.get("AZURE_FEDERATED_TOKEN_FILE").is_some()
}

fn entra_identity_from_access_token(
    token: &str,
    expected_tenant: &str,
    expected_client: &str,
) -> Result<AgentIdentity, String> {
    #[derive(Deserialize)]
    struct Claims {
        tid: String,
        #[serde(default)]
        appid: Option<String>,
        #[serde(default)]
        azp: Option<String>,
        iss: String,
        exp: i64,
    }
    let claims: Claims = decode_jwt_claims(token)?;
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err("token has expired".into());
    }
    if claims.tid != expected_tenant {
        return Err("tenant claim does not match AZURE_TENANT_ID".into());
    }
    if claims.appid.as_deref().or(claims.azp.as_deref()) != Some(expected_client) {
        return Err("client claim does not match AZURE_CLIENT_ID".into());
    }
    validate_entra_issuer(&claims.iss, expected_tenant)?;
    Ok(AgentIdentity::new(
        IdentitySource::EntraWorkloadIdentity,
        format!("entra:{expected_tenant}:{expected_client}"),
    ))
}

fn decode_jwt_claims<T: for<'de> Deserialize<'de>>(token: &str) -> Result<T, String> {
    let mut parts = token.split('.');
    let _header = parts.next();
    let payload = parts.next().ok_or("invalid JWT format")?;
    if parts.next().is_none() || parts.next().is_some() {
        return Err("invalid JWT format (expected three dot-separated parts)".into());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .map_err(|error| format!("invalid base64url payload: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid JSON claims: {error}"))
}

fn validate_entra_issuer(issuer: &str, tenant: &str) -> Result<(), String> {
    let url = url::Url::parse(issuer).map_err(|error| format!("invalid issuer URL: {error}"))?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("issuer must be an HTTPS Azure authority without user information".into());
    }
    let trusted = matches!(
        url.host_str(),
        Some(
            "login.microsoftonline.com"
                | "login.microsoftonline.us"
                | "login.chinacloudapi.cn"
                | "login.microsoftonline.de"
                | "sts.windows.net"
        )
    );
    if !trusted {
        return Err("issuer host is not a trusted Azure authority".into());
    }
    if !url
        .path_segments()
        .is_some_and(|mut segments| segments.any(|segment| segment.eq_ignore_ascii_case(tenant)))
    {
        return Err("issuer path does not identify AZURE_TENANT_ID".into());
    }
    Ok(())
}

/// AWS role identity — declared but unsupported in this build.
///
/// Verifying an AWS role requires an STS `GetCallerIdentity` call, and there is
/// no STS client compiled in (adding `aws-sdk-sts` is out of scope). We refuse
/// to read `AWS_ROLE_ARN` and call it verified: an env var is an assertion, not
/// an authentication, and labelling it verified would be a lie in the security
/// model.
fn resolve_aws_unsupported(_env: &dyn Env) -> ResolverResult {
    ResolverResult::Unsupported(
        "AWS role: unsupported in this build. Verifying an AWS role needs an STS \
         GetCallerIdentity client, which is not compiled in. Reading AWS_ROLE_ARN would be an \
         unverified assertion, not an authentication, so it is deliberately not used. Use \
         XV_AGENT_ID for an explicit (unverified) assertion instead."
            .to_string(),
    )
}

/// SPIFFE identity — declared but unsupported in this build.
///
/// Obtaining a verified SPIFFE identity requires a Workload API client, which is
/// not compiled in. Declared as a slot so policy can name the source.
fn resolve_spiffe_unsupported(_env: &dyn Env) -> ResolverResult {
    ResolverResult::Unsupported(
        "SPIFFE: unsupported in this build. A verified SPIFFE identity needs a Workload API \
         client, which is not compiled in. Use XV_AGENT_ID for an explicit (unverified) \
         assertion instead."
            .to_string(),
    )
}

/// `XV_AGENT_ID` explicit environment assertion — unverified by construction.
fn resolve_env_assertion(env: &dyn Env) -> ResolverResult {
    match env.get("XV_AGENT_ID") {
        Some(id) => ResolverResult::Resolved(AgentIdentity::new(IdentitySource::EnvAssertion, id)),
        None => ResolverResult::NotPresent(
            "Environment assertion: set XV_AGENT_ID=<stable-agent-id> to assert an identity. \
             This is UNVERIFIED — any process can set it — so rules that disclose raw secret \
             material still refuse it unless allow_unverified_identities = true."
                .to_string(),
        ),
    }
}

/// Render an [`Resolution::Unresolved`] into an actionable, multi-line
/// fail-closed diagnostic naming every resolver tried and what each needs.
pub fn unresolved_diagnostic(attempts: &[ResolverAttempt]) -> String {
    let mut out = String::from(
        "Policy enforcement is configured, but no agent identity could be resolved, so xv is \
         failing closed. Resolvers tried, in order:\n",
    );
    for attempt in attempts {
        out.push_str(&format!("  - {}: {}\n", attempt.source, attempt.reason));
    }
    out.push_str(
        "Resolve one of these, or set XV_AGENT_ID for an explicit (unverified) assertion. To \
         run without enforcement, remove the [agent] block or set enforce = false.",
    );
    out
}

/// Process-wide cached resolution against the real environment.
///
/// Runs the chain at most once per process; later calls return the same cached
/// value. Tests should use [`resolve_with_env`] with a [`MapEnv`] instead of
/// this, since this reads (and pins) the real process environment.
pub fn current_resolution() -> &'static Resolution {
    static CACHE: OnceLock<Resolution> = OnceLock::new();
    CACHE.get_or_init(|| {
        if OidcConfig::available_in_env() {
            let config = OidcConfig {
                tenant_id: String::new(),
                client_id: String::new(),
                request_url: std::env::var(ENV_REQUEST_URL).unwrap_or_default(),
                request_token: std::env::var(ENV_REQUEST_TOKEN).unwrap_or_default(),
            };
            let fetched = run_async(fetch_github_token_async(&config));
            return resolve_with_env_and_tokens(
                &SystemEnv,
                Some(fetched.as_deref().map_err(Clone::clone)),
                None,
            );
        }
        if entra_context_present(&SystemEnv) {
            let fetched = run_async(fetch_entra_access_token(&SystemEnv));
            return resolve_with_env_and_tokens(
                &SystemEnv,
                None,
                Some(fetched.as_deref().map_err(Clone::clone)),
            );
        }
        resolve_with_env(&SystemEnv)
    })
}

async fn fetch_github_token_async(config: &OidcConfig) -> Result<String, String> {
    GithubOidcCredential::request_identity_token(config)
        .await
        .map_err(|error| error.to_string())
}

async fn fetch_entra_access_token(env: &dyn Env) -> Result<String, String> {
    let tenant_id = env
        .get("AZURE_TENANT_ID")
        .ok_or("AZURE_TENANT_ID is missing")?;
    let client_id = env
        .get("AZURE_CLIENT_ID")
        .ok_or("AZURE_CLIENT_ID is missing")?;
    let token_path = env
        .get("AZURE_FEDERATED_TOKEN_FILE")
        .ok_or("AZURE_FEDERATED_TOKEN_FILE is missing")?;
    let metadata = std::fs::metadata(&token_path)
        .map_err(|error| format!("cannot inspect federated token file: {error}"))?;
    if metadata.len() > MAX_FEDERATED_TOKEN_BYTES as u64 {
        return Err(format!(
            "federated token file exceeds {MAX_FEDERATED_TOKEN_BYTES} bytes"
        ));
    }
    let assertion = std::fs::read_to_string(&token_path)
        .map_err(|error| format!("cannot read federated token file: {error}"))?;
    let authority = trusted_authority(env)?;
    let scope = entra_scope(authority.host_str().unwrap_or_default());
    let credential = azure_identity::WorkloadIdentityCredential::new(
        azure_core::new_http_client(),
        authority,
        tenant_id,
        client_id,
        assertion,
    );
    let token = credential
        .get_token(&[scope])
        .await
        .map_err(|error| format!("Azure workload-identity exchange failed: {error}"))?;
    if token.expires_on <= time::OffsetDateTime::now_utc() {
        return Err("Azure workload-identity exchange returned an expired token".into());
    }
    Ok(token.token.secret().to_string())
}

fn trusted_authority(env: &dyn Env) -> Result<url::Url, String> {
    let raw = env
        .get("AZURE_AUTHORITY_HOST")
        .unwrap_or_else(|| "https://login.microsoftonline.com".to_string());
    let url =
        url::Url::parse(&raw).map_err(|error| format!("invalid Azure authority URL: {error}"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(
            url.host_str(),
            Some(
                "login.microsoftonline.com"
                    | "login.microsoftonline.us"
                    | "login.chinacloudapi.cn"
                    | "login.microsoftonline.de"
            )
        )
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("AZURE_AUTHORITY_HOST must be a documented HTTPS Azure authority host without user information, a custom port, path, query, or fragment".into());
    }
    Ok(url)
}

fn entra_scope(authority_host: &str) -> &'static str {
    match authority_host {
        "login.microsoftonline.us" => "https://management.usgovcloudapi.net/.default",
        "login.chinacloudapi.cn" => "https://management.chinacloudapi.cn/.default",
        "login.microsoftonline.de" => "https://management.microsoftazure.de/.default",
        _ => "https://management.azure.com/.default",
    }
}

fn run_async<F>(future: F) -> Result<String, String>
where
    F: std::future::Future<Output = Result<String, String>> + Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Ok(_) => std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Runtime::new()
                        .map_err(|e| format!("could not create runtime for OIDC request: {e}"))?
                        .block_on(future)
                })
                .join()
                .unwrap_or_else(|_| Err("OIDC request worker panicked".into()))
        }),
        Err(_) => tokio::runtime::Runtime::new()
            .map_err(|e| format!("could not create runtime for OIDC request: {e}"))?
            .block_on(future),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_jwt(
        repository: &str,
        workflow_ref: Option<&str>,
        workflow: Option<&str>,
        git_ref: &str,
    ) -> String {
        let claims = serde_json::json!({
            "iss": GITHUB_ISSUER,
            "aud": AZURE_AUDIENCE,
            "exp": chrono::Utc::now().timestamp() + 600,
            "repository": repository,
            "job_workflow_ref": workflow_ref,
            "workflow": workflow,
            "ref": git_ref,
        });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        format!("header.{payload}.signature")
    }

    #[test]
    fn github_oidc_resolves_verified_with_expected_id() {
        let env = MapEnv::from_pairs([
            (ENV_REQUEST_URL, "https://pipelines.example/token?x=1"),
            (ENV_REQUEST_TOKEN, "runtime-bearer"),
            ("GITHUB_REPOSITORY", "bziobnic/crosstache"),
            (
                "GITHUB_WORKFLOW_REF",
                "bziobnic/crosstache/.github/workflows/ci.yml@refs/heads/main",
            ),
        ]);
        let token = github_jwt(
            "bziobnic/crosstache",
            Some("bziobnic/crosstache/.github/workflows/ci.yml@refs/heads/main"),
            None,
            "refs/heads/main",
        );
        match resolve_with_env_and_tokens(&env, Some(Ok(&token)), None) {
            Resolution::Resolved(id) => {
                assert_eq!(
                    id.id,
                    "github:bziobnic/crosstache:.github/workflows/ci.yml@refs/heads/main"
                );
                assert_eq!(id.source, IdentitySource::GithubOidc);
                assert!(id.verified);
            }
            other => panic!("expected github resolution, got {other:?}"),
        }
    }

    #[test]
    fn github_oidc_falls_back_to_workflow_and_ref() {
        let env = MapEnv::from_pairs([
            (ENV_REQUEST_URL, "https://x/token"),
            (ENV_REQUEST_TOKEN, "t"),
            ("GITHUB_REPOSITORY", "o/r"),
            ("GITHUB_WORKFLOW", "CI"),
            ("GITHUB_REF", "refs/heads/main"),
        ]);
        let token = github_jwt("o/r", None, Some("CI"), "refs/heads/main");
        match resolve_with_env_and_tokens(&env, Some(Ok(&token)), None) {
            Resolution::Resolved(id) => assert_eq!(id.id, "github:o/r:CI@refs/heads/main"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn entra_resolves_verified_only_from_an_exchanged_matching_token() {
        let env = MapEnv::from_pairs([
            ("AZURE_CLIENT_ID", "client-123"),
            ("AZURE_TENANT_ID", "tenant-abc"),
            ("AZURE_FEDERATED_TOKEN_FILE", "/var/run/token"),
        ]);
        let token = test_jwt(serde_json::json!({
            "tid": "tenant-abc",
            "appid": "client-123",
            "iss": "https://login.microsoftonline.com/tenant-abc/v2.0",
            "exp": chrono::Utc::now().timestamp() + 600,
        }));
        match resolve_with_env_and_tokens(&env, None, Some(Ok(&token))) {
            Resolution::Resolved(id) => {
                assert_eq!(id.id, "entra:tenant-abc:client-123");
                assert_eq!(id.source, IdentitySource::EntraWorkloadIdentity);
                assert!(id.verified);
            }
            other => panic!("{other:?}"),
        }
    }

    fn test_jwt(claims: serde_json::Value) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        format!("header.{payload}.signature")
    }

    #[test]
    fn entra_environment_presence_alone_is_never_verified() {
        let env = MapEnv::from_pairs([
            ("AZURE_CLIENT_ID", "client-123"),
            ("AZURE_TENANT_ID", "tenant-abc"),
            ("AZURE_FEDERATED_TOKEN_FILE", "/var/run/token"),
            ("XV_AGENT_ID", "weaker-fallback"),
        ]);
        assert!(matches!(resolve_with_env(&env), Resolution::Unresolved(_)));
    }

    #[test]
    fn entra_rejects_mismatched_claims_and_untrusted_issuers() {
        let env = MapEnv::from_pairs([
            ("AZURE_CLIENT_ID", "client-123"),
            ("AZURE_TENANT_ID", "tenant-abc"),
            ("AZURE_FEDERATED_TOKEN_FILE", "/var/run/token"),
        ]);
        for claims in [
            serde_json::json!({
                "tid": "other-tenant", "appid": "client-123",
                "iss": "https://login.microsoftonline.com/other-tenant/v2.0",
                "exp": chrono::Utc::now().timestamp() + 600,
            }),
            serde_json::json!({
                "tid": "tenant-abc", "appid": "other-client",
                "iss": "https://login.microsoftonline.com/tenant-abc/v2.0",
                "exp": chrono::Utc::now().timestamp() + 600,
            }),
            serde_json::json!({
                "tid": "tenant-abc", "appid": "client-123",
                "iss": "https://attacker.example/tenant-abc/v2.0",
                "exp": chrono::Utc::now().timestamp() + 600,
            }),
        ] {
            let token = test_jwt(claims);
            assert!(matches!(
                resolve_with_env_and_tokens(&env, None, Some(Ok(&token))),
                Resolution::Unresolved(_)
            ));
        }
    }

    #[test]
    fn entra_without_token_file_does_not_resolve() {
        // A bare AZURE_CLIENT_ID is not a workload-identity context.
        let env = MapEnv::from_pairs([("AZURE_CLIENT_ID", "client-123")]);
        assert!(matches!(resolve_with_env(&env), Resolution::Unresolved(_)));
    }

    #[test]
    fn env_assertion_resolves_unverified_only() {
        let env = MapEnv::from_pairs([("XV_AGENT_ID", "team/deployer")]);
        match resolve_with_env(&env) {
            Resolution::Resolved(id) => {
                assert_eq!(id.id, "team/deployer");
                assert_eq!(id.source, IdentitySource::EnvAssertion);
                assert!(!id.verified, "env assertion must be unverified");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_env_assertion_is_unverified() {
        // Drive every *resolvable* source and confirm the verified flag.
        let github = MapEnv::from_pairs([
            (ENV_REQUEST_URL, "u"),
            (ENV_REQUEST_TOKEN, "t"),
            ("GITHUB_REPOSITORY", "o/r"),
        ]);
        let entra = MapEnv::from_pairs([
            ("AZURE_CLIENT_ID", "c"),
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_FEDERATED_TOKEN_FILE", "/f"),
        ]);
        let asserted = MapEnv::from_pairs([("XV_AGENT_ID", "x")]);
        match resolve_with_env(&asserted) {
            Resolution::Resolved(id) => assert!(!id.verified),
            other => panic!("{other:?}"),
        }
        let entra_token = test_jwt(serde_json::json!({
            "tid": "t", "appid": "c",
            "iss": "https://login.microsoftonline.com/t/v2.0",
            "exp": chrono::Utc::now().timestamp() + 600,
        }));
        match resolve_with_env_and_tokens(&entra, None, Some(Ok(&entra_token))) {
            Resolution::Resolved(id) => assert!(id.verified),
            other => panic!("{other:?}"),
        }
        let token = github_jwt("o/r", None, Some("CI"), "refs/heads/main");
        match resolve_with_env_and_tokens(&github, Some(Ok(&token)), None) {
            Resolution::Resolved(id) => assert!(id.verified),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn aws_and_spiffe_report_unsupported_and_never_resolve() {
        // Even with the env vars a naive resolver might read, the unsupported
        // slots must not resolve — and must not be labelled verified.
        let env = MapEnv::from_pairs([
            ("AWS_ROLE_ARN", "arn:aws:iam::123:role/deployer"),
            ("SPIFFE_ENDPOINT_SOCKET", "unix:///tmp/spire.sock"),
        ]);
        match resolve_with_env(&env) {
            Resolution::Unresolved(attempts) => {
                let aws = attempts
                    .iter()
                    .find(|a| a.source == IdentitySource::AwsRole)
                    .expect("aws attempt present");
                assert!(aws.reason.contains("unsupported"), "{}", aws.reason);
                let spiffe = attempts
                    .iter()
                    .find(|a| a.source == IdentitySource::Spiffe)
                    .expect("spiffe attempt present");
                assert!(spiffe.reason.contains("unsupported"), "{}", spiffe.reason);
            }
            other => panic!("AWS_ROLE_ARN must NOT resolve to a verified identity: {other:?}"),
        }
    }

    #[test]
    fn context_vars_attach_to_resolved_identity() {
        let env = MapEnv::from_pairs([
            ("XV_AGENT_ID", "asserted"),
            ("XV_AGENT_SESSION", "sess-1"),
            ("XV_AGENT_PRINCIPAL", "alice@example.com"),
            ("XV_AGENT_PURPOSE", "deploy step"),
            ("XV_AGENT_DELEGATION", "human, orchestrator, child"),
        ]);
        match resolve_with_env(&env) {
            Resolution::Resolved(id) => {
                assert_eq!(id.session_id.as_deref(), Some("sess-1"));
                assert_eq!(id.invoking_principal.as_deref(), Some("alice@example.com"));
                assert_eq!(id.purpose.as_deref(), Some("deploy step"));
                assert_eq!(
                    id.delegation_chain,
                    vec![
                        "human".to_string(),
                        "orchestrator".to_string(),
                        "child".to_string()
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_environment_is_unresolved_with_all_reasons() {
        let env = MapEnv::from_pairs([]);
        match resolve_with_env(&env) {
            Resolution::Unresolved(attempts) => {
                assert_eq!(attempts.len(), 5, "every resolver reports a reason");
                let diag = unresolved_diagnostic(&attempts);
                assert!(diag.contains("github-oidc"), "{diag}");
                assert!(diag.contains(ENV_REQUEST_URL), "{diag}");
                assert!(diag.contains(ENV_REQUEST_TOKEN), "{diag}");
                assert!(diag.contains("AZURE_CLIENT_ID"), "{diag}");
                assert!(diag.contains("AZURE_FEDERATED_TOKEN_FILE"), "{diag}");
                assert!(diag.contains("GetCallerIdentity"), "{diag}");
                assert!(diag.contains("Workload API"), "{diag}");
                assert!(diag.contains("XV_AGENT_ID"), "{diag}");
                assert!(diag.contains("failing closed"), "{diag}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_github_token_is_an_actionable_failed_attempt() {
        let env = MapEnv::from_pairs([(ENV_REQUEST_URL, "u"), (ENV_REQUEST_TOKEN, "t")]);
        match resolve_with_env_and_tokens(&env, Some(Ok("not-a-jwt")), None) {
            Resolution::Unresolved(attempts) => {
                let github = attempts
                    .iter()
                    .find(|attempt| attempt.source == IdentitySource::GithubOidc)
                    .unwrap();
                assert!(
                    github.reason.contains("invalid JWT format"),
                    "{}",
                    github.reason
                );
            }
            other => panic!("malformed GitHub token must not resolve: {other:?}"),
        }
    }

    #[test]
    fn github_claims_require_trusted_issuer_audience_and_future_expiry() {
        let base = serde_json::json!({
            "iss": GITHUB_ISSUER,
            "aud": AZURE_AUDIENCE,
            "exp": chrono::Utc::now().timestamp() + 600,
            "repository": "o/r",
            "workflow": "CI",
            "ref": "refs/heads/main",
        });
        assert!(github_identity_from_jwt(&test_jwt(base.clone())).is_ok());
        for (field, value) in [
            ("iss", serde_json::json!("https://attacker.example")),
            ("aud", serde_json::json!("other-audience")),
            ("exp", serde_json::json!(chrono::Utc::now().timestamp() - 1)),
        ] {
            let mut claims = base.clone();
            claims[field] = value;
            assert!(github_identity_from_jwt(&test_jwt(claims)).is_err());
        }
    }

    #[test]
    fn custom_entra_authority_hosts_are_rejected() {
        for authority in [
            "http://login.microsoftonline.com",
            "https://attacker.example",
            "https://user@login.microsoftonline.com",
            "https://login.microsoftonline.com/tenant",
        ] {
            let env = MapEnv::from_pairs([("AZURE_AUTHORITY_HOST", authority)]);
            assert!(trusted_authority(&env).is_err(), "accepted {authority}");
        }
        let sovereign =
            MapEnv::from_pairs([("AZURE_AUTHORITY_HOST", "https://login.microsoftonline.us")]);
        assert!(trusted_authority(&sovereign).is_ok());
    }

    #[test]
    fn current_resolution_is_cached() {
        // Same &'static on repeated calls: resolution runs at most once.
        let a = current_resolution();
        let b = current_resolution();
        assert!(std::ptr::eq(a, b));
    }
}
