//! Authentication module for Azure services
//!
//! This module re-exports from `backend::azure::auth` for backward
//! compatibility. New code should import directly from
//! `crate::backend::azure::auth`.

/// Re-export the Azure auth provider module at the legacy path so that
/// existing `use crate::auth::provider::*` statements continue to work.
pub mod provider {
    pub use crate::backend::azure::auth::*;
}
