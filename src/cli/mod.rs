//! CLI module for crosstache
//!
//! This module contains all command-line interface related functionality,
//! including command definitions, argument parsing, and command execution.

pub mod commands;
#[cfg(feature = "file-ops")]
pub mod file;
#[cfg(feature = "file-ops")]
pub mod file_ops;
pub(crate) mod config_ops;
pub(crate) mod helpers;
pub(crate) mod secret_ops;
pub(crate) mod system_ops;
pub(crate) mod upgrade_ops;
pub(crate) mod vault_ops;

pub use commands::*;
