//! `xv backend` — configured-backend lifecycle.

use crate::cli::config_ops::guard_against_project_vaults_overlay;
use crate::cli::helpers::confirm_proceed;
use crate::config::backend_ops::{add_backend, configured_backends, BackendType};
use crate::config::settings::{load_config_file_only, Config};
use crate::config::setup::atomic_save_config;
use crate::error::Result;
use crate::utils::output;

/// Where a backend's secrets live, for the `ls` listing.
fn location(backend: BackendType, config: &Config) -> String {
    match backend {
        BackendType::Local => config
            .local
            .as_ref()
            .and_then(|l| l.store_path.clone())
            .unwrap_or_else(|| "(default store path)".into()),
        BackendType::Azure => {
            let azure = config.azure_settings();
            azure
                .default_vault
                .unwrap_or_else(|| "(no vault configured)".into())
        }
        BackendType::Aws => config
            .aws
            .as_ref()
            .and_then(|a| a.region.clone())
            .unwrap_or_else(|| "(no region configured)".into()),
    }
}

pub(crate) async fn execute_backend_ls(config: Config) -> Result<()> {
    let configured = configured_backends(&config);
    if configured.is_empty() {
        // Matches every sibling list-style empty-state message in this repo
        // (vault_ops, secret_ops, file_ops, ...): guidance/chrome goes to
        // stderr via output::info, keeping stdout reserved for data rows.
        output::info("No backends configured. Run `xv init` to configure one.");
        return Ok(());
    }

    let active = config.effective_backend_name();
    for backend in configured {
        let marker = if backend.as_str() == active {
            "  (active)"
        } else {
            ""
        };
        println!(
            "{}\t{}{marker}",
            backend.as_str(),
            location(backend, &config)
        );
    }
    Ok(())
}

/// `_config` is the dispatch-resolved config (`main.rs` unconditionally
/// overwrites `.backend` on it to compute the *effective* backend for the
/// command about to run — see `resolve_effective_backend`). Saving that back
/// out would silently relocate the user's actual active backend to whatever
/// `--backend`/`XV_BACKEND`/the cwd's `.xv.toml` profile happened to resolve
/// to for THIS invocation. `xv init` has the same trap and dodges it the same
/// way (see `execute_init_command`'s `_config` parameter) — load a fresh
/// on-disk config (`load_config_file_only`, which unlike
/// `load_config_no_validation` also skips environment-variable overrides —
/// `XV_BACKEND` is exactly as dispatch-resolved as `--backend` is, and must
/// never round-trip into a save either) as the base for both the
/// already-configured check and `add_backend`.
pub(crate) async fn execute_backend_add(backend: String, yes: bool, _config: Config) -> Result<()> {
    execute_backend_add_inner(
        backend,
        yes,
        _config,
        crate::config::init::ConfigInitializer::new(),
    )
    .await
}

/// Body of `execute_backend_add`, with the `ConfigInitializer` injectable so
/// tests can drive the interactive collector through a `ScriptedPrompter`
/// instead of a real terminal — dialoguer's `Input` refuses to run at all
/// without one, so there is no way to exercise the save path through the
/// public entry point in an automated test.
///
/// `_config` is intentionally unused: see the doc comment on
/// `execute_backend_add`.
async fn execute_backend_add_inner(
    backend: String,
    yes: bool,
    _config: Config,
    initializer: crate::config::init::ConfigInitializer,
) -> Result<()> {
    let backend: BackendType = backend.parse()?;
    let base = load_config_file_only().await.unwrap_or_default();

    if configured_backends(&base).contains(&backend) {
        let prompt = format!(
            "Backend '{backend}' is already configured; reconfigure it? \
             Its existing settings will be replaced."
        );
        if !confirm_proceed(yes, &prompt, "--yes")? {
            output::info("Aborted; no changes made.");
            return Ok(());
        }
    }

    let request = initializer.collect_backend_request(backend).await?;

    // `xv backend add` never moves the write target — that is `xv init`'s job.
    let updated = add_backend(&request, base, false).await?;
    atomic_save_config(&updated, &Config::get_config_path()?).await?;

    output::success(&format!("Configured backend '{backend}'"));
    output::info(&format!(
        "It is not the active backend. Switch with `xv config set backend {backend}`, \
         or attach one of its vaults with `xv cx add <vault> --backend {backend}`."
    ));
    if backend == BackendType::Azure {
        output::info(
            "Blob storage was not configured for this vault (used for `xv file` operations). \
             `xv init` sets that up interactively; add it later if you need file storage.",
        );
    }
    Ok(())
}

/// `config` is the dispatch-resolved config — see the doc comment on
/// `execute_backend_add` for why it must never be the base for a save. It is
/// used for exactly one thing here: `guard_against_project_vaults_overlay`
/// needs `config.env_flag` (the CLI `--env` for THIS invocation, which is
/// never persisted to disk) to know which `.xv.toml` env profile is active.
/// Every value-affecting decision and the save itself use `base`, a fresh
/// `load_config_file_only()` read — no environment-variable overrides, no
/// dispatch-resolved `.backend`.
pub(crate) async fn execute_backend_rm(
    backend: String,
    purge: bool,
    yes: bool,
    config: Config,
) -> Result<()> {
    let backend: BackendType = backend.parse()?;
    let base = load_config_file_only().await.unwrap_or_default();
    let configured = configured_backends(&base);

    if !configured.contains(&backend) {
        let names: Vec<&str> = configured.iter().map(|b| b.as_str()).collect();
        let listed = if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        };
        return Err(crate::error::CrosstacheError::invalid_argument(format!(
            "backend '{backend}' is not configured; configured backends: {listed}"
        )));
    }

    if purge && backend != BackendType::Local {
        return Err(crate::error::CrosstacheError::invalid_argument(format!(
            "--purge deletes an on-disk store and applies to the local backend only; \
             '{backend}' stores its secrets remotely. Remove the configuration with \
             `xv backend rm {backend}`, and delete remote data with `xv vault delete`."
        )));
    }

    // Refuse to silently relocate the write target.
    if base.effective_backend_name() == backend.as_str() && configured.len() > 1 {
        let others: Vec<&str> = configured
            .iter()
            .filter(|b| **b != backend)
            .map(|b| b.as_str())
            .collect();
        return Err(crate::error::CrosstacheError::invalid_argument(format!(
            "'{backend}' is the active backend; switch first with \
             `xv config set backend {}`, then `xv backend rm {backend}`",
            others[0]
        )));
    }

    // A `.xv.toml` `[env.X].vaults` overlay REPLACES the context workspace
    // entirely (see `guard_against_project_vaults_overlay`'s doc comment in
    // config_ops.rs), so if one is active for this directory, the context
    // workspace this function is about to read and mutate below is not the
    // workspace actually in effect — same guard, same place in the sequence
    // as `execute_cx_rm`, before any context read/write.
    guard_against_project_vaults_overlay(&config).await?;

    // Refuse when removal would strand the workspace's write target. Mirrors
    // `execute_cx_rm` in src/cli/config_ops.rs.
    let mut context_manager = crate::config::context::ContextManager::load().await?;
    if let Some(ws) = context_manager.workspace.clone() {
        let doomed: Vec<_> = ws
            .entries
            .iter()
            .filter(|e| e.backend.as_deref() == Some(backend.as_str()))
            .collect();
        let removes_default = doomed.iter().any(|e| e.default);
        let survivors = ws.entries.len() - doomed.len();
        if removes_default && survivors > 0 {
            return Err(crate::error::CrosstacheError::invalid_argument(format!(
                "removing '{backend}' would remove the workspace's default vault; \
                 choose a new default with `xv cx default <alias>` first"
            )));
        }
    }

    if !confirm_proceed(
        yes,
        &format!("Remove backend '{backend}' from the configuration?"),
        "--yes",
    )? {
        output::info("Aborted; no changes made.");
        return Ok(());
    }

    // Resolve the config path and build the updated config up front, before
    // either write, so a failure here (as opposed to mid-write) leaves both
    // files untouched.
    let config_path = Config::get_config_path()?;
    let mut updated = base;
    let data_location = match backend {
        BackendType::Local => {
            let where_it_lives = updated
                .local
                .as_ref()
                .and_then(|l| l.store_path.clone())
                .unwrap_or_else(|| "(default store path)".into());
            updated.local = None;
            where_it_lives
        }
        BackendType::Azure => {
            updated.azure = None;
            updated.subscription_id.clear();
            updated.tenant_id.clear();
            updated.default_resource_group.clear();
            updated.default_location.clear();
            "Azure Key Vault (unchanged)".to_string()
        }
        BackendType::Aws => {
            updated.aws = None;
            "AWS Secrets Manager (unchanged)".to_string()
        }
    };

    if updated.effective_backend_name() == backend.as_str() {
        updated.backend = None;
    }

    // Save the config FIRST: it's the more-recoverable half (re-adding a
    // backend is a scripted `xv backend add`) and it's the one that gates
    // whether this backend is "removed" at all. If it fails, nothing else
    // has been touched yet. Compute the pruned workspace now so the context
    // write below is a plain save with no further decisions to make.
    let pruned_workspace = context_manager.workspace.clone().and_then(|mut ws| {
        let before = ws.entries.len();
        ws.entries
            .retain(|e| e.backend.as_deref() != Some(backend.as_str()));
        if ws.entries.len() == before {
            None // nothing to drop; leave context_manager.workspace as-is
        } else if ws.entries.is_empty() {
            Some(None)
        } else {
            Some(Some(ws))
        }
    });

    atomic_save_config(&updated, &config_path).await?;

    // Drop workspace entries that pointed at the removed backend, now that
    // the config save committed.
    if let Some(new_workspace) = pruned_workspace {
        context_manager.workspace = new_workspace;
        context_manager.save().await?;
    }

    output::success(&format!(
        "Removed backend '{backend}' from the configuration"
    ));
    output::info(&format!(
        "Data was not deleted; it remains at: {data_location}"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::init::ConfigInitializer;
    use crate::utils::interactive::{Answer, ScriptedPrompter};

    /// RAII guard that sets an env var for its lifetime and restores the
    /// previous value (or removes it, if previously unset) on drop. Same
    /// pattern as `config::context::tests::EnvVarGuard` / `cli::secret_ops`'s
    /// copy — no shared test-util module exists in this crate.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// Regression for a CRITICAL finding: `main.rs` unconditionally resolves
    /// `config.backend` to the *effective* backend for the command about to
    /// run (CLI `--backend` / `XV_BACKEND` / the cwd's `.xv.toml` profile)
    /// before dispatch, and hands that resolved `Config` to every command
    /// handler — including `execute_backend_add`. Saving it back out would
    /// silently relocate the user's real active backend to whatever this one
    /// invocation happened to resolve to. This drives `execute_backend_add_inner`
    /// with a `_config` whose `.backend` deliberately disagrees with the
    /// on-disk file, and asserts the save reflects only the fresh on-disk
    /// read — never `_config`.
    ///
    /// A real terminal is required to exercise `execute_backend_add`'s
    /// success path (dialoguer's `Input` refuses to run without one), so this
    /// injects a `ScriptedPrompter` via `execute_backend_add_inner` rather
    /// than spawning the `xv` binary.
    #[tokio::test]
    async fn execute_backend_add_inner_ignores_the_dispatch_configs_backend() {
        let home = tempfile::tempdir().unwrap();
        let conf_dir = home.path().join("xv");
        std::fs::create_dir_all(&conf_dir).unwrap();
        let store_path = home.path().join("store");
        let key_file = home.path().join("key.txt");
        std::fs::write(
            conf_dir.join("xv.conf"),
            format!(
                r#"
backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true

[local]
store_path = "{}"
key_file = "{}"
default_vault = "default"
"#,
                store_path.display(),
                key_file.display(),
            ),
        )
        .unwrap();

        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", home.path());
        let _home = EnvVarGuard::set("HOME", home.path());

        // Simulates exactly what `main.rs` hands to every command handler: a
        // `Config` whose `.backend` has already been overwritten to the
        // dispatch-resolved value ("aws" here) for this invocation, even
        // though the on-disk active backend is "local".
        let dispatch_config = Config {
            backend: Some("aws".to_string()),
            ..Config::default()
        };

        let initializer = ConfigInitializer::with_prompter(Box::new(ScriptedPrompter::new(vec![
            Answer::Text(store_path.to_string_lossy().to_string()),
            Answer::Text(key_file.to_string_lossy().to_string()),
            Answer::Text("default".to_string()),
        ])));

        // Reconfiguring the already-configured `local` backend, `--yes`'d.
        // This is orthogonal to the active-backend question the test cares
        // about — it only needs a backend `add_backend` will accept.
        execute_backend_add_inner("local".to_string(), true, dispatch_config, initializer)
            .await
            .unwrap();

        let saved = std::fs::read_to_string(conf_dir.join("xv.conf")).unwrap();
        assert!(
            saved.contains("backend = \"local\""),
            "the on-disk active backend must stay 'local', not the dispatch \
             config's 'aws': {saved}"
        );
        assert!(
            !saved.contains("backend = \"aws\""),
            "the dispatch config's backend must never be written: {saved}"
        );
    }
}
