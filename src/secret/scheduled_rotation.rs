//! Structured due-rotation service.
//!
//! `xv rotate --due` and the scheduled runner (`xv schedule run`) rotate the
//! same set of secrets, but they need very different things back. The CLI
//! renders a human transcript and exits non-zero on a partial batch; the
//! scheduled runner has to persist a machine-readable outcome (`last-run.json`)
//! *without* capturing stdout or parsing error strings.
//!
//! So the discovery/evaluate/rotate loop lives here, returning a
//! [`DueRotationSummary`] of aggregate counts plus typed failure categories,
//! and everything user-facing is delegated to a [`DueRotationObserver`]:
//!
//! - the CLI adapter (`crate::cli::secret_ops::execute_rotate_due`) implements
//!   the observer to print exactly the messages it printed before, and does the
//!   ambient target resolution and the batch confirmation itself;
//! - the scheduled runner passes an already-validated `(backend_name, vault)`
//!   pair straight through — it never touches workspace/vault resolution — and
//!   uses [`SilentObserver`].
//!
//! ## What the summary may contain
//!
//! Aggregate counts and closed-set failure categories only. **No secret name
//! and no error body ever enters a [`DueRotationSummary`]**, because the
//! scheduler serializes it to a state file that is not the vault. Per-secret
//! detail is reported to the observer, which is free to render it for a person
//! at the terminal, and is dropped otherwise.
//!
//! ## Failure model
//!
//! - A **total discovery failure** (unknown backend name, unreadable store,
//!   list refused) returns a [`DueRotationError`] — typed and name-free for the
//!   same reason the summary is, since it is what a scheduled run records when
//!   there is no summary at all. There is no partial answer to report: the
//!   caller cannot tell "no secrets" apart from "could not look", and a
//!   scheduled run must not record a green outcome for a vault it never read.
//! - A **per-secret failure** (including an unparseable policy, when the
//!   observer chooses to proceed) is counted in
//!   [`DueRotationSummary::failed`] and categorized in
//!   [`DueRotationSummary::failures`]. The run continues; other due secrets
//!   still rotate.

use std::sync::Arc;

use crate::backend::{registry::BackendRegistry, Backend};
use crate::cli::commands::CharsetType;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::rotation::{evaluate, RotationStatus};

/// Generation parameters for the values written by a due-rotation run.
///
/// Mirrors `xv rotate`'s `--length` / `--charset` / `--generator` flags; the
/// defaults are clap's, so a caller with no user input (the scheduled runner)
/// rotates exactly like a bare `xv rotate --due`.
#[derive(Debug, Clone)]
pub struct DueRotationOptions {
    /// Length of each generated value.
    pub length: usize,
    /// Character set used when no custom generator is configured.
    pub charset: CharsetType,
    /// Optional custom generator script, overriding `length`/`charset`.
    pub generator: Option<String>,
    /// Whether each rotation may bump the ambient context's usage counters.
    ///
    /// Context usage tracking is **interactive only**. The scheduled runner
    /// pins a digest of the context file it recorded at install time, and any
    /// byte change makes the next firing refuse with `context_digest changed`
    /// — so a scheduled sweep must neither read nor write that file. The
    /// default is therefore `false`, which is what `DueRotationOptions::
    /// default()` (the scheduled runner's constructor) gets; the terminal
    /// adapter for `xv rotate --due` opts in explicitly.
    pub track_context_usage: bool,
}

impl Default for DueRotationOptions {
    fn default() -> Self {
        Self {
            // Keep in sync with the `--length` default in `cli::commands`.
            length: 32,
            charset: CharsetType::default(),
            generator: None,
            // Fail-safe for the scheduled runner: never touch the pinned
            // ambient context.
            track_context_usage: false,
        }
    }
}

/// Closed set of reasons one secret did not rotate.
///
/// Derived from the [`CrosstacheError`] variant that ended the attempt, never
/// from message text — the same rule the local audit log follows for its status
/// tokens. Adding a variant here is a deliberate act; classification must not
/// drift when an error message is reworded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DueRotationFailureCategory {
    /// The secret carries an unparseable `xv:rotate_every` tag, so it could
    /// not be evaluated at all.
    InvalidPolicy,
    /// The secret vanished between listing and rotation.
    NotFound,
    /// The backend refused the read or write on authorization grounds.
    PermissionDenied,
    /// The backend could not be reached, or failed the operation itself.
    Backend,
    /// Anything else that ended the rotation of this one secret.
    Rotate,
}

impl DueRotationFailureCategory {
    /// Stable, kebab-case code. Part of what the scheduler persists.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidPolicy => "invalid-policy",
            Self::NotFound => "secret-not-found",
            Self::PermissionDenied => "permission-denied",
            Self::Backend => "backend-error",
            Self::Rotate => "rotate-failed",
        }
    }

    /// A fixed, sanitized description. Constant per category by construction,
    /// so it can never carry a secret name, a vault name, or an error body.
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidPolicy => "the rotation interval could not be parsed",
            Self::NotFound => "the secret no longer exists",
            Self::PermissionDenied => "the backend denied the operation",
            Self::Backend => "the backend failed the operation",
            Self::Rotate => "the rotation did not complete",
        }
    }

    /// Classify a rotation error by its variant.
    fn classify(error: &CrosstacheError) -> Self {
        match error {
            CrosstacheError::SecretNotFound { .. } | CrosstacheError::VaultNotFound { .. } => {
                Self::NotFound
            }
            CrosstacheError::PermissionDenied(_) | CrosstacheError::AuthenticationError(_) => {
                Self::PermissionDenied
            }
            CrosstacheError::BackendUnavailable { .. }
            | CrosstacheError::AzureApiError(_)
            | CrosstacheError::NetworkError(_)
            | CrosstacheError::DnsResolutionError { .. }
            | CrosstacheError::ConnectionTimeout(_)
            | CrosstacheError::ConnectionRefused(_)
            | CrosstacheError::SslError(_)
            | CrosstacheError::RateLimited(_)
            | CrosstacheError::Conflict(_) => Self::Backend,
            _ => Self::Rotate,
        }
    }
}

/// One counted failure. Carries a category and nothing else — deliberately not
/// the secret's name, the vault, or the error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DueRotationFailure {
    /// Why this secret did not rotate.
    pub category: DueRotationFailureCategory,
}

// The scheduled runner writes both into `last-run.json`'s diagnostics.
impl DueRotationFailure {
    /// Stable code for this failure's category.
    pub fn code(&self) -> &'static str {
        self.category.code()
    }

    /// Sanitized, constant message for this failure's category.
    pub fn message(&self) -> &'static str {
        self.category.message()
    }
}

/// Closed set of reasons a whole run never produced a summary.
///
/// The per-secret analogue is [`DueRotationFailureCategory`]; this is the
/// analogue for the failures that end the run before (or instead of) any
/// rotation. Same rule: derived from the [`CrosstacheError`] variant, never
/// from message text, and every rendering is a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DueRotationErrorKind {
    /// The backend could not be materialized (not attached, not configured)
    /// or could not be reached at all.
    BackendUnavailable,
    /// The target vault does not exist.
    VaultNotFound,
    /// The backend refused the listing on authorization grounds.
    PermissionDenied,
    /// Anything else that stopped discovery.
    Discovery,
    /// The observer refused the run at the plan stage — nothing was written.
    Refused,
}

impl DueRotationErrorKind {
    /// Stable, kebab-case code. Part of what the scheduler persists.
    pub fn code(self) -> &'static str {
        match self {
            Self::BackendUnavailable => "backend-unavailable",
            Self::VaultNotFound => "vault-not-found",
            Self::PermissionDenied => "permission-denied",
            Self::Discovery => "discovery-failed",
            Self::Refused => "run-refused",
        }
    }

    /// A fixed, sanitized description. Constant per kind by construction, so it
    /// can never carry a vault name or a backend error body.
    pub fn message(self) -> &'static str {
        match self {
            Self::BackendUnavailable => "the backend could not be resolved or reached",
            Self::VaultNotFound => "the target vault does not exist",
            Self::PermissionDenied => "the backend denied the listing",
            Self::Discovery => "the vault could not be listed",
            Self::Refused => "the run was refused before anything was rotated",
        }
    }

    /// Classify a discovery error by its variant.
    fn classify(error: &CrosstacheError) -> Self {
        match error {
            CrosstacheError::VaultNotFound { .. } => Self::VaultNotFound,
            CrosstacheError::PermissionDenied(_) | CrosstacheError::AuthenticationError(_) => {
                Self::PermissionDenied
            }
            CrosstacheError::BackendUnavailable { .. }
            | CrosstacheError::AzureApiError(_)
            | CrosstacheError::NetworkError(_)
            | CrosstacheError::DnsResolutionError { .. }
            | CrosstacheError::ConnectionTimeout(_)
            | CrosstacheError::ConnectionRefused(_)
            | CrosstacheError::SslError(_)
            | CrosstacheError::RateLimited(_) => Self::BackendUnavailable,
            _ => Self::Discovery,
        }
    }
}

/// A whole run that never happened.
///
/// This is the one value a scheduled runner is most likely to serialize
/// verbatim, so it is name-free by construction: [`code`](Self::code) and
/// [`message`](Self::message) are constants, and the `Debug` and `Display`
/// impls are written by hand so the wrapped [`CrosstacheError`] — which may
/// name the vault and quote a backend error body — cannot reach a state file
/// through a stray `{:?}`.
///
/// The real error is still carried, for terminal callers: `?` in a function
/// returning [`crate::error::Result`] unwraps it back to the original
/// `CrosstacheError` through [`From`], which is how `xv rotate --due` keeps its
/// pre-extraction output byte for byte.
pub struct DueRotationError {
    /// Why the run never happened.
    pub kind: DueRotationErrorKind,
    /// The underlying error. Private on purpose: reaching it is a deliberate
    /// act (`into_source`, or `?` into a `CrosstacheError`), never something a
    /// `{:?}` on this struct does by accident.
    source: CrosstacheError,
}

impl DueRotationError {
    fn new(kind: DueRotationErrorKind, source: CrosstacheError) -> Self {
        Self { kind, source }
    }

    /// Wrap a discovery error, classifying it by variant.
    fn discovery(source: CrosstacheError) -> Self {
        Self::new(DueRotationErrorKind::classify(&source), source)
    }

    /// Stable code for this run's failure kind.
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// Sanitized, constant message for this run's failure kind.
    pub fn message(&self) -> &'static str {
        self.kind.message()
    }

    /// The underlying error, for a caller that renders for a person.
    // The scheduled runner uses `code`/`message`; the CLI adapter goes through
    // `From`. This is the explicit escape hatch for anything else.
    #[allow(dead_code)]
    pub fn into_source(self) -> CrosstacheError {
        self.source
    }
}

// Hand-written: the derived impl would print `source`, which is exactly the
// text that must not reach `last-run.json`.
impl std::fmt::Debug for DueRotationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DueRotationError")
            .field("kind", &self.kind)
            .field("code", &self.code())
            .field("message", &self.message())
            .finish_non_exhaustive()
    }
}

// Also constant. `std::error::Error::source` is deliberately NOT implemented:
// a chain-printing formatter would otherwise reintroduce the error body.
impl std::fmt::Display for DueRotationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DueRotationError {}

impl From<DueRotationError> for CrosstacheError {
    fn from(error: DueRotationError) -> Self {
        error.source
    }
}

/// Result of a due-rotation run: the summary, or a typed whole-run failure.
pub type DueRotationResult = std::result::Result<DueRotationSummary, DueRotationError>;

/// Aggregate result of a due-rotation run. Safe to serialize into scheduler
/// state: counts and categories only.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DueRotationSummary {
    /// Secrets in the vault carrying a rotation policy (valid or not).
    pub policy_managed: usize,
    /// Policy-managed secrets whose policy had come due.
    pub due: usize,
    /// Due secrets that rotated successfully.
    pub rotated: usize,
    /// Secrets that failed (including unparseable policies).
    pub failed: usize,
    /// One entry per failure, in the order they occurred.
    pub failures: Vec<DueRotationFailure>,
}

/// What the service found, handed to the observer once before anything is
/// written. Secret names appear here — this goes to the *observer*, not into
/// the summary — so a terminal adapter can list them for a person.
#[derive(Debug, Clone)]
pub struct DueRotationPlan {
    /// The vault the run targets.
    // Read by an observer that renders for a person; the CLI adapter already
    // knows its own vault name and the scheduled runner uses `SilentObserver`.
    #[allow(dead_code)]
    pub vault: String,
    /// Every policy-managed secret, valid or not.
    pub policy_managed: usize,
    /// Secrets whose `xv:rotate_every` tag could not be parsed, sorted.
    pub invalid: Vec<String>,
    /// Secrets that are due, sorted.
    pub due: Vec<String>,
}

/// The observer's verdict on a [`DueRotationPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecision {
    /// Rotate the due secrets.
    Proceed,
    /// Stop without writing anything (nothing due, or the person declined).
    Abort,
}

/// Per-secret reporting seam. Every method has a no-op default, so a caller
/// that only wants the summary implements nothing.
pub trait DueRotationObserver {
    /// Called once after discovery and before any write.
    ///
    /// Returning [`PlanDecision::Abort`] ends the run with a summary whose
    /// `rotated`/`failed` are zero; returning `Err` propagates unchanged (the
    /// CLI uses this to refuse a vault containing unparseable policies, and to
    /// surface a failed confirmation prompt).
    fn on_plan(&mut self, plan: &DueRotationPlan) -> Result<PlanDecision> {
        let _ = plan;
        Ok(PlanDecision::Proceed)
    }

    /// One secret rotated successfully.
    fn on_rotated(&mut self, name: &str) {
        let _ = name;
    }

    /// One secret failed. `error` is the real error, for a terminal adapter to
    /// render; it never reaches the summary.
    fn on_failure(
        &mut self,
        name: &str,
        category: DueRotationFailureCategory,
        error: &CrosstacheError,
    ) {
        let (_, _, _) = (name, category, error);
    }
}

/// Observer that reports nothing — the scheduled runner's choice, since its
/// record of the run is the returned [`DueRotationSummary`].
#[derive(Debug, Default, Clone, Copy)]
// Constructed by the scheduled runner (and by this module's tests); the CLI
// adapter has its own observer.
pub struct SilentObserver;

impl DueRotationObserver for SilentObserver {}

/// Evaluate every secret in `vault` against its rotation policy.
///
/// Returns `(name, status)` pairs sorted by name, with unmanaged secrets
/// filtered out — an unmanaged secret is not "ok", it is out of scope. Shared
/// with `xv rotate --check`, which is why it takes an already-resolved backend.
pub(crate) async fn evaluate_vault_policies(
    backend: &dyn Backend,
    vault: &str,
) -> Result<Vec<(String, RotationStatus)>> {
    let secrets = backend
        .secrets()
        .list_secrets(vault, None)
        .await
        .map_err(CrosstacheError::from)?;

    let now = chrono::Utc::now();
    let mut rows: Vec<(String, RotationStatus)> = secrets
        .into_iter()
        .map(|s| {
            // `updated_on` on a summary is a display string (and empty on some
            // backends), so it is not usable as a machine baseline. A policy
            // with no `xv:rotated_at` therefore evaluates as due-once, which
            // then stamps a real timestamp. `xv update --rotate-every` stamps
            // one at policy-set time so this is not the normal path.
            let status = evaluate(&s.tags, None, now);
            (s.name, status)
        })
        .filter(|(_, status)| !matches!(status, RotationStatus::NoPolicy))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}

/// Rotate every due secret in `(backend_name, vault)`.
///
/// The scheduled runner's entry point: `backend_name` and `vault` are taken
/// literally — this performs **no** workspace, context, or vault-name
/// resolution — and the backend is materialized from `registry` by name.
///
/// A failure to materialize the backend or to list the vault is a total
/// discovery failure and returns `Err`.
// The scheduled runner's entry point; the CLI adapter calls
// `run_due_rotation_with_backend` with its already-resolved backend.
pub async fn run_due_rotation(
    config: &Config,
    registry: &BackendRegistry,
    backend_name: &str,
    vault: &str,
    options: &DueRotationOptions,
    observer: &mut dyn DueRotationObserver,
) -> DueRotationResult {
    let backend = registry
        .materialize(backend_name)
        // Not `discovery`: a name that does not resolve to a backend is the
        // backend being unavailable, whatever error variant the registry used
        // to say so.
        .map_err(|e| {
            DueRotationError::new(
                DueRotationErrorKind::BackendUnavailable,
                CrosstacheError::from(e),
            )
        })?;
    run_due_rotation_with_backend(config, backend, backend_name, vault, options, observer).await
}

/// [`run_due_rotation`] against an already-materialized backend.
///
/// The CLI adapter uses this: `xv rotate --due` resolves its target through the
/// workspace seam (which may hand back a backend from a workspace-scoped
/// registry that the ambient registry cannot materialize by name), and then
/// passes that exact backend in. `backend_name` is still needed — it is the
/// registry name used for cache invalidation and record writes.
pub(crate) async fn run_due_rotation_with_backend(
    config: &Config,
    backend: Arc<dyn Backend>,
    backend_name: &str,
    vault: &str,
    options: &DueRotationOptions,
    observer: &mut dyn DueRotationObserver,
) -> DueRotationResult {
    let statuses = evaluate_vault_policies(backend.as_ref(), vault)
        .await
        .map_err(DueRotationError::discovery)?;

    let invalid: Vec<String> = statuses
        .iter()
        .filter(|(_, s)| s.is_invalid())
        .map(|(n, _)| n.clone())
        .collect();
    let due: Vec<String> = statuses
        .iter()
        .filter(|(_, s)| s.is_due())
        .map(|(n, _)| n.clone())
        .collect();

    let mut summary = DueRotationSummary {
        policy_managed: statuses.len(),
        due: due.len(),
        rotated: 0,
        failed: 0,
        failures: Vec::new(),
    };

    let plan = DueRotationPlan {
        vault: vault.to_string(),
        policy_managed: statuses.len(),
        invalid,
        due,
    };

    let decision = observer
        .on_plan(&plan)
        .map_err(|e| DueRotationError::new(DueRotationErrorKind::Refused, e))?;
    if decision == PlanDecision::Abort {
        return Ok(summary);
    }

    // An unparseable policy means we cannot know whether that secret is
    // overdue. The CLI refuses the whole run (its observer returns `Err`); a
    // caller that proceeds anyway gets it counted as a failure rather than
    // silently skipped, so a green run never hides an unevaluated secret.
    for name in &plan.invalid {
        summary.failed += 1;
        summary.failures.push(DueRotationFailure {
            category: DueRotationFailureCategory::InvalidPolicy,
        });
        observer.on_failure(
            name,
            DueRotationFailureCategory::InvalidPolicy,
            &CrosstacheError::InvalidArgument(
                DueRotationFailureCategory::InvalidPolicy.message().into(),
            ),
        );
    }

    // Rotate through the same single-secret helper a manual `xv rotate` uses,
    // so record handling, reserved-key guards, and audit/git hooks all apply
    // identically. No rotation interval is passed: `--due` acts on the existing
    // policy and must never redefine it.
    let local_registry = BackendRegistry::new(backend);

    for name in &plan.due {
        match crate::cli::secret_ops::execute_secret_rotate(
            &local_registry,
            backend_name,
            name,
            Some(vault.to_string()),
            options.length,
            options.charset,
            options.generator.clone(),
            false, // show_value
            true,  // force: the batch was confirmed (or needs no confirmation)
            None,  // rotation_interval
            config,
            options.track_context_usage,
        )
        .await
        {
            Ok(()) => {
                // Same invalidation the single-secret path performs, keyed by
                // the RESOLVED registry name rather than the backend's kind.
                crate::cache::invalidation::on_secret_mutation(config, backend_name, vault);
                summary.rotated += 1;
                observer.on_rotated(name);
            }
            Err(e) => {
                let category = DueRotationFailureCategory::classify(&e);
                summary.failed += 1;
                summary.failures.push(DueRotationFailure { category });
                observer.on_failure(name, category, &e);
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
#[path = "scheduled_rotation_tests.rs"]
mod tests;
