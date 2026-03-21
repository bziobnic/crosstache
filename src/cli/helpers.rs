//! Shared CLI helper functions (clipboard, token parsing, random generation, formatting).

use crate::cli::commands::CharsetType;
use crate::error::{CrosstacheError, Result};
use zeroize::Zeroizing;

/// Parse a single key-value pair from `KEY=value` format.
pub(crate) fn parse_key_val<T, U>(
    s: &str,
) -> std::result::Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

pub(crate) fn format_cache_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Claims extracted from a JWT token.
pub(crate) struct TokenClaims {
    pub tenant_id: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub object_id: Option<String>,
}

/// Decode a JWT payload and extract identity claims.
pub(crate) fn extract_claims_from_token(token: &str) -> Result<TokenClaims> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(CrosstacheError::authentication("Invalid JWT token format"));
    }

    let payload = parts[1];
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    // Azure AD JWT tokens use base64url encoding (RFC 4648 §5), no padding
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| CrosstacheError::authentication("Failed to decode token payload"))?;

    let payload_str = String::from_utf8(decoded)
        .map_err(|_| CrosstacheError::authentication("Invalid UTF-8 in token payload"))?;

    let v: serde_json::Value = serde_json::from_str(&payload_str)
        .map_err(|_| CrosstacheError::authentication("Invalid JSON in token payload"))?;

    // Azure AD tokens use different claim names depending on token version:
    //   v1: unique_name, upn    v2: preferred_username, email
    let email = v["email"]
        .as_str()
        .or_else(|| v["preferred_username"].as_str())
        .or_else(|| v["upn"].as_str())
        .or_else(|| v["unique_name"].as_str())
        .map(String::from);

    let name = v["name"].as_str().map(String::from);

    let tenant_id = v["tid"].as_str().map(String::from);

    let object_id = v["oid"].as_str().map(String::from);

    Ok(TokenClaims {
        tenant_id,
        name,
        email,
        object_id,
    })
}

/// Copy text to the system clipboard.
///
/// On Linux (X11/Wayland), `arboard` clipboard content is lost when the process exits
/// because the clipboard is owned by the process. We use external tools (`wl-copy`,
/// `xclip`, `xsel`) which fork a daemon to hold the selection, so clipboard content
/// persists. Falls back to `arboard` on other platforms or if no tool is available.
pub(crate) fn copy_to_clipboard(text: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(result) = linux_clipboard_copy(text) {
            return result;
        }
        // No external tool found — fall back to arboard with a warning
        eprintln!(
            "hint: Install xclip, xsel, or wl-clipboard for reliable clipboard support on Linux."
        );
    }

    // macOS, Windows, and Linux fallback
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Failed to access clipboard: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("Failed to copy to clipboard: {e}"))
}

/// Try to copy to clipboard using a Linux external tool.
/// Returns `Some(Ok(()))` on success, `Some(Err(...))` if a tool was found but failed,
/// or `None` if no suitable tool is available.
#[cfg(target_os = "linux")]
fn linux_clipboard_copy(text: &str) -> Option<std::result::Result<(), String>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();

    // Ordered list of clipboard commands to try
    let candidates: &[(&str, &[&str])] = if is_wayland {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("wl-copy", &[]),
        ]
    };

    for &(cmd, args) in candidates {
        if Command::new("which")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            let child = Command::new(cmd)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn();

            match child {
                Ok(mut child) => {
                    if let Some(mut stdin) = child.stdin.take() {
                        if let Err(e) = stdin.write_all(text.as_bytes()) {
                            return Some(Err(format!("{cmd}: failed to write to stdin: {e}")));
                        }
                        drop(stdin);
                    }
                    match child.wait() {
                        Ok(status) if status.success() => return Some(Ok(())),
                        Ok(status) => {
                            return Some(Err(format!("{cmd} exited with {status}")));
                        }
                        Err(e) => {
                            return Some(Err(format!("{cmd}: {e}")));
                        }
                    }
                }
                Err(_) => continue, // tool exists but failed to spawn, try next
            }
        }
    }

    None // no suitable tool found
}

/// Spawn a detached child process that clears the clipboard after `seconds`.
/// The child outlives the parent process, fixing the issue where std::thread::spawn
/// would be killed when the CLI exits.
pub(crate) fn schedule_clipboard_clear(seconds: u64) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("sleep {seconds} && printf '' | pbcopy")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
        let cmd = if is_wayland {
            format!(
                "sleep {seconds} && \
                 (wl-copy --clear 2>/dev/null || \
                  xclip -selection clipboard < /dev/null 2>/dev/null || \
                  xsel --clipboard --delete 2>/dev/null || true)"
            )
        } else {
            format!(
                "sleep {seconds} && \
                 (xclip -selection clipboard < /dev/null 2>/dev/null || \
                  xsel --clipboard --delete 2>/dev/null || \
                  wl-copy --clear 2>/dev/null || true)"
            )
        };
        let _ = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW prevents the child from inheriting the parent console,
        // which would otherwise cause the terminal to close when PowerShell detaches.
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let cmd = format!("Start-Sleep -Seconds {seconds}; Set-Clipboard ''");
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &cmd])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

/// Generate a random value using the specified parameters
pub(crate) fn generate_random_value(
    length: usize,
    charset: CharsetType,
    custom_generator: Option<String>,
) -> Result<Zeroizing<String>> {
    use rand::prelude::*;

    if let Some(generator_script) = custom_generator {
        // Execute custom generator script
        return execute_custom_generator(&generator_script, length).map(Zeroizing::new);
    }

    if length == 0 {
        return Err(CrosstacheError::invalid_argument(
            "Length must be greater than 0",
        ));
    }

    let charset_str = charset.chars();
    let charset_bytes = charset_str.as_bytes();

    if charset_bytes.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "Character set cannot be empty",
        ));
    }

    let mut rng = thread_rng();
    let random_value: String = (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..charset_bytes.len());
            charset_bytes[idx] as char
        })
        .collect();

    Ok(Zeroizing::new(random_value))
}

/// Execute a custom generator script
fn execute_custom_generator(script_path: &str, length: usize) -> Result<String> {
    use std::process::{Command, Stdio};

    let script = std::path::Path::new(script_path);

    // Check if the script exists
    if !script.exists() {
        return Err(CrosstacheError::config(format!(
            "Generator script not found: {}",
            script_path
        )));
    }

    // Security: validate script ownership and permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(script)
            .map_err(|e| CrosstacheError::config(format!("Cannot read script metadata: {e}")))?;
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid && meta.uid() != 0 {
            return Err(CrosstacheError::config(format!(
                "Generator script '{}' is not owned by you or root — refusing to execute",
                script_path
            )));
        }
        if meta.mode() & 0o002 != 0 {
            return Err(CrosstacheError::config(format!(
                "Generator script '{}' is world-writable — refusing to execute (chmod o-w to fix)",
                script_path
            )));
        }
    }

    // Set up environment for the script
    let mut cmd = Command::new(script_path);
    cmd.env("XV_SECRET_LENGTH", length.to_string());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Execute the script
    let output = cmd.output().map_err(|e| {
        CrosstacheError::config(format!(
            "Failed to execute generator script '{}': {}",
            script_path, e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CrosstacheError::config(format!(
            "Generator script failed with exit code {}: {}",
            output.status.code().unwrap_or(-1),
            stderr
        )));
    }

    let generated_value = String::from_utf8(output.stdout)
        .map_err(|e| {
            CrosstacheError::config(format!("Generator script output is not valid UTF-8: {}", e))
        })?
        .trim()
        .to_string();

    if generated_value.is_empty() {
        return Err(CrosstacheError::config(
            "Generator script produced empty output",
        ));
    }

    Ok(generated_value)
}

/// Mask secret values in text output
pub(crate) fn mask_secrets(text: &str, secrets: &[Zeroizing<String>]) -> String {
    let mut result = text.to_string();

    for secret in secrets {
        if secret.len() >= 4 {
            // Only mask secrets that are at least 4 characters
            // Replace with [MASKED] to indicate redaction
            result = result.replace(secret.as_str(), "[MASKED]");
        }
    }

    result
}
