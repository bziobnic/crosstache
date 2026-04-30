//! Pre-commit leak scanner.
//!
//! See `docs/scan.md` for the user-facing contract.

pub mod engine;
pub mod finding;
pub mod orchestrator;
pub mod patterns;
pub mod walker;
