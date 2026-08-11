//! `xv backend` — configured-backend lifecycle.

use crate::config::backend_ops::{configured_backends, BackendType};
use crate::config::settings::Config;
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
