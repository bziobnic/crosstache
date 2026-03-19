//! Blob sync helpers: change detection and local path mapping for `xv file sync`.

use crate::blob::models::FileInfo;
use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};

/// Clock skew / filesystem rounding tolerance for comparing mtimes (seconds).
pub const SYNC_MTIME_EPSILON_SECS: i64 = 2;

/// Whether local file metadata matches remote closely enough to skip a transfer.
#[must_use]
pub fn is_unchanged(local_size: u64, local_mtime: DateTime<Utc>, remote: &FileInfo) -> bool {
    if local_size != remote.size {
        return false;
    }
    let diff = (local_mtime - remote.last_modified).num_seconds().abs();
    diff <= SYNC_MTIME_EPSILON_SECS
}

#[must_use]
fn local_mtime_clearly_newer(local_mtime: DateTime<Utc>, remote: &FileInfo) -> bool {
    local_mtime > remote.last_modified + Duration::seconds(SYNC_MTIME_EPSILON_SECS)
}

#[must_use]
fn remote_mtime_clearly_newer(local_mtime: DateTime<Utc>, remote: &FileInfo) -> bool {
    remote.last_modified > local_mtime + Duration::seconds(SYNC_MTIME_EPSILON_SECS)
}

/// Resolve bidirectional sync when the blob exists both locally and remotely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BothAction {
    Upload,
    Download,
    Skip,
}

#[must_use]
pub fn resolve_both(local_size: u64, local_mtime: DateTime<Utc>, remote: &FileInfo) -> BothAction {
    if is_unchanged(local_size, local_mtime, remote) {
        return BothAction::Skip;
    }
    if local_mtime_clearly_newer(local_mtime, remote) {
        BothAction::Upload
    } else if remote_mtime_clearly_newer(local_mtime, remote) {
        BothAction::Download
    } else if local_mtime >= remote.last_modified {
        // Ambiguous (e.g. size differs but mtimes within epsilon): prefer newer mtime, then upload.
        BothAction::Upload
    } else {
        BothAction::Download
    }
}

/// Map a blob name back to a local path under `base_path`, inverting upload path rules
/// when given the same optional `prefix` used during upload (`path_to_blob_name` in the CLI).
#[must_use]
pub fn local_path_from_blob(base_path: &Path, prefix: Option<&str>, blob_name: &str) -> PathBuf {
    let rel = match prefix.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => {
            let p = p.trim_matches('/');
            let head = format!("{p}/");
            blob_name.strip_prefix(&head).unwrap_or(blob_name)
        }
        None => blob_name,
    };
    rel.split('/')
        .fold(base_path.to_path_buf(), |acc, c| acc.join(c))
}

/// Longest shared prefix ending with `/` across blob names, if any.
/// Used to scope `--delete` when `--prefix` is not set (only under this tree).
#[must_use]
pub fn common_directory_prefix(names: &std::collections::HashSet<String>) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut iter = names.iter();
    let first = iter.next()?.as_str();
    let mut prefix = first.to_string();
    for n in iter {
        while !n.starts_with(&prefix) {
            if prefix.is_empty() {
                return None;
            }
            prefix.pop();
        }
    }
    prefix.rfind('/').map(|i| prefix[..=i].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn remote(size: u64, last_modified: DateTime<Utc>) -> FileInfo {
        FileInfo {
            name: "x".to_string(),
            size,
            content_type: "application/octet-stream".to_string(),
            last_modified,
            etag: "e".to_string(),
            groups: vec![],
            metadata: Default::default(),
            tags: Default::default(),
        }
    }

    #[test]
    fn unchanged_same_size_close_mtime() {
        let t = Utc::now();
        let r = remote(10, t);
        assert!(is_unchanged(10, t + Duration::seconds(1), &r));
    }

    #[test]
    fn changed_size_differs() {
        let t = Utc::now();
        let r = remote(10, t);
        assert!(!is_unchanged(11, t, &r));
    }

    #[test]
    fn both_prefers_local_when_newer() {
        let base = Utc::now();
        let r = remote(100, base);
        let local_mtime = base + Duration::seconds(10);
        assert_eq!(resolve_both(100, local_mtime, &r), BothAction::Upload);
    }

    #[test]
    fn both_prefers_remote_when_newer() {
        let base = Utc::now();
        let r = remote(100, base + Duration::seconds(10));
        let local_mtime = base;
        assert_eq!(resolve_both(100, local_mtime, &r), BothAction::Download);
    }

    #[test]
    fn common_prefix_shared_dir() {
        let mut s = HashSet::new();
        s.insert("docs/a.txt".to_string());
        s.insert("docs/b.txt".to_string());
        assert_eq!(common_directory_prefix(&s), Some("docs/".to_string()));
    }
}
