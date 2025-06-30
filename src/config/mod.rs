//! Configuration management module
//! 
//! This module handles configuration loading, validation, and persistence
//! from multiple sources including command-line arguments, environment variables,
//! configuration files, and default values.

pub mod settings;
pub mod context;

pub use settings::*;
pub use context::*;