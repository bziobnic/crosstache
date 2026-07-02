//! Utility functions module
//!
//! This module contains various utility functions including name sanitization,
//! retry logic, connection string parsing, table formatting, and other helpers.

/// Maximum number of pagination pages to follow before aborting to prevent infinite loops.
pub const MAX_PAGES: usize = 1000;

/// Maximum number of bytes accepted from a single API response body.
pub const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

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
pub mod list_output;
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
