//! Move (rename/relocate) command parsing and validation.
//!
//! Grammar (spec 2026-07-02-fs-verbs): a trailing `/` marks a folder;
//! `/` alone is the vault root; otherwise the last segment is the secret
//! name and everything before it the folder. A bare destination therefore
//! means "vault root + rename".

use crate::error::{CrosstacheError, Result};

/// Plan for moving secrets or folders.
#[allow(dead_code)] // wired up by the mv executor (next task) — remove this attribute there
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MvPlan {
    /// Move a single secret.
    Secret {
        src_folder: Option<String>,
        src_name: String,
        dest_folder: Option<String>,
        dest_name: String,
    },
    /// Move (rename) a folder.
    Folder {
        src_prefix: String,
        dest_prefix: Option<String>,
    },
}

/// Parse the `xv mv` operands into a move plan.
///
/// Grammar (spec 2026-07-02-fs-verbs): a trailing `/` marks a folder;
/// `/` alone is the vault root; otherwise the last segment is the secret
/// name and everything before it the folder. A bare destination therefore
/// means "vault root + rename".
#[allow(dead_code)] // wired up by the mv executor (next task) — remove this attribute there
pub(crate) fn parse_mv(source: &str, dest: &str) -> Result<MvPlan> {
    let source = source.trim();
    let dest = dest.trim();
    if source.is_empty() || dest.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "mv requires a SOURCE and a DEST",
        ));
    }

    let src_is_folder = source == "/" || source.ends_with('/');
    if src_is_folder {
        let src_prefix = source.trim_matches('/');
        if src_prefix.is_empty() {
            return Err(CrosstacheError::invalid_argument(
                "moving the vault root is not supported; name a folder (e.g. 'app/')",
            ));
        }
        if src_prefix.split('/').any(str::is_empty) {
            return Err(CrosstacheError::invalid_argument(format!(
                "invalid source folder '{source}'"
            )));
        }
        let dest_prefix = if dest == "/" {
            None
        } else if let Some(stripped) = dest.strip_suffix('/') {
            let p = stripped.trim_start_matches('/');
            if p.is_empty() || p.split('/').any(str::is_empty) {
                return Err(CrosstacheError::invalid_argument(format!(
                    "invalid destination folder '{dest}'"
                )));
            }
            Some(p.to_string())
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "folder moves require a folder destination ending in / (got '{dest}'); \
                 did you mean '{dest}/'?"
            )));
        };
        return Ok(MvPlan::Folder {
            src_prefix: src_prefix.to_string(),
            dest_prefix,
        });
    }

    let (src_folder, src_name) = split_secret_path(source)?;
    let (dest_folder, dest_name) = if dest == "/" {
        (None, src_name.clone())
    } else if let Some(stripped) = dest.strip_suffix('/') {
        let p = stripped.trim_start_matches('/');
        if p.is_empty() || p.split('/').any(str::is_empty) {
            return Err(CrosstacheError::invalid_argument(format!(
                "invalid destination folder '{dest}'"
            )));
        }
        (Some(p.to_string()), src_name.clone())
    } else {
        split_secret_path(dest)?
    };

    Ok(MvPlan::Secret {
        src_folder,
        src_name,
        dest_folder,
        dest_name,
    })
}

/// Split `folder/name` (no trailing slash): last segment = name, the rest =
/// folder (`None` at the root). Leading `/` is tolerated (`/x` == `x`).
fn split_secret_path(path: &str) -> Result<(Option<String>, String)> {
    let path = path.trim_start_matches('/');
    let (folder, name) = match path.rsplit_once('/') {
        Some((f, n)) => (Some(f), n),
        None => (None, path),
    };
    if name.is_empty() || folder.is_some_and(|f| f.is_empty() || f.split('/').any(str::is_empty)) {
        return Err(CrosstacheError::invalid_argument(format!(
            "invalid secret path '{path}'"
        )));
    }
    Ok((folder.map(String::from), name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(sf: Option<&str>, sn: &str, df: Option<&str>, dn: &str) -> MvPlan {
        MvPlan::Secret {
            src_folder: sf.map(String::from),
            src_name: sn.into(),
            dest_folder: df.map(String::from),
            dest_name: dn.into(),
        }
    }

    #[test]
    fn grammar_table() {
        // (source, dest, expected) — every row of the spec grammar table.
        let cases = [
            (
                "db/pass",
                "app/",
                secret(Some("db"), "pass", Some("app"), "pass"),
            ),
            (
                "db/pass",
                "app/pw",
                secret(Some("db"), "pass", Some("app"), "pw"),
            ),
            (
                "db/pass",
                "newname",
                secret(Some("db"), "pass", None, "newname"),
            ),
            ("app/pass", "/", secret(Some("app"), "pass", None, "pass")),
            ("pass", "app/", secret(None, "pass", Some("app"), "pass")),
            (
                "a/b/pass",
                "x/y/",
                secret(Some("a/b"), "pass", Some("x/y"), "pass"),
            ),
            (
                "app/",
                "svc/",
                MvPlan::Folder {
                    src_prefix: "app".into(),
                    dest_prefix: Some("svc".into()),
                },
            ),
            (
                "app/",
                "/",
                MvPlan::Folder {
                    src_prefix: "app".into(),
                    dest_prefix: None,
                },
            ),
            (
                "app/db/",
                "svc/",
                MvPlan::Folder {
                    src_prefix: "app/db".into(),
                    dest_prefix: Some("svc".into()),
                },
            ),
        ];
        for (src, dst, want) in cases {
            let got = parse_mv(src, dst).unwrap_or_else(|e| panic!("mv {src} {dst}: {e}"));
            assert_eq!(got, want, "mv {src} {dst}");
        }
    }

    #[test]
    fn grammar_errors() {
        // Folder source requires a folder destination.
        let e = parse_mv("app/", "svc").unwrap_err().to_string();
        assert!(e.contains("ending in /"), "{e}");
        // Vault root is not a movable source.
        assert!(parse_mv("/", "svc/").is_err());
        // Empty operands.
        assert!(parse_mv("", "x").is_err());
        assert!(parse_mv("x", "").is_err());
        assert!(parse_mv("x", "   ").is_err());
        // Destination that is only a slashless empty name after a folder.
        assert!(parse_mv("db/pass", "app//").is_err());
        // Folder source with an internal empty segment.
        assert!(parse_mv("app//db/", "svc/").is_err());
    }
}
