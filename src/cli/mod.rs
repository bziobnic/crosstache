//! CLI module for crosstache
//!
//! This module contains all command-line interface related functionality,
//! including command definitions, argument parsing, and command execution.

pub mod commands;
#[cfg(feature = "file-ops")]
pub mod file;
pub(crate) mod helpers;

pub use commands::*;
