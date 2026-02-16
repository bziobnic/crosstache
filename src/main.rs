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
    // Initialize logging
    init_logging();

    // Parse command-line arguments
    let cli = Cli::parse();

    // Execute the command
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    info!("Starting crosstache");

    // Load configuration differently based on command
    let config = match &cli.command {
        crate::cli::Commands::Config { .. } | crate::cli::Commands::Init => {
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

fn print_user_friendly_error(error: &CrosstacheError) {
    use CrosstacheError::*;

    match error {
        AuthenticationError(msg) => {
            eprintln!("🔐 Authentication Error");
            eprintln!("{msg}");
        }
        AzureApiError(msg) => {
            eprintln!("☁️  Azure API Error");
            eprintln!("{msg}");
        }
        NetworkError(msg) => {
            eprintln!("🌐 Network Error");
            eprintln!("{msg}");
        }
        ConfigError(msg) => {
            eprintln!("⚙️  Configuration Error");
            eprintln!("{msg}");
        }
        VaultNotFound { name } => {
            eprintln!("🔒 Vault Not Found");
            eprintln!("The Azure Key Vault '{name}' was not found.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The vault name is correct");
            eprintln!("2. The vault exists in your subscription");
            eprintln!("3. You have access to the vault");
            eprintln!("4. You're using the correct subscription");
        }
        SecretNotFound { name } => {
            eprintln!("🔑 Secret Not Found");
            eprintln!("The secret '{name}' was not found in the vault.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The secret name is correct");
            eprintln!("2. The secret exists in the vault");
            eprintln!("3. You have 'Get' permissions for secrets");
        }
        PermissionDenied(msg) => {
            eprintln!("🚫 Permission Denied");
            eprintln!("{msg}");
            eprintln!("\nPlease verify:");
            eprintln!("1. Your account has the necessary permissions");
            eprintln!("2. You have access to the Azure subscription");
            eprintln!("3. The resource you're trying to access exists");
        }
        _ => {
            eprintln!("❌ Error: {error}");
        }
    }
}
