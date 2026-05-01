//! Read-only Terminal UI for crosstache. Feature-gated on `tui`.
//! See `docs/tui.md` for the user-facing contract.

use crate::config::Config;
use crate::error::Result;

/// Entrypoint. Sets up the terminal, runs the event loop, restores
/// the terminal on exit. Filled in across Tasks 2-11.
pub async fn run_tui(_config: Config) -> Result<()> {
    Ok(())
}
