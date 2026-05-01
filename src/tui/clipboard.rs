//! TUI-flavored clipboard write. Wraps the existing crate clipboard
//! helper; converts its String error into CrosstacheError.

use crate::error::{CrosstacheError, Result};

pub fn copy_string(value: &str) -> Result<()> {
    crate::cli::helpers::copy_to_clipboard(value)
        .map_err(|e| CrosstacheError::config(format!("clipboard: {e}")))
}
