//! CLI module for crosstache
//!
//! This module contains all command-line interface related functionality,
//! including command definitions, argument parsing, and command execution.

pub mod commands;
pub(crate) mod helpers;
#[cfg(feature = "file-ops")]
pub mod file;

pub use commands::*;
