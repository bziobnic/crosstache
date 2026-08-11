//! `xv backend` — configured-backend lifecycle.

use crate::cli::helpers::confirm_proceed;
use crate::config::backend_ops::{add_backend, configured_backends, BackendType};
use crate::config::settings::{load_config_no_validation, Config};
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
/// way (see `execute_init_command`'s `_config` parameter) — load a fresh,
/// unresolved copy of the on-disk config as the base for both the
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
    let base = load_config_no_validation().await.unwrap_or_default();

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
