//! crosstache - Azure Key Vault Management Tool
//!
//! A comprehensive command-line tool for managing Azure Key Vaults,
//! written in Rust for performance, safety, and reliability.

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
#[cfg(feature = "file-ops")]
mod blob;
mod cache;
mod cli;
mod config;
mod error;
mod secret;
mod utils;
mod vault;

use crate::cli::Cli;
use crate::error::{CrosstacheError, Result};

#[tokio::main]
async fn main() {
    // Reset SIGPIPE to default behavior so piping to commands like `head` or
    // `echo` doesn't cause a panic when the reader closes the pipe early.
    reset_sigpipe();

    // Initialize logging
    init_logging();

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

async fn run(cli: Cli) -> Result<()> {
    info!("Starting crosstache");

    // Load configuration differently based on command
    let config = match &cli.command {
        crate::cli::Commands::Config { .. }
        | crate::cli::Commands::Init
        | crate::cli::Commands::Upgrade { .. } => {
            // For config and init commands, load without validation
            load_config_without_validation().await?
        }
        _ => {
            // For other commands, load with validation
            config::load_config().await?
        }
    };

    // Execute the command
    cli.execute(config).await?;

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
    use crate::utils::output;
    use crate::error::CrosstacheError::*;
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

    // Plain-text path: stable code line, optional did-you-mean, detailed
    // per-variant guidance (restored from pre-0.6 UX), then TTY-only hint.
    eprintln!("error[{}]: {}", error.code(), error);

    if let Some(s) = error.suggestion() {
        eprintln!("  did you mean: {s}?");
    }

    // Per-variant guidance (restores pre-0.6 plain-text UX); runs after the
    // stable code line and optional typo suggestion.
    match error {
        AuthenticationError(msg) => {
            output::error("Authentication Error");
            eprintln!("{msg}");
        }
        AzureApiError(msg) => {
            output::error("Azure API Error");
            eprintln!("{msg}");
        }
        NetworkError(msg) => {
            output::error("Network Error");
            eprintln!("{msg}");
        }
        ConfigError(msg) => {
            output::error("Configuration Error");
            eprintln!("{msg}");
        }
        ConfigLoadError(e) => {
            output::error("Configuration Error");
            eprintln!("{e}");
        }
        VaultNotFound { name, .. } => {
            output::error("Vault Not Found");
            eprintln!("The Azure Key Vault '{name}' was not found.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The vault name is correct");
            eprintln!("2. The vault exists in your subscription");
            eprintln!("3. You have access to the vault");
            eprintln!("4. You're using the correct subscription");
        }
        SecretNotFound { name, .. } => {
            output::error("Secret Not Found");
            eprintln!("The secret '{name}' was not found in the vault.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The secret name is correct");
            eprintln!("2. The secret exists in the vault");
            eprintln!("3. You have 'Get' permissions for secrets");
        }
        InvalidSecretName { name } => {
            output::error("Invalid Secret Name");
            eprintln!("The name '{name}' is not allowed.");
            eprintln!("\nSecret names must follow Azure Key Vault naming rules (alphanumeric and hyphens).");
        }
        PermissionDenied(msg) => {
            output::error("Permission Denied");
            eprintln!("{msg}");
            eprintln!("\nPlease verify:");
            eprintln!("1. Your account has the required RBAC role (e.g., 'Key Vault Secrets User' or 'Key Vault Administrator')");
            eprintln!("2. You have access to the Azure subscription");
            eprintln!("3. If using access policies, ensure Get/List/Set permissions are granted");
            eprintln!("\nTo check your roles: xv vault roles");
        }
        DnsResolutionError { vault_name, .. } => {
            output::error("DNS Resolution Failed");
            eprintln!("Could not resolve the vault '{vault_name}'.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The vault name is spelled correctly");
            eprintln!("2. The vault exists and has not been deleted");
            eprintln!("3. Your network/DNS settings are correct");
        }
        ConnectionTimeout(msg) => {
            output::error("Connection Timeout");
            eprintln!("{msg}");
            eprintln!("\nPlease check your network connection and try again.");
        }
        ConnectionRefused(msg) => {
            output::error("Connection Refused");
            eprintln!("{msg}");
            eprintln!("\nPlease check that the vault exists and is accessible.");
        }
        SslError(msg) => {
            output::error("SSL/TLS Error");
            eprintln!("{msg}");
            eprintln!("\nPlease check your TLS configuration and proxy settings.");
        }
        InvalidUrl(msg) => {
            output::error("Invalid URL");
            eprintln!("{msg}");
            eprintln!("\nCheck the endpoint URL and any proxy or redirect configuration.");
        }
        InvalidArgument(msg) => {
            output::error("Invalid Argument");
            eprintln!("{msg}");
        }
        Upgrade(msg) => {
            output::error("Upgrade Error");
            eprintln!("{msg}");
        }
        IoError(e) => {
            output::error("I/O Error");
            eprintln!("{e}");
            eprintln!("\nPlease check file permissions and available disk space.");
        }
        JsonError(e) => {
            output::error("JSON Parse Error");
            eprintln!("{e}");
            eprintln!("\nThe response or data could not be parsed. This may indicate a corrupt file or unexpected API response.");
        }
        YamlError(e) => {
            output::error("YAML Parse Error");
            eprintln!("{e}");
            eprintln!("\nThe configuration or data could not be parsed. Check the file for syntax errors.");
        }
        SerializationError(msg) => {
            output::error("Serialization Error");
            eprintln!("{msg}");
            eprintln!("\nThe data could not be encoded or decoded. Check for corrupt or incompatible content.");
        }
        HttpError(e) => {
            output::error("HTTP Error");
            eprintln!("{e}");
            eprintln!("\nPlease check your network connection and Azure service status.");
        }
        UuidError(e) => {
            output::error("Invalid UUID");
            eprintln!("{e}");
            eprintln!("\nA resource identifier is in an unexpected format.");
        }
        RegexError(e) => {
            output::error("Invalid Pattern");
            eprintln!("{e}");
        }
        Unknown(msg) => {
            output::error("Error");
            eprintln!("{msg}");
        }
    }

    if std::io::stderr().is_terminal() {
        if let Some(hint) = hint_for(error.code()) {
            eprintln!("  hint: {hint}");
        }
    }
}
