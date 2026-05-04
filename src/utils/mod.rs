//! Utility functions module
//!
//! This module contains various utility functions including name sanitization,
//! retry logic, connection string parsing, table formatting, and other helpers.

/// Re-export Azure detection at the legacy path for backward compatibility.
/// New code should import from `crate::backend::azure::detect`.
pub mod azure_detect {
    pub use crate::backend::azure::detect::*;
}
pub mod datetime;
pub mod error_hints;
pub mod format;
pub mod fuzzy;
pub mod helpers;
pub mod interactive;
pub mod network;
pub mod output;
pub mod pager;
pub mod pagination;
pub mod progress;
pub mod resource_detector;
pub mod retry;
pub mod sanitizer;
pub mod suggestions;
pub mod url_helpers;
