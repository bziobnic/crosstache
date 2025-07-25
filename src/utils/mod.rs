//! Utility functions module
//!
//! This module contains various utility functions including name sanitization,
//! retry logic, connection string parsing, table formatting, and other helpers.

pub mod azure_detect;
pub mod format;
pub mod helpers;
pub mod interactive;
pub mod network;
pub mod retry;
pub mod sanitizer;

pub use azure_detect::*;
pub use format::{DisplayUtils, FormattableOutput, OutputFormat, TableFormatter, TemplateError, ColorTheme};
pub use helpers::*;
pub use interactive::{InteractivePrompt, ProgressIndicator, SetupHelper};
pub use network::*;
pub use retry::*;
pub use sanitizer::*;
