//! crosstache - Azure Key Vault Management Tool
//!
//! A comprehensive command-line tool for managing Azure Key Vaults,
//! written in Rust for performance, safety, and reliability.

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth;
mod cli;
mod config;
mod error;
mod secret;
mod utils;
mod vault;

use crate::cli::Cli;
use crate::error::{crosstacheError, Result};

#[tokio::main]
async fn main() {
    // Initialize logging
    init_logging();

    // Parse command-line arguments
    let cli = Cli::parse();

    // Execute the command
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    info!("Starting crosstache");

    // Load configuration differently based on command
    let config = match &cli.command {
        crate::cli::Commands::Config { .. } => {
            // For config commands, load without validation
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
    use crate::config::Config;

    let mut config = Config::default();

    // Load from configuration file if it exists
    let config_path = Config::get_config_path()?;
    if config_path.exists() {
        config = load_from_file(&config_path).await?;
    }

    // Override with environment variables
    load_from_env(&mut config);

    // Don't validate for config commands
    Ok(config)
}

async fn load_from_file(path: &std::path::PathBuf) -> Result<crate::config::Config> {
    let contents = tokio::fs::read_to_string(path).await?;

    // Try to parse as TOML first, then JSON as fallback
    if let Ok(config) = toml::from_str::<crate::config::Config>(&contents) {
        return Ok(config);
    }

    let config = serde_json::from_str::<crate::config::Config>(&contents)?;
    Ok(config)
}

fn load_from_env(config: &mut crate::config::Config) {
    if let Ok(value) = std::env::var("DEBUG") {
        config.debug = value.to_lowercase() == "true" || value == "1";
    }

    if let Ok(value) = std::env::var("AZURE_SUBSCRIPTION_ID") {
        config.subscription_id = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_VAULT") {
        config.default_vault = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_RESOURCE_GROUP") {
        config.default_resource_group = value;
    }

    if let Ok(value) = std::env::var("AZURE_TENANT_ID") {
        config.tenant_id = value;
    }

    if let Ok(value) = std::env::var("FUNCTION_APP_URL") {
        config.function_app_url = value;
    }

    if let Ok(value) = std::env::var("CACHE_TTL") {
        if let Ok(seconds) = value.parse::<u64>() {
            config.cache_ttl = std::time::Duration::from_secs(seconds);
        }
    }
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
