//! Git-native versioning for the local store.
//!
//! With `[local].git = true` the store becomes a real git repository: every
//! mutation is followed by a commit, so `git log`, `git diff`, `git bisect`, and
//! `git push` all work on the actual history — the `pass` model. This is
//! additive to the backend's own `.versions/` archive, which continues to drive
//! `xv history` / `xv rollback` on every backend.
//!
//! ## What is and is not committed
//!
//! Only **age ciphertext** and metadata land in commits. The age identity and
//! recipients files are excluded two ways:
//!
//! 1. A managed `.gitignore` (written by [`LocalGitStore::ensure_repo`]) lists
//!    the identity and recipients filenames plus the advisory `.lock` files.
//! 2. Before every commit, [`LocalGitStore::commit`] re-inspects the staged
//!    path list and **aborts** if the identity or recipients file is present.
//!    The `.gitignore` is a convenience; this check is the actual gate, because
//!    a user can defeat `.gitignore` with `git add -f` or by relocating
//!    `key_file` inside the store.
//!
//! Git history is append-only for practical purposes, so a private key
//! committed once is very hard to expunge. That asymmetry is why the guard is a
//! hard error rather than a warning.
//!
//! Committing the store also strengthens [`super::audit`]: a git remote holds
//! copies of the audit log that a local attacker cannot reach, which is the
//! off-box sink the hash chain alone cannot provide.
//!
//! ## Why the `git` CLI rather than a library
//!
//! `src/scan/staged.rs` already shells out to `git` for the pre-commit scanner,
//! so the dependency is established and users' `includeIf`, hooks, signing
//! config, and credential helpers all keep working — which matters most for
//! `push`/`pull` against a real remote.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::backend::error::BackendError;

/// Filenames that must never be committed, relative to the store root.
/// Extended at runtime with the resolved identity/recipients filenames when
/// those live inside the store.
const ALWAYS_IGNORED: &[&str] = &[".lock", "*.lock"];

/// Marker delimiting the block `xv` manages inside `.gitignore`, so a
/// user-authored `.gitignore` is preserved across upgrades.
const MANAGED_HEADER: &str = "# --- managed by crosstache (xv); do not edit this block ---";
const MANAGED_FOOTER: &str = "# --- end crosstache block ---";

/// One commit as reported by [`LocalGitStore::log`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GitCommit {
    /// Abbreviated commit hash.
    pub hash: String,
    /// Author date, ISO-8601.
    pub date: String,
    /// Subject line.
    pub subject: String,
}

/// Git-backed versioning for one local store directory.
pub struct LocalGitStore {
    store_path: PathBuf,
    /// Absolute path to the age identity file.
    key_file: PathBuf,
    /// Absolute path to the age recipients file.
    recipients_file: PathBuf,
}

impl LocalGitStore {
    /// Bind a git store to `store_path`, guarding the given key material.
    pub fn new(store_path: PathBuf, key_file: PathBuf, recipients_file: PathBuf) -> Self {
        Self {
            store_path,
            key_file,
            recipients_file,
        }
    }

    /// Whether the store already contains a git repository.
    pub fn is_repo(&self) -> bool {
        self.store_path.join(".git").exists()
    }

    /// Initialize the repository if absent and (re)write the managed
    /// `.gitignore` block. Idempotent.
    pub fn ensure_repo(&self) -> Result<(), BackendError> {
        if !self.store_path.exists() {
            return Err(BackendError::Internal(format!(
                "local store {} does not exist yet",
                self.store_path.display()
            )));
        }
        if !self.is_repo() {
            // `-b main` pins the initial branch so behavior does not depend on
            // the user's init.defaultBranch or their git version's default.
            self.run(&["init", "-b", "main"])?;
        }
        self.write_gitignore()?;
        Ok(())
    }

    /// Paths (relative to the store) that must never be tracked.
    fn ignored_patterns(&self) -> Vec<String> {
        let mut patterns: Vec<String> = ALWAYS_IGNORED.iter().map(|s| (*s).to_string()).collect();
        for key_path in [&self.key_file, &self.recipients_file] {
            // Ignore by relative path when the file lives inside the store, and
            // by bare filename otherwise — the latter is belt-and-braces for a
            // store later moved on top of key material.
            if let Some(rel) = self.relative_to_store(key_path) {
                patterns.push(rel.to_string_lossy().replace('\\', "/"));
            } else if let Some(name) = key_path.file_name() {
                patterns.push(name.to_string_lossy().to_string());
            }
        }
        patterns.sort();
        patterns.dedup();
        patterns
    }

    /// Write (or refresh) the managed block in `<store>/.gitignore`, preserving
    /// any user-authored lines outside it.
    fn write_gitignore(&self) -> Result<(), BackendError> {
        let path = self.store_path.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();

        let mut preserved: Vec<&str> = Vec::new();
        let mut inside = false;
        for line in existing.lines() {
            if line.trim() == MANAGED_HEADER {
                inside = true;
                continue;
            }
            if line.trim() == MANAGED_FOOTER {
                inside = false;
                continue;
            }
            if !inside {
                preserved.push(line);
            }
        }

        let mut out = String::new();
        out.push_str(MANAGED_HEADER);
        out.push('\n');
        out.push_str("# age key material and lock files are never versioned.\n");
        for pattern in self.ignored_patterns() {
            out.push_str(&pattern);
            out.push('\n');
        }
        out.push_str(MANAGED_FOOTER);
        out.push('\n');
        let trailing = preserved.join("\n");
        if !trailing.trim().is_empty() {
            out.push_str(trailing.trim_start_matches('\n'));
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }

        std::fs::write(&path, out)
            .map_err(|e| BackendError::Internal(format!("write store .gitignore: {e}")))
    }

    /// Stage everything and commit with `message`.
    ///
    /// Returns the new commit's short hash, or `None` when the working tree was
    /// already clean (nothing to record).
    ///
    /// Aborts with an error — before committing — if key material is staged.
    pub fn commit(&self, message: &str) -> Result<Option<String>, BackendError> {
        self.ensure_repo()?;
        self.run(&["add", "-A"])?;

        // Hard gate. Runs after staging and before the commit, so it sees
        // exactly what would be recorded, including anything a previous
        // `git add -f` forced into the index.
        self.assert_no_key_material_staged()?;

        // Nothing staged → nothing to commit. `git commit` would exit non-zero.
        if self.run(&["diff", "--cached", "--quiet"]).is_ok() {
            return Ok(None);
        }

        let mut args: Vec<String> = Vec::new();
        // Supply an identity only when the user has none configured, so
        // existing user.name/user.email and signing settings win.
        if !self.has_committer_identity() {
            args.push("-c".into());
            args.push("user.name=crosstache".into());
            args.push("-c".into());
            args.push("user.email=xv@localhost".into());
        }
        args.push("commit".into());
        args.push("-m".into());
        args.push(message.to_string());
        // A store commit must not be blocked or rewritten by repo hooks the
        // user installed for source code (e.g. an `xv scan` pre-commit hook,
        // which would flag the store's own ciphertext).
        args.push("--no-verify".into());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&arg_refs)?;
        let hash = self.run(&["rev-parse", "--short", "HEAD"])?;
        Ok(Some(hash.trim().to_string()))
    }

    /// Error if the index contains the age identity or recipients file.
    fn assert_no_key_material_staged(&self) -> Result<(), BackendError> {
        let staged = self.run(&["diff", "--cached", "--name-only", "-z"])?;
        let guarded: Vec<PathBuf> = [&self.key_file, &self.recipients_file]
            .iter()
            .filter_map(|p| self.relative_to_store(p))
            .collect();
        if guarded.is_empty() {
            return Ok(());
        }

        for entry in staged.split('\0').filter(|s| !s.is_empty()) {
            let entry_path = PathBuf::from(entry);
            if guarded.contains(&entry_path) {
                // Unstage so a retry after the user moves the key does not trip
                // over a still-staged secret key.
                let _ = self.run(&["reset", "-q", "HEAD", "--", entry]);
                return Err(BackendError::Internal(format!(
                    "refusing to commit the local store: age key material ('{entry}') is staged. \
                     Git history is effectively permanent, so committing a private key would \
                     expose every secret in the store to anyone who ever sees the repo. Move \
                     key_file outside the store (the default is ~/.xv/key.txt, with the store at \
                     ~/.xv/store) or add it to .gitignore, then retry."
                )));
            }
        }
        Ok(())
    }

    /// Whether git can already determine a committer identity.
    fn has_committer_identity(&self) -> bool {
        self.run(&["config", "user.email"])
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }

    /// Commit history, newest first. `limit` of 0 means unlimited.
    ///
    /// `secret_filter` restricts history to commits *recorded for* that secret,
    /// by matching the commit subjects this module writes (`set NAME`,
    /// `rename OLD to NEW`, …) — never by pathspec. Paths would miss every
    /// commit under `[local].opaque_filenames`, where on-disk stems are keyed
    /// hashes that contain no trace of the name; subjects always carry the
    /// plain name on both layouts, and keep matching across a legacy→opaque
    /// migration that renames every file.
    pub fn log(
        &self,
        secret_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GitCommit>, BackendError> {
        if !self.is_repo() {
            return Err(self.not_a_repo());
        }
        let mut args: Vec<String> = vec![
            "log".into(),
            // Unit separator between fields, record separator between commits:
            // neither can appear in a commit subject, unlike the tab/pipe a
            // naive format would use.
            "--pretty=format:%h\u{1f}%aI\u{1f}%s".into(),
        ];
        // With a filter, fetch everything and bound *after* filtering —
        // otherwise `-nN` would count non-matching commits against the limit.
        if limit > 0 && secret_filter.is_none() {
            args.push(format!("-n{limit}"));
        }

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&arg_refs)?;
        let mut commits: Vec<GitCommit> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| {
                let mut parts = line.split('\u{1f}');
                Some(GitCommit {
                    hash: parts.next()?.to_string(),
                    date: parts.next()?.to_string(),
                    subject: parts.next().unwrap_or_default().to_string(),
                })
            })
            .collect();

        if let Some(name) = secret_filter {
            commits.retain(|c| subject_mentions(&c.subject, name));
            if limit > 0 {
                commits.truncate(limit);
            }
        }
        Ok(commits)
    }

    /// `git status --short` output for the store.
    pub fn status(&self) -> Result<String, BackendError> {
        if !self.is_repo() {
            return Err(self.not_a_repo());
        }
        // `--untracked-files=all` lists individual files; the default collapses
        // an untracked directory to `?? vaults/`, which hides which secrets are
        // uncommitted — the one thing this command exists to answer.
        self.run(&["status", "--short", "--branch", "--untracked-files=all"])
    }

    /// Name-and-status diff for `rev` (default: last commit).
    ///
    /// Deliberately never emits file *contents*: a textual diff of the store
    /// would be a diff of age ciphertext (useless) or, for plaintext metadata,
    /// could surface note/tag values in terminal scrollback and CI logs.
    pub fn diff(&self, rev: Option<&str>) -> Result<String, BackendError> {
        if !self.is_repo() {
            return Err(self.not_a_repo());
        }
        let rev = rev.unwrap_or("HEAD");
        // `--name-status` gives paths plus change type and no file contents.
        // Note `--no-patch` would suppress the name list too, defeating the
        // purpose, so it is deliberately absent.
        self.run(&["show", "--name-status", "--oneline", rev])
    }

    /// Push the store to a remote.
    pub fn push(&self, remote: &str, branch: Option<&str>) -> Result<String, BackendError> {
        if !self.is_repo() {
            return Err(self.not_a_repo());
        }
        match branch {
            Some(b) => self.run(&["push", remote, b]),
            None => self.run(&["push", remote]),
        }
    }

    /// Pull from a remote. Uses `--ff-only`: a merge of two divergent secret
    /// stores would need conflict resolution on ciphertext, which cannot be
    /// done meaningfully, so we refuse rather than produce a broken tree.
    pub fn pull(&self, remote: &str, branch: Option<&str>) -> Result<String, BackendError> {
        if !self.is_repo() {
            return Err(self.not_a_repo());
        }
        let mut args = vec!["pull", "--ff-only", remote];
        if let Some(b) = branch {
            args.push(b);
        }
        self.run(&args)
    }

    /// Make `path` relative to the store root, or `None` if it is outside.
    ///
    /// Canonicalizes both sides where possible so a symlinked or
    /// `..`-containing `key_file` cannot slip past the containment check.
    fn relative_to_store(&self, path: &Path) -> Option<PathBuf> {
        let store = self
            .store_path
            .canonicalize()
            .unwrap_or_else(|_| self.store_path.clone());
        let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        candidate.strip_prefix(&store).ok().map(Path::to_path_buf)
    }

    fn not_a_repo(&self) -> BackendError {
        BackendError::Internal(format!(
            "the local store at {} is not a git repository. Run 'xv git init' to start \
             versioning it, or set git = true under [local] in your config.",
            self.store_path.display()
        ))
    }

    /// Run `git` inside the store, returning stdout on success.
    fn run(&self, args: &[&str]) -> Result<String, BackendError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.store_path)
            .args(args)
            .output()
            .map_err(|e| {
                BackendError::Internal(format!(
                    "failed to run git: {e}. Git-native versioning requires git on PATH."
                ))
            })?;
        if !out.status.success() {
            return Err(BackendError::Internal(format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or("?"),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

/// Whether a commit subject written by this module refers to `name`.
///
/// The subject vocabulary is closed — every auto-commit message is produced by
/// [`LocalSecretBackend`](super::secrets::LocalSecretBackend)'s hooks — so exact
/// verb matching is possible and preferred over a substring search, which would
/// let a secret named `A` match every other secret's history.
///
/// A rename matches on either side, so `xv git log NEW_NAME` picks up the
/// commit that created the name and `xv git log OLD_NAME` shows where the old
/// history ends. A name that itself contains `" to "` can over-match the rename
/// patterns; that ambiguity is inherent to a flat subject line and acceptable
/// for a history *view* (nothing security-relevant keys off this filter).
fn subject_mentions(subject: &str, name: &str) -> bool {
    for verb in ["set", "update", "delete", "restore", "purge"] {
        if subject == format!("{verb} {name}") {
            return true;
        }
    }
    subject.starts_with(&format!("rollback {name} to "))
        || subject.starts_with(&format!("rename {name} to "))
        || (subject.starts_with("rename ") && subject.ends_with(&format!(" to {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Store with key material *outside* it — the default, safe layout.
    fn fixture() -> (TempDir, LocalGitStore) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(store.join("vaults/default/secrets")).unwrap();
        let git = LocalGitStore::new(
            store,
            tmp.path().join("key.txt"),
            tmp.path().join("recipients.txt"),
        );
        (tmp, git)
    }

    fn write_secret(git: &LocalGitStore, name: &str, body: &str) {
        let dir = git.store_path.join("vaults/default/secrets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.age")), body).unwrap();
    }

    #[test]
    fn ensure_repo_is_idempotent_and_writes_managed_gitignore() {
        let (_tmp, git) = fixture();
        assert!(!git.is_repo());
        git.ensure_repo().unwrap();
        assert!(git.is_repo());
        git.ensure_repo().unwrap();

        let ignore = std::fs::read_to_string(git.store_path.join(".gitignore")).unwrap();
        assert!(ignore.contains(MANAGED_HEADER));
        assert!(ignore.contains("key.txt"));
        assert!(ignore.contains("recipients.txt"));
        // Exactly one managed block after two runs.
        assert_eq!(ignore.matches(MANAGED_HEADER).count(), 1);
    }

    #[test]
    fn gitignore_preserves_user_lines() {
        let (_tmp, git) = fixture();
        std::fs::write(git.store_path.join(".gitignore"), "# mine\nscratch/\n").unwrap();
        git.ensure_repo().unwrap();
        let ignore = std::fs::read_to_string(git.store_path.join(".gitignore")).unwrap();
        assert!(ignore.contains("# mine"), "{ignore}");
        assert!(ignore.contains("scratch/"), "{ignore}");
        assert!(ignore.contains("key.txt"), "{ignore}");
    }

    #[test]
    fn commit_records_ciphertext_and_returns_hash() {
        let (_tmp, git) = fixture();
        write_secret(&git, "DB_PASSWORD", "age-ciphertext-1");
        let hash = git.commit("set DB_PASSWORD").unwrap();
        assert!(hash.is_some(), "first write should produce a commit");

        let log = git.log(None, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "set DB_PASSWORD");
        assert!(!log[0].hash.is_empty());
        assert!(!log[0].date.is_empty());
    }

    #[test]
    fn clean_tree_commits_nothing() {
        let (_tmp, git) = fixture();
        write_secret(&git, "A", "ct");
        assert!(git.commit("set A").unwrap().is_some());
        assert!(
            git.commit("set A again").unwrap().is_none(),
            "an unchanged tree must not create an empty commit"
        );
        assert_eq!(git.log(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn log_filters_by_secret_name() {
        let (_tmp, git) = fixture();
        write_secret(&git, "ALPHA", "ct1");
        git.commit("set ALPHA").unwrap();
        write_secret(&git, "BETA", "ct2");
        git.commit("set BETA").unwrap();
        write_secret(&git, "ALPHA", "ct3");
        git.commit("update ALPHA").unwrap();

        let alpha = git.log(Some("ALPHA"), 0).unwrap();
        let subjects: Vec<&str> = alpha.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["update ALPHA", "set ALPHA"]);

        assert_eq!(git.log(None, 0).unwrap().len(), 3);
        assert_eq!(git.log(None, 2).unwrap().len(), 2);
        // With a filter, the limit bounds MATCHING commits, not raw history —
        // "set BETA" between the two ALPHA commits must not eat the budget.
        assert_eq!(git.log(Some("ALPHA"), 1).unwrap().len(), 1);
    }

    #[test]
    fn log_filter_works_when_paths_are_opaque_stems() {
        // The regression Bugbot caught: with [local].opaque_filenames the
        // committed paths are keyed-hash stems that contain no trace of the
        // secret name, so a pathspec filter (`*NAME*`) matches nothing. The
        // filter must key on commit subjects, which always carry plain names.
        let (_tmp, git) = fixture();
        write_secret(&git, "T5KQZJ2MBQXW3ZL6", "opaque-ct-1");
        git.commit("set DB_PASSWORD").unwrap();
        write_secret(&git, "9YHFA4WNPRC8E2VD", "opaque-ct-2");
        git.commit("set OTHER").unwrap();
        write_secret(&git, "T5KQZJ2MBQXW3ZL6", "opaque-ct-3");
        git.commit("update DB_PASSWORD").unwrap();

        let subjects: Vec<String> = git
            .log(Some("DB_PASSWORD"), 0)
            .unwrap()
            .into_iter()
            .map(|c| c.subject)
            .collect();
        assert_eq!(
            subjects,
            vec!["update DB_PASSWORD", "set DB_PASSWORD"],
            "opaque on-disk paths must not hide a secret's history"
        );
    }

    #[test]
    fn log_filter_matches_whole_names_not_substrings() {
        // A secret named "A" must not match every subject containing an A —
        // the closed subject vocabulary allows exact matching.
        let (_tmp, git) = fixture();
        write_secret(&git, "A", "ct");
        git.commit("set A").unwrap();
        write_secret(&git, "ALPHA", "ct");
        git.commit("set ALPHA").unwrap();

        let a = git.log(Some("A"), 0).unwrap();
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].subject, "set A");
    }

    #[test]
    fn log_filter_matches_both_sides_of_a_rename() {
        let (_tmp, git) = fixture();
        write_secret(&git, "OLD", "ct");
        git.commit("set OLD").unwrap();
        write_secret(&git, "NEW", "ct");
        git.commit("rename OLD to NEW").unwrap();
        write_secret(&git, "NEW", "ct2");
        git.commit("update NEW").unwrap();

        let old = git.log(Some("OLD"), 0).unwrap();
        let old_subjects: Vec<&str> = old.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            old_subjects,
            vec!["rename OLD to NEW", "set OLD"],
            "the old name's history should end at the rename"
        );

        let new = git.log(Some("NEW"), 0).unwrap();
        let new_subjects: Vec<&str> = new.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            new_subjects,
            vec!["update NEW", "rename OLD to NEW"],
            "the new name's history should start at the rename"
        );
    }

    #[test]
    fn commit_subject_with_delimiters_survives_log_parsing() {
        let (_tmp, git) = fixture();
        write_secret(&git, "WEIRD", "ct");
        // Tabs and pipes would break a naive --pretty format.
        git.commit("set A\tB|C secret").unwrap();
        let log = git.log(None, 1).unwrap();
        assert_eq!(log[0].subject, "set A\tB|C secret");
    }

    #[test]
    fn refuses_to_commit_identity_inside_the_store() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        // Pathological config: key_file lives *inside* the versioned store.
        let key = store.join("key.txt");
        std::fs::write(&key, "AGE-SECRET-KEY-1EXAMPLE").unwrap();
        let git = LocalGitStore::new(store.clone(), key.clone(), store.join("recipients.txt"));

        write_secret(&git, "A", "ct");
        git.ensure_repo().unwrap();
        // Force it past .gitignore, exactly as `git add -f` would.
        git.run(&["add", "-f", "key.txt"]).unwrap();

        let err = git.commit("set A").expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("refusing to commit"), "{msg}");
        assert!(msg.contains("key.txt"), "{msg}");

        // And the key must not have been recorded.
        let tracked = git.run(&["ls-files"]).unwrap();
        assert!(
            !tracked.contains("key.txt"),
            "key material must never be tracked: {tracked}"
        );
    }

    #[test]
    fn gitignore_keeps_inside_store_key_untracked_by_default() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let key = store.join("key.txt");
        std::fs::write(&key, "AGE-SECRET-KEY-1EXAMPLE").unwrap();
        let git = LocalGitStore::new(store.clone(), key, store.join("recipients.txt"));

        write_secret(&git, "A", "ct");
        // No `add -f` this time: the managed .gitignore should keep it out and
        // the commit should succeed.
        let hash = git.commit("set A").unwrap();
        assert!(hash.is_some());
        let tracked = git.run(&["ls-files"]).unwrap();
        assert!(!tracked.contains("key.txt"), "{tracked}");
        assert!(tracked.contains("A.age"), "{tracked}");
    }

    #[test]
    fn audit_log_is_versioned() {
        // The audit log must be committed — a git remote is the off-box copy
        // the hash chain alone cannot provide.
        let (_tmp, git) = fixture();
        let audit_dir = git.store_path.join("vaults/default/.audit");
        std::fs::create_dir_all(&audit_dir).unwrap();
        std::fs::write(audit_dir.join("log.jsonl"), "{\"seq\":1}\n").unwrap();
        git.commit("set A").unwrap();
        let tracked = git.run(&["ls-files"]).unwrap();
        assert!(tracked.contains(".audit/log.jsonl"), "{tracked}");
    }

    #[test]
    fn lock_files_are_not_versioned() {
        let (_tmp, git) = fixture();
        write_secret(&git, "A", "ct");
        std::fs::write(git.store_path.join("vaults/default/.lock"), "").unwrap();
        git.commit("set A").unwrap();
        let tracked = git.run(&["ls-files"]).unwrap();
        assert!(!tracked.contains(".lock"), "{tracked}");
    }

    #[test]
    fn operations_error_before_init() {
        let (_tmp, git) = fixture();
        assert!(git.log(None, 5).is_err());
        assert!(git.status().is_err());
        assert!(git.diff(None).is_err());
        let msg = git.log(None, 5).unwrap_err().to_string();
        assert!(msg.contains("xv git init"), "{msg}");
    }

    #[test]
    fn diff_reports_names_without_contents() {
        let (_tmp, git) = fixture();
        write_secret(&git, "A", "plaintext-metadata-value");
        git.commit("set A").unwrap();
        let diff = git.diff(None).unwrap();
        assert!(diff.contains("A.age"), "{diff}");
        assert!(
            !diff.contains("plaintext-metadata-value"),
            "diff must not print file contents: {diff}"
        );
    }

    #[test]
    fn status_reports_untracked_changes() {
        let (_tmp, git) = fixture();
        git.ensure_repo().unwrap();
        write_secret(&git, "A", "ct");
        let status = git.status().unwrap();
        assert!(status.contains("A.age"), "{status}");
    }
}
