//! AWS Secrets Manager backend.
//!
//! Phase 3 / v0.10. See `docs/superpowers/specs/2026-05-09-aws-backend-phase-3-design.md`.

pub mod auth;
pub mod config;
pub mod encoding;
pub mod errors;
pub mod metadata;
pub mod models;
pub mod secrets;
pub mod vaults;

// AwsBackend struct fleshed out in Task 12.
