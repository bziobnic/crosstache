//! Interactive input utilities for user prompts and setup workflows
//!
//! This module provides utilities for creating interactive command-line experiences
//! including prompts, confirmations, and progress indicators.

use crate::error::{CrosstacheError, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Interactive prompt utilities
pub struct InteractivePrompt {
    theme: ColorfulTheme,
}

impl InteractivePrompt {
    /// Create a new interactive prompt instance
    pub fn new() -> Self {
        Self {
            theme: ColorfulTheme::default(),
        }
    }

    /// Display a welcome message for the setup process
    pub fn welcome(&self) -> Result<()> {
        println!("🚀 Welcome to crosstache!");
        println!("Let's get you set up for Azure Key Vault management.");
        println!();
        Ok(())
    }

    /// Prompt for yes/no confirmation with a default value
    pub fn confirm(&self, message: &str, default: bool) -> Result<bool> {
        let result = Confirm::with_theme(&self.theme)
            .with_prompt(message)
            .default(default)
            .interact()
            .map_err(|e| CrosstacheError::config(format!("Failed to get user input: {e}")))?;
        Ok(result)
    }

    /// Prompt for text input with optional default and validation
    #[allow(dead_code)]
    pub fn input_text(&self, message: &str, default: Option<&str>) -> Result<String> {
        let mut input = Input::with_theme(&self.theme).with_prompt(message);
        
        if let Some(default_value) = default {
            input = input.default(default_value.to_string());
        }

        let result = input
            .interact_text()
            .map_err(|e| CrosstacheError::config(format!("Failed to get user input: {e}")))?;
        
        Ok(result)
    }

    /// Prompt for text input with validation function
    pub fn input_text_validated<F>(&self, message: &str, default: Option<&str>, validator: F) -> Result<String>
    where
        F: Fn(&str) -> std::result::Result<(), String> + 'static,
    {
        let mut input = Input::with_theme(&self.theme)
            .with_prompt(message)
            .validate_with(|input: &String| validator(input.as_str()));
        
        if let Some(default_value) = default {
            input = input.default(default_value.to_string());
        }

        let result = input
            .interact_text()
            .map_err(|e| CrosstacheError::config(format!("Failed to get user input: {e}")))?;
        
        Ok(result)
    }

    /// Prompt for selection from a list of options
    pub fn select(&self, message: &str, options: &[String], default: Option<usize>) -> Result<usize> {
        let mut select = Select::with_theme(&self.theme)
            .with_prompt(message)
            .items(options)
            .max_length(20);

        if let Some(default_index) = default {
            select = select.default(default_index);
        }

        let result = select
            .interact()
            .map_err(|e| CrosstacheError::config(format!("Failed to get user selection: {e}")))?;
        
        Ok(result)
    }

    /// Display an informational message
    pub fn info(&self, message: &str) -> Result<()> {
        println!("ℹ️  {message}");
        Ok(())
    }

    /// Display a success message
    pub fn success(&self, message: &str) -> Result<()> {
        println!("✅ {message}");
        Ok(())
    }

    /// Display a warning message
    #[allow(dead_code)]
    pub fn warning(&self, message: &str) -> Result<()> {
        println!("⚠️  {message}");
        Ok(())
    }

    /// Display an error message
    pub fn error(&self, message: &str) -> Result<()> {
        println!("❌ {message}");
        Ok(())
    }

    /// Display a step header during setup
    pub fn step(&self, step_number: u8, total_steps: u8, title: &str) -> Result<()> {
        println!();
        println!("📋 Step {step_number}/{total_steps}: {title}");
        println!();
        Ok(())
    }
}

impl Default for InteractivePrompt {
    fn default() -> Self {
        Self::new()
    }
}

/// Progress indicator for long-running operations
pub struct ProgressIndicator {
    bar: ProgressBar,
}

impl ProgressIndicator {
    /// Create a new progress indicator
    pub fn new(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
                .template("{spinner:.blue} {msg}")
                .expect("Progress bar template should be valid"),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(100));
        
        Self { bar }
    }

    /// Update the progress message
    pub fn set_message(&self, message: &str) {
        self.bar.set_message(message.to_string());
    }

    /// Finish with success message
    pub fn finish_success(&self, message: &str) {
        self.bar.finish_with_message(format!("✅ {message}"));
    }

    /// Finish with error message
    pub fn finish_error(&self, message: &str) {
        self.bar.finish_with_message(format!("❌ {message}"));
    }

    /// Finish and clear the progress indicator
    pub fn finish_clear(&self) {
        self.bar.finish_and_clear();
    }
}

/// Utility functions for interactive setup workflows
pub struct SetupHelper;

impl SetupHelper {
    /// Validate Azure subscription ID format
    pub fn validate_subscription_id(subscription_id: &str) -> std::result::Result<(), String> {
        if subscription_id.trim().is_empty() {
            return Err("Subscription ID cannot be empty".to_string());
        }
        
        // Basic GUID format validation
        let guid_pattern = regex::Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
            .map_err(|_| "Invalid regex pattern".to_string())?;
        
        if !guid_pattern.is_match(subscription_id.trim()) {
            return Err("Subscription ID must be a valid GUID format".to_string());
        }
        
        Ok(())
    }

    /// Validate resource group name
    pub fn validate_resource_group_name(name: &str) -> std::result::Result<(), String> {
        let name = name.trim();
        
        if name.is_empty() {
            return Err("Resource group name cannot be empty".to_string());
        }
        
        if name.len() > 90 {
            return Err("Resource group name cannot exceed 90 characters".to_string());
        }
        
        if name.starts_with('.') || name.starts_with('-') || name.ends_with('.') || name.ends_with('-') {
            return Err("Resource group name cannot start or end with '.' or '-'".to_string());
        }
        
        // Check for valid characters and no consecutive periods/hyphens
        let mut prev_char = ' ';
        for ch in name.chars() {
            if !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-' && ch != '_' {
                return Err("Resource group name can only contain alphanumeric characters, periods, hyphens, and underscores".to_string());
            }
            if (ch == '.' && prev_char == '.') || (ch == '-' && prev_char == '-') {
                return Err("Resource group name cannot contain consecutive periods or hyphens".to_string());
            }
            prev_char = ch;
        }
        
        Ok(())
    }

    /// Validate vault name according to Azure Key Vault requirements
    pub fn validate_vault_name(name: &str) -> std::result::Result<(), String> {
        let name = name.trim();
        
        if name.is_empty() {
            return Err("Vault name cannot be empty".to_string());
        }
        
        if name.len() < 3 || name.len() > 24 {
            return Err("Vault name must be between 3 and 24 characters".to_string());
        }
        
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err("Vault name can only contain alphanumeric characters and hyphens".to_string());
        }
        
        if name.starts_with('-') || name.ends_with('-') {
            return Err("Vault name cannot start or end with a hyphen".to_string());
        }
        
        if name.contains("--") {
            return Err("Vault name cannot contain consecutive hyphens".to_string());
        }
        
        // Must start with a letter
        if !name.chars().next().unwrap_or(' ').is_ascii_alphabetic() {
            return Err("Vault name must start with a letter".to_string());
        }
        
        Ok(())
    }

    /// Generate a default vault name based on username and current time
    pub fn generate_default_vault_name() -> String {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_string())
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        
        let timestamp = chrono::Utc::now().format("%m%d").to_string();
        format!("kv-{username}-{timestamp}")
    }

    /// Generate a default resource group name
    pub fn generate_default_resource_group() -> String {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_string())
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        
        format!("rg-{username}-keyvaults")
    }

    /// Generate a default storage account name
    pub fn generate_storage_account_name() -> String {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_string())
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        
        let timestamp = chrono::Utc::now().format("%m%d%H%M").to_string();
        let generated_name = format!("st{username}{timestamp}");
        
        // Ensure it's not too long (max 24 characters)
        if generated_name.len() > 24 {
            format!("st{username}{timestamp}")
        } else {
            generated_name
        }
    }

    /// Validate storage account name according to Azure Storage requirements
    pub fn validate_storage_account_name(name: &str) -> std::result::Result<(), String> {
        let name = name.trim();
        
        if name.is_empty() {
            return Err("Storage account name cannot be empty".to_string());
        }
        
        if name.len() < 3 || name.len() > 24 {
            return Err("Storage account name must be between 3 and 24 characters".to_string());
        }
        
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err("Storage account name can only contain lowercase letters and numbers".to_string());
        }
        
        if !name.chars().next().unwrap_or(' ').is_ascii_alphabetic() {
            return Err("Storage account name must start with a letter".to_string());
        }
        
        Ok(())
    }

    /// Validate container name according to Azure Storage requirements
    pub fn validate_container_name(name: &str) -> std::result::Result<(), String> {
        let name = name.trim();
        
        if name.is_empty() {
            return Err("Container name cannot be empty".to_string());
        }
        
        if name.len() < 3 || name.len() > 63 {
            return Err("Container name must be between 3 and 63 characters".to_string());
        }
        
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err("Container name can only contain lowercase letters, numbers, and hyphens".to_string());
        }
        
        if name.starts_with('-') || name.ends_with('-') {
            return Err("Container name cannot start or end with a hyphen".to_string());
        }
        
        if name.contains("--") {
            return Err("Container name cannot contain consecutive hyphens".to_string());
        }
        
        if !name.chars().next().unwrap_or(' ').is_ascii_alphabetic() {
            return Err("Container name must start with a letter".to_string());
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_subscription_id() {
        // Valid GUID
        assert!(SetupHelper::validate_subscription_id("12345678-1234-1234-1234-123456789012").is_ok());
        
        // Invalid format
        assert!(SetupHelper::validate_subscription_id("not-a-guid").is_err());
        assert!(SetupHelper::validate_subscription_id("").is_err());
        assert!(SetupHelper::validate_subscription_id("12345678-1234-1234-1234-12345678901").is_err());
    }

    #[test]
    fn test_validate_resource_group_name() {
        // Valid names
        assert!(SetupHelper::validate_resource_group_name("my-resource-group").is_ok());
        assert!(SetupHelper::validate_resource_group_name("rg_test_123").is_ok());
        
        // Invalid names
        assert!(SetupHelper::validate_resource_group_name("").is_err());
        assert!(SetupHelper::validate_resource_group_name(".invalid").is_err());
        assert!(SetupHelper::validate_resource_group_name("invalid.").is_err());
        assert!(SetupHelper::validate_resource_group_name("invalid--name").is_err());
        assert!(SetupHelper::validate_resource_group_name(&"a".repeat(91)).is_err());
    }

    #[test]
    fn test_validate_vault_name() {
        // Valid names
        assert!(SetupHelper::validate_vault_name("myvault123").is_ok());
        assert!(SetupHelper::validate_vault_name("my-vault").is_ok());
        
        // Invalid names
        assert!(SetupHelper::validate_vault_name("").is_err());
        assert!(SetupHelper::validate_vault_name("ab").is_err());
        assert!(SetupHelper::validate_vault_name(&"a".repeat(25)).is_err());
        assert!(SetupHelper::validate_vault_name("-invalid").is_err());
        assert!(SetupHelper::validate_vault_name("invalid-").is_err());
        assert!(SetupHelper::validate_vault_name("invalid--name").is_err());
        assert!(SetupHelper::validate_vault_name("123vault").is_err());
    }

    #[test]
    fn test_generate_default_names() {
        let vault_name = SetupHelper::generate_default_vault_name();
        assert!(vault_name.starts_with("kv-"));
        assert!(SetupHelper::validate_vault_name(&vault_name).is_ok());
        
        let rg_name = SetupHelper::generate_default_resource_group();
        assert!(rg_name.starts_with("rg-"));
        assert!(rg_name.ends_with("-keyvaults"));
        assert!(SetupHelper::validate_resource_group_name(&rg_name).is_ok());
    }
}