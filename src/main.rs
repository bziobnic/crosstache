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

async fn run(cli: Cli) -> Result<()> {
    info!("Starting crosstache");

    // Load configuration differently based on command
    let mut config = match &cli.command {
        crate::cli::Commands::Config { .. }
        | crate::cli::Commands::Init
        | crate::cli::Commands::Upgrade { .. }
        | crate::cli::Commands::Version
        | crate::cli::Commands::Completion { .. }
        | crate::cli::Commands::Migrate { .. } => {
            // These commands don't talk to Azure — skip credential validation.
            load_config_without_validation().await?
        }
        _ => {
            // For other commands, load with validation
            config::load_config().await?
        }
    };

    // Apply CLI --env flag to config (used by resolve_vault_name and
    // resolve_resource_group when consulting .xv.toml).
    config.env_flag = cli.env.clone();

    // Resolve env-profile backend (validate early; fail before touching Azure).
    // Precedence: CLI --backend > env-profile backend > config-file backend > "azure".
    let profile_backend: Option<String> = if cli.backend.is_none() {
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
    config.cli_backend_was_arg = std::env::args_os()
        .any(|a| a == "--backend" || a.to_string_lossy().starts_with("--backend="));

    config.backend = Some(
        crate::config::project::resolve_effective_backend(
            cli.backend.as_deref(),
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

    // Build the backend registry for commands that talk to a secrets backend.
    // Commands that are purely local (Config, Init, Cache, Context, etc.) skip
    // this entirely.  For commands that *may* need the backend we attempt
    // construction but treat failure as non-fatal: the registry becomes `None`
    // and individual command handlers will create their own auth provider on
    // demand via `get_azure_auth_provider(None, config)`.
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
