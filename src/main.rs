//! crosstache - Azure Key Vault Management Tool
//!
//! A comprehensive command-line tool for managing Azure Key Vaults,
//! written in Rust for performance, safety, and reliability.

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod backend;
#[cfg(feature = "file-ops")]
mod blob;
mod cache;
mod cli;
mod config;
mod error;
mod scan;
mod secret;
#[cfg(feature = "tui")]
mod tui;
mod utils;
mod vault;

use crate::cli::Cli;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;

#[tokio::main]
async fn main() {
    // Reset SIGPIPE to default behavior so piping to commands like `head` or
    // `echo` doesn't cause a panic when the reader closes the pipe early.
    reset_sigpipe();

    // Initialize logging
    init_logging();

    // Handle special internal commands before clap parsing
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "__complete-secrets" {
        let format = OutputFormat::Plain; // Default format for completion
        if let Err(e) = run_complete_secrets().await {
            error!("Error: {}", e);
            print_user_friendly_error(&e, format);
            std::process::exit(e.exit_code());
        }
        return;
    }

    if args.len() > 1 && args[1] == "__complete-folders" {
        let format = OutputFormat::Plain; // Default format for completion
        if let Err(e) = run_complete_folders().await {
            error!("Error: {}", e);
            print_user_friendly_error(&e, format);
            std::process::exit(e.exit_code());
        }
        return;
    }

    // Parse command-line arguments
    let cli = Cli::parse();
    let format = cli.format; // OutputFormat is Copy

    // Execute the command
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e, format);
        std::process::exit(e.exit_code());
    }
}

async fn run_complete_secrets() -> Result<()> {
    // Load config without validation for internal complete-secrets command
    let config = load_config_without_validation().await?;
    crate::cli::secret_ops::execute_complete_secrets(config).await
}

async fn run_complete_folders() -> Result<()> {
    // Load config without validation for the internal completion command
    let config = load_config_without_validation().await?;
    crate::cli::secret_ops::execute_complete_folders(config).await
}

async fn run(cli: Cli) -> Result<()> {
    info!("Starting crosstache");

    // Load configuration WITHOUT validation for every command. Validation is
    // deferred until after the `.xv.toml` env-profile backend has been
    // resolved and folded into `config.backend` below (and only performed
    // for commands that actually need a backend) — a profile selecting
    // `local`/`aws` must not be rejected against global Azure config that
    // was never going to be used (issue #305).
    let mut config = load_config_without_validation().await?;

    // Apply CLI --env flag to config (used by resolve_vault_name and
    // resolve_resource_group when consulting .xv.toml).
    config.env_flag = cli.env.clone();

    // Determine whether `--backend` was actually passed as a CLI argument, as
    // opposed to merely populated by clap from `XV_BACKEND` via `env =
    // "XV_BACKEND"` on the flag (see src/cli/commands.rs). This must be
    // computed BEFORE the profile-backend lookup below, since the lookup is
    // now gated on it rather than on `cli.backend.is_none()` — a real
    // `--backend` flag should suppress the profile lookup, but `XV_BACKEND`
    // alone must not (issue #305).
    // Stop scanning at the `--` separator: tokens after it belong to a
    // passthrough child command (e.g. `xv run -- echo --backend prod`) and
    // must not be mistaken for a real `--backend` flag on `xv` itself.
    let cli_backend_was_arg = std::env::args_os()
        .skip(1)
        .take_while(|a| a != "--")
        .any(|a| a == "--backend" || a.to_string_lossy().starts_with("--backend="));

    // Resolve env-profile backend (validate early; fail before touching Azure).
    // Precedence: true `--backend` flag > .xv.toml env-profile backend >
    // XV_BACKEND / global config-file backend > built-in "azure".
    // Gated on `!cli_backend_was_arg` (not `cli.backend.is_none()`) so that
    // `XV_BACKEND` — which clap folds into `cli.backend` indistinguishably
    // from a real flag — does not suppress the profile lookup.
    let profile_backend: Option<String> = if !cli_backend_was_arg {
        match std::env::current_dir() {
            Err(_) => None, // degenerate env — skip project-config walk
            Ok(cwd) => {
                if let Ok(Some((path, proj_cfg))) =
                    crate::config::project::find_project_config(&cwd).await
                {
                    if let Ok((name, profile)) =
                        crate::config::project::resolve_env(&proj_cfg, config.env_flag.as_deref())
                    {
                        if let Some(ref b) = profile.backend {
                            crate::config::project::validate_env_profile_backend(b)?;
                            // Emit a notice when the project file (especially an
                            // ancestor directory's) overrides the backend — this
                            // is the highest-impact override a .xv.toml can make.
                            if path.parent().map(|p| p != cwd.as_path()).unwrap_or(false) {
                                if let Some(line) =
                                    crate::config::project::capture_cross_boundary_notice(
                                        &path, name,
                                    )
                                {
                                    eprintln!("{line}");
                                }
                            }
                            tracing::debug!(
                                "backend '{b}' selected by env profile '{name}' in {}",
                                path.display()
                            );
                            Some(b.clone())
                        } else {
                            None
                        }
                    } else {
                        // Unknown env name — skip; command handler surfaces the error.
                        None
                    }
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    // Snapshot the on-disk backend value BEFORE resolution overwrites it, plus
    // the original CLI flag and whether `--backend` was actually passed
    // (vs. populated by clap from XV_BACKEND via `env = "XV_BACKEND"`).
    // `xv config show --resolved` reads these to attribute precedence
    // correctly when values across layers coincide.
    config.disk_backend = config.backend.clone();
    config.cli_backend = cli.backend.clone();
    config.cli_backend_was_arg = cli_backend_was_arg;

    // Only feed the CLI slot when `--backend` was a real argument — when it
    // was merely populated from `XV_BACKEND`, that value already flows into
    // `config.backend` via `load_from_env` and participates at the
    // config-file layer instead, letting the .xv.toml profile outrank it.
    let cli_backend_for_resolution = if cli_backend_was_arg {
        cli.backend.as_deref()
    } else {
        None
    };

    config.backend = Some(
        crate::config::project::resolve_effective_backend(
            cli_backend_for_resolution,
            profile_backend.as_deref(),
            config.backend.as_deref(),
        )
        .to_string(),
    );

    // P0.3: Emit a clear error before dispatch when AWS is requested on a build
    // that was compiled without --features aws, rather than surfacing a generic
    // "No backend registry available" message later.
    #[cfg(not(feature = "aws"))]
    if config.effective_backend_name() == "aws" {
        return Err(CrosstacheError::backend_unavailable(
            "aws",
            "AWS backend is not included in this build. \
Rebuild with `cargo build --features aws` or install an AWS-enabled binary.",
        ));
    }

    // Commands that never talk to a secrets backend (this `matches!` is the
    // source of truth for exactly which ones) must not be validated against
    // one. Computed BEFORE validation (moved up from its original position
    // just above the registry-construction block below) so `needs_backend`
    // can gate both.
    let needs_backend = !matches!(
        cli.command,
        crate::cli::Commands::Config { .. }
            | crate::cli::Commands::Init
            | crate::cli::Commands::Upgrade { .. }
            | crate::cli::Commands::Version
            | crate::cli::Commands::Completion { .. }
            | crate::cli::Commands::Parse { .. }
            | crate::cli::Commands::Cache { .. }
            | crate::cli::Commands::Local { .. }
            | crate::cli::Commands::Context { .. }
            // Env subcommands that only read/write `.xv.toml` need no backend.
            // `env pull` / `env push` DO talk to the active backend, so they are
            // intentionally excluded here and get a registry built below.
            | crate::cli::Commands::Env {
                command: crate::cli::commands::EnvCommands::List
                    | crate::cli::commands::EnvCommands::Use { .. }
                    | crate::cli::commands::EnvCommands::Create { .. }
                    | crate::cli::commands::EnvCommands::Delete { .. }
                    | crate::cli::commands::EnvCommands::Show,
            }
            | crate::cli::Commands::Migrate { .. }
            | crate::cli::Commands::Scan {
                command: Some(crate::cli::commands::ScanCommands::Install { .. }),
                ..
            }
            | crate::cli::Commands::Scan {
                command: Some(crate::cli::commands::ScanCommands::Uninstall),
                ..
            }
    );

    // Validate the effective backend config only for commands that actually
    // need it. Validating unconditionally (as the pre-#305 code did, before
    // the profile backend had even been folded in) rejects setup-oriented
    // commands like `context init --backend aws` for lacking config that
    // they exist to create in the first place.
    if needs_backend {
        config.validate()?;
    }

    // Build the backend registry for commands that talk to a secrets backend.
    // For commands that *may* need the backend we attempt construction but
    // treat failure as non-fatal: the registry becomes `None` and individual
    // command handlers will create their own auth provider on demand via
    // `get_azure_auth_provider(None, config)`.
    let registry = if needs_backend {
        match backend::BackendRegistry::from_config(&config) {
            Ok(r) => Some(r),
            Err(e) => {
                // Log but don't block — commands that genuinely need the
                // backend will fail with their own clear error when they
                // call `get_azure_auth_provider`.
                tracing::debug!(
                    "Backend '{}' init failed (non-fatal): {e}",
                    config.effective_backend_name()
                );
                None
            }
        }
    } else {
        None
    };

    // Execute the command
    cli.execute(config, registry.as_ref()).await?;

    Ok(())
}

async fn load_config_without_validation() -> Result<crate::config::Config> {
    use crate::config::load_config_no_validation;

    // Use the config module's function but without validation
    load_config_no_validation().await
}

fn init_logging() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crosstache=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Reset SIGPIPE to default so the process terminates cleanly when a pipe reader
/// (e.g., `head`, `echo`) closes early, instead of panicking on write.
#[cfg(unix)]
fn reset_sigpipe() {
    // SAFETY: This installs the process-wide default disposition for SIGPIPE
    // before any worker threads are spawned. Both the signal number and handler
    // constant come from libc, and no Rust references or memory are touched.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {
    // No-op on non-Unix platforms
}

fn print_user_friendly_error(error: &CrosstacheError, format: crate::utils::format::OutputFormat) {
    use crate::utils::error_hints::hint_for;
    use crate::utils::format::OutputFormat;
    use std::io::IsTerminal;

    // Machine-readable envelope on stdout only when the user explicitly set
    // `--format json|yaml`. Do not use `resolve_for_stdout()` here: default
    // `Auto` becomes JSON when stdout is not a TTY, which would write errors to
    // stdout and break pipelines (e.g. `xv get SECRET | consuming-command`).
    if matches!(format, OutputFormat::Json | OutputFormat::Yaml) {
        let mut envelope = serde_json::json!({
            "error": {
                "code": error.code(),
                "message": error.to_string(),
                "exit_code": error.exit_code(),
            }
        });
        if let Some(s) = error.suggestion() {
            envelope["error"]["suggestion"] = serde_json::Value::String(s.to_string());
        }
        let rendered = match format {
            OutputFormat::Json => serde_json::to_string(&envelope).unwrap_or_default(),
            OutputFormat::Yaml => serde_yaml::to_string(&envelope).unwrap_or_default(),
            _ => unreachable!(),
        };
        println!("{rendered}");
        return;
    }

    // Plain-text path: one primary line (`Display` is the message), optional
    // suggestion, then TTY-only hint. Do not add a second copy of the payload
    // here — `error` already formats the full message via thiserror.
    eprintln!("error[{}]: {}", error.code(), error);

    if let Some(s) = error.suggestion() {
        eprintln!("  did you mean: {s}?");
    }

    if std::io::stderr().is_terminal() {
        if let Some(hint) = hint_for(error.code()) {
            eprintln!("  hint: {hint}");
        }
    }
}
