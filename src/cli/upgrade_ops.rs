//! Self-update command: check for and install new versions of xv.

use crate::error::Result;

/// Check for and optionally install a new version of xv.
pub(crate) async fn execute_upgrade_command(_check: bool, _force: bool) -> Result<()> {
    todo!("upgrade command not yet implemented")
}
