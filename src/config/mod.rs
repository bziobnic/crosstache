//! Configuration management module
//!
//! This module handles configuration loading, validation, and persistence
//! from multiple sources including command-line arguments, environment variables,
//! configuration files, and default values.

pub mod backend_ops;
pub mod context;
pub mod doctor;
pub mod init;
pub mod project;
pub mod settings;
pub mod setup;

pub use context::*;
pub use settings::*;

/// `sha256:<lowercase hex>` over the exact bytes given.
///
/// Used to pin the precise on-disk inputs (global config, `.xv.toml`,
/// context file) that produced a resolved scheduled-rotation target, so a
/// later unattended run can detect that one of them changed. Always digest
/// the same buffer that was parsed — never re-read the file — so a
/// concurrent edit cannot slip between the parse and the digest.
pub(crate) fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}
