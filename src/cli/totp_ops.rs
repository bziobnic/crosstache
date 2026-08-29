use crate::backend::BackendRegistry;
use crate::cli::helpers::{
    copy_to_clipboard, resolve_workspace_or_default, schedule_clipboard_clear, use_trait_path,
};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::records::{parse_sensitive_envelope, FIELD_TAG_PREFIX, RECORD_CONTENT_TYPE};
use crate::totp::{generate_current, GeneratedTotp, DEFAULT_TOTP_FIELD};
use crate::utils::output;
use crate::workspace::TargetMode;
use std::collections::HashMap;
use std::io::Write;
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
struct ClipboardPolicy {
    message: String,
    clear_after_seconds: Option<u64>,
}

fn clipboard_policy(
    name: &str,
    expires_in_seconds: u64,
    clipboard_timeout: u64,
) -> ClipboardPolicy {
    if clipboard_timeout == 0 {
        return ClipboardPolicy {
            message: format!(
                "TOTP code for '{name}' copied to clipboard (expires in {expires_in_seconds}s)"
            ),
            clear_after_seconds: None,
        };
    }
    let clear_after_seconds = clipboard_timeout.min(expires_in_seconds);
    ClipboardPolicy {
        message: format!(
            "TOTP code for '{name}' copied to clipboard (expires in {expires_in_seconds}s; clipboard clears in {clear_after_seconds}s)"
        ),
        clear_after_seconds: Some(clear_after_seconds),
    }
}

fn write_raw(writer: &mut impl Write, code: &str) -> std::io::Result<()> {
    write!(writer, "{code}")
}

fn present_to_clipboard<C, S, F, L>(
    name: &str,
    generated: &GeneratedTotp,
    clipboard_timeout: u64,
    copy: C,
    on_success: S,
    on_failure: F,
    schedule_clear: L,
) where
    C: FnOnce(&str) -> std::result::Result<(), String>,
    S: FnOnce(&str),
    F: FnOnce(&str, &str),
    L: FnOnce(u64),
{
    match copy(generated.code.as_str()) {
        Ok(()) => {
            let policy = clipboard_policy(name, generated.expires_in_seconds, clipboard_timeout);
            on_success(&policy.message);
            if let Some(seconds) = policy.clear_after_seconds {
                schedule_clear(seconds);
            }
        }
        Err(error) => on_failure(
            &format!("Failed to copy TOTP code to clipboard: {error}"),
            "Use '--raw' to print the TOTP code to stdout instead.",
        ),
    }
}

fn extract_totp_material(
    name: &str,
    content_type: &str,
    value: Option<&str>,
    tags: &HashMap<String, String>,
    field: &str,
) -> Result<Zeroizing<String>> {
    if content_type != RECORD_CONTENT_TYPE {
        return Err(CrosstacheError::config(format!(
            "secret '{name}' is not a typed record; TOTP fields must be encrypted record fields"
        )));
    }
    let raw = value
        .ok_or_else(|| CrosstacheError::config(format!("secret '{name}' has no record value")))?;
    let mut envelope = parse_sensitive_envelope(raw).map_err(|_| {
        CrosstacheError::config(format!(
            "secret '{name}' has an invalid record envelope: expected a JSON object of strings"
        ))
    })?;
    if let Some(material) = envelope.remove(field) {
        return Ok(material);
    }
    if tags.contains_key(&format!("{FIELD_TAG_PREFIX}{field}")) {
        return Err(CrosstacheError::config(format!(
            "field '{field}' of secret '{name}' is listable metadata, not encrypted secret material; move it into the record envelope with 'xv update {name} --field-secret {field}=<seed>'"
        )));
    }
    let mut known: Vec<String> = envelope.keys().cloned().collect();
    known.extend(
        tags.keys()
            .filter_map(|key| key.strip_prefix(FIELD_TAG_PREFIX).map(str::to_string)),
    );
    known.sort();
    known.dedup();
    let known = if known.is_empty() {
        "none".to_string()
    } else {
        known.join(", ")
    };
    Err(CrosstacheError::config(format!(
        "secret '{name}' has no field '{field}'. Known fields: {known}"
    )))
}

pub(crate) async fn execute_totp(
    name: &str,
    field: Option<&str>,
    raw: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    if !use_trait_path(registry) {
        return Err(CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        ));
    }

    let (backend, _backend_name, vault_name, resolved_name) =
        resolve_workspace_or_default(name, &config, TargetMode::Read).await?;
    let secret = backend
        .secrets()
        .get_secret(&vault_name, &resolved_name, true)
        .await?;
    let field = field.unwrap_or(DEFAULT_TOTP_FIELD);
    let material = extract_totp_material(
        &resolved_name,
        &secret.content_type,
        secret.value.as_ref().map(|value| value.as_str()),
        &secret.tags,
        field,
    )?;
    let generated = generate_current(material.as_str())?;

    if raw {
        let mut stdout = std::io::stdout().lock();
        write_raw(&mut stdout, generated.code.as_str())?;
        return Ok(());
    }

    present_to_clipboard(
        name,
        &generated,
        config.clipboard_timeout,
        copy_to_clipboard,
        output::success,
        |warning, hint| {
            output::warn(warning);
            eprintln!("{hint}");
        },
        schedule_clipboard_clear,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tags(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn extracts_only_an_encrypted_envelope_field() {
        let material = extract_totp_material(
            "github",
            RECORD_CONTENT_TYPE,
            Some(r#"{"password":"pw","one-time-code":"SECRET-SEED"}"#),
            &HashMap::new(),
            DEFAULT_TOTP_FIELD,
        )
        .unwrap();
        assert_eq!(material.as_str(), "SECRET-SEED");
    }

    #[test]
    fn rejects_metadata_field_with_storage_repair_hint() {
        let error = extract_totp_material(
            "github",
            RECORD_CONTENT_TYPE,
            Some(r#"{"password":"pw"}"#),
            &tags(&[("f.one-time-code", "SECRET-SEED")]),
            DEFAULT_TOTP_FIELD,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--field-secret"), "{message}");
        assert!(!message.contains("SECRET-SEED"), "{message}");
    }

    #[test]
    fn rejects_untyped_and_missing_fields_without_values() {
        let untyped = extract_totp_material(
            "github",
            "text/plain",
            Some("SECRET-SEED"),
            &HashMap::new(),
            DEFAULT_TOTP_FIELD,
        )
        .unwrap_err()
        .to_string();
        assert!(untyped.contains("typed record"), "{untyped}");
        assert!(!untyped.contains("SECRET-SEED"), "{untyped}");

        let missing = extract_totp_material(
            "github",
            RECORD_CONTENT_TYPE,
            Some(r#"{"password":"pw","recovery":"value"}"#),
            &tags(&[("f.username", "alice")]),
            DEFAULT_TOTP_FIELD,
        )
        .unwrap_err()
        .to_string();
        assert!(
            missing.contains("password, recovery, username"),
            "{missing}"
        );
        assert!(!missing.contains("value"), "{missing}");
    }

    #[test]
    fn clipboard_policy_caps_clear_at_expiry() {
        assert_eq!(
            clipboard_policy("github", 17, 30),
            ClipboardPolicy {
                message: "TOTP code for 'github' copied to clipboard (expires in 17s; clipboard clears in 17s)".into(),
                clear_after_seconds: Some(17),
            }
        );
        assert_eq!(
            clipboard_policy("github", 17, 5).clear_after_seconds,
            Some(5)
        );
        assert_eq!(
            clipboard_policy("github", 17, 17).clear_after_seconds,
            Some(17)
        );
    }

    #[test]
    fn clipboard_policy_respects_disabled_clearing() {
        assert_eq!(
            clipboard_policy("github", 17, 0),
            ClipboardPolicy {
                message: "TOTP code for 'github' copied to clipboard (expires in 17s)".into(),
                clear_after_seconds: None,
            }
        );
    }

    #[test]
    fn clipboard_failure_never_presents_code_or_schedules_clear() {
        let code = "731904";
        let generated = GeneratedTotp {
            code: Zeroizing::new(code.to_string()),
            expires_in_seconds: 17,
        };
        let mut successes = Vec::new();
        let mut failures = Vec::new();
        let mut scheduled = Vec::new();

        present_to_clipboard(
            "github",
            &generated,
            30,
            |_| Err("clipboard unavailable".to_string()),
            |message| successes.push(message.to_string()),
            |warning, hint| failures.push((warning.to_string(), hint.to_string())),
            |seconds| scheduled.push(seconds),
        );

        assert!(successes.is_empty());
        assert!(scheduled.is_empty());
        assert_eq!(failures.len(), 1);
        let (warning, hint) = &failures[0];
        assert!(warning.contains("Failed to copy TOTP code to clipboard"));
        assert!(hint.contains("--raw"));
        assert!(!warning.contains(code), "{warning}");
        assert!(!hint.contains(code), "{hint}");
    }

    #[test]
    fn raw_writer_emits_only_code_without_newline() {
        let mut bytes = Vec::new();
        write_raw(&mut bytes, "012345").unwrap();
        assert_eq!(bytes, b"012345");
    }
}
