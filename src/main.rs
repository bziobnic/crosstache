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
        | crate::cli::Commands::Completion { .. } => {
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

    // Apply CLI --backend flag to config (overrides config file and env var).
    if let Some(ref backend) = cli.backend {
        config.backend = Some(backend.clone());
    }

    // Build the backend registry for commands that talk to a secrets backend.
    // Commands that don't need a backend (Config, Init, etc.) skip this.
    let needs_backend = !matches!(
        cli.command,
        crate::cli::Commands::Config { .. }
            | crate::cli::Commands::Init
            | crate::cli::Commands::Upgrade { .. }
            | crate::cli::Commands::Version
            | crate::cli::Commands::Completion { .. }
            | crate::cli::Commands::Parse { .. }
    );

    let registry = if needs_backend {
        match backend::BackendRegistry::from_config(&config) {
            Ok(r) => Some(r),
            Err(e) => {
                // If the backend is unsupported or auth fails, surface a
                // clear error instead of silently falling back.
                return Err(CrosstacheError::config(format!(
                    "Failed to initialize '{}' backend: {e}",
                    config.effective_backend_name()
                )));
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
