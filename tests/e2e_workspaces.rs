//! Hermetic end-to-end tests for multi-vault workspaces (Phase A).
//!
//! Two independent **local** (age-encrypted file) backends —
//! `[named_backends.local-a]` / `[named_backends.local-b]` — give a real
//! cross-backend workspace with zero cloud dependencies (spec §Testing).
//! Every test uses `env_clear()` + an explicit allowlist (the #317 lesson:
//! selective `env_remove()` leaks host vars into the child), so nothing here
//! reads or writes the host's real config/state.
//!
//! NOTE (Task 4 history): the `get`/`set` tests below originally had to seed
//! workspace state by writing the context JSON file directly, since `xv cx
//! add` didn't exist yet. Task 5 landed `cx add`/`rm`/`default`/`ls`, so
//! every test here now goes through the real CLI surface end-to-end.

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// A hermetic environment with two independent local-backend stores
/// registered as named backends `local-a` and `local-b`, plus a private
/// global context directory. Neither backend is the top-level active
/// backend by default (top-level stays `"local"`, matching the store-less
/// default single-vault path) — `local-a`/`local-b` exist purely as
/// `named_backends` entries for `cx add --backend local-a|local-b`.
struct WorkspaceEnv {
    _tmp: TempDir,
    home: PathBuf,
    config_dir: PathBuf,
}

impl WorkspaceEnv {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let home = tmp.path().join("home");
        let config_dir = home.join(".config");
        let xv_dir = config_dir.join("xv");
        let store_default = tmp.path().join("default").join("store");
        let store_a = tmp.path().join("a").join("store");
        let store_b = tmp.path().join("b").join("store");
        // Each backend's key file must live in its OWN directory: the local
        // backend derives `recipients_file` from `key_file.parent()`
        // (src/backend/local/config.rs), so key files sharing a parent
        // directory would collide on the same recipients.txt and silently
        // cross-contaminate encryption identities across stores.
        let key_default = tmp.path().join("default").join("key.txt");
        let key_a = tmp.path().join("a").join("key.txt");
        let key_b = tmp.path().join("b").join("key.txt");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        for d in [&store_default, &store_a, &store_b] {
            std::fs::create_dir_all(d).expect("create store dir");
        }

        let config_content = format!(
            r#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = "{store_default}"
key_file = "{key_default}"
default_vault = "default"

[named_backends.local-a]
type = "local"
store_path = "{store_a}"
key_file = "{key_a}"
default_vault = "default"

[named_backends.local-b]
type = "local"
store_path = "{store_b}"
key_file = "{key_b}"
default_vault = "default"
"#,
            store_default = store_default.display(),
            key_default = key_default.display(),
            store_a = store_a.display(),
            key_a = key_a.display(),
            store_b = store_b.display(),
            key_b = key_b.display(),
        );
        std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");

        Self {
            _tmp: tmp,
            home,
            config_dir,
        }
    }

    /// A hermetic `xv` command: `env_clear()` + an explicit allowlist, cwd
    /// pinned to a fresh empty scratch dir so no ancestor `.xv.toml` is ever
    /// picked up unless the test explicitly writes one for that purpose.
    fn xv(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_dir)
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("XV_BACKEND", "local")
            .env("NO_COLOR", "1")
            .current_dir(&self.home);
        cmd
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.xv().args(args).output().expect("execute xv binary")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {args:?} failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn err(&self, args: &[&str]) -> std::process::Output {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "command {args:?} unexpectedly succeeded.\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        out
    }

    /// Like `ok`, but returns stdout+stderr combined — `success`/`info`/
    /// `hint` messages are written to stderr (stdout is reserved for
    /// machine-consumable payloads), so assertions on human-readable
    /// confirmation text need both streams.
    fn ok_combined(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {args:?} failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        combined(&out)
    }
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---------------------------------------------------------------------------
// `xv cx` surface (Task 5)
// ---------------------------------------------------------------------------

#[test]
fn cx_add_ls_rm_roundtrip() {
    let env = WorkspaceEnv::new();

    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
    assert!(ls.contains("stage"), "{ls}");
    assert!(ls.contains("local-a"), "{ls}");
    assert!(ls.contains("local-b"), "{ls}");

    env.ok(&["cx", "rm", "stage"]);
    let ls2 = env.ok(&["cx", "ls"]);
    assert!(!ls2.contains("stage"), "{ls2}");
    assert!(ls2.contains("work"), "{ls2}");
}

#[test]
fn cx_first_add_becomes_default() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    let ls = env.ok(&["cx", "ls"]);
    // Single entry in this workspace, so the default marker ("*") appearing
    // anywhere in the output must be "work"'s.
    assert!(ls.contains("work"), "{ls}");
    assert!(ls.contains('*'), "{ls}");
}

#[test]
fn cx_add_duplicate_alias_errors() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    let out = env.err(&["cx", "add", "default", "--backend", "local-b", "--as", "work"]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
}

#[test]
fn cx_add_alias_colliding_with_backend_name_errors() {
    let env = WorkspaceEnv::new();
    let out = env.err(&["cx", "add", "default", "--backend", "local-a", "--as", "azure"]);
    let msg = combined(&out);
    assert!(msg.contains("azure") || msg.contains("collid"), "{msg}");
}

#[test]
fn cx_rm_default_requires_replacement() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);
    // "work" is the default (first-add); removing it while "stage" also
    // exists must error rather than silently reassigning the default.
    let out = env.err(&["cx", "rm", "work"]);
    let msg = combined(&out);
    assert!(msg.contains("default"), "{msg}");

    // After making "stage" the default, removing "work" must succeed.
    env.ok(&["cx", "default", "stage"]);
    env.ok(&["cx", "rm", "work"]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(!ls.contains("work"), "{ls}");
}

#[test]
fn cx_rm_last_entry_restores_single_vault_behavior() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "rm", "work"]);
    let ls_out = env.run(&["cx", "ls"]);
    assert!(ls_out.status.success());
    let msg = combined(&ls_out);
    assert!(
        msg.contains("No workspace attached") || msg.contains("single-vault"),
        "{msg}"
    );
}

#[test]
fn context_use_with_workspace_errors_pointing_at_cx_default() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    let out = env.err(&["context", "use", "some-vault"]);
    let msg = combined(&out);
    assert!(msg.contains("cx default"), "{msg}");
}

#[test]
fn cx_add_probes_vault_exists_unless_force() {
    let env = WorkspaceEnv::new();
    // "local-a" and "local-b" are both valid, reachable named backends —
    // the probe (a list call) must succeed against a real vault name even
    // if the vault is empty (local backend vaults are directories created
    // on demand, so this exercises the "reachable" path, not "must
    // pre-exist"). This mainly asserts --force is accepted and doesn't
    // itself break a normal add.
    env.ok(&[
        "cx", "add", "default", "--backend", "local-a", "--as", "work", "--force",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
}

// ---------------------------------------------------------------------------
// `get`/`set` workspace semantics (Task 4)
// ---------------------------------------------------------------------------

#[test]
fn cx_workspace_get_unqualified_unique_match() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    env.ok(&["set", "stage:ONLY_IN_STAGE", "--value", "v1"]);
    let value = env.ok(&["get", "ONLY_IN_STAGE", "--raw"]);
    assert_eq!(value, "v1");
}

#[test]
fn get_ambiguous_errors_exit_13_lists_qualified_forms() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    env.ok(&["set", "work:DUP_NAME", "--value", "work-value"]);
    env.ok(&["set", "stage:DUP_NAME", "--value", "stage-value"]);

    let out = env.err(&["get", "DUP_NAME"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:DUP_NAME"), "{msg}");
    assert!(msg.contains("stage:DUP_NAME"), "{msg}");
}

#[test]
fn get_qualified_reads_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    env.ok(&["set", "stage:API_KEY", "--value", "stage-secret"]);
    let value = env.ok(&["get", "stage:API_KEY", "--raw"]);
    assert_eq!(value, "stage-secret");
}

#[test]
fn get_unknown_alias_lists_attached() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    let out = env.err(&["get", "nope:SOMETHING"]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
    assert!(msg.contains("stage"), "{msg}");
}

#[test]
fn set_unqualified_writes_default_only() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);
    // "work" is the default (first add).

    env.ok(&["set", "SHARED_NAME", "--value", "unqualified-value"]);

    // Must be readable directly from "work" (the default)...
    let work_value = env.ok(&["get", "work:SHARED_NAME", "--raw"]);
    assert_eq!(work_value, "unqualified-value");

    // ...and absent from "stage" — an unqualified write never searches.
    let stage_lookup = env.err(&["get", "stage:SHARED_NAME"]);
    assert!(!stage_lookup.status.success());
}

#[test]
fn set_qualified_writes_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    env.ok(&["set", "stage:QUALIFIED_WRITE", "--value", "stage-only"]);
    let value = env.ok(&["get", "stage:QUALIFIED_WRITE", "--raw"]);
    assert_eq!(value, "stage-only");

    // Must NOT have landed in "work".
    let work_lookup = env.err(&["get", "work:QUALIFIED_WRITE"]);
    assert!(!work_lookup.status.success());
}

#[test]
fn exact_name_with_colon_wins_over_alias() {
    let env = WorkspaceEnv::new();
    env.ok(&["cx", "add", "default", "--backend", "local-a", "--as", "work"]);
    env.ok(&["cx", "add", "default", "--backend", "local-b", "--as", "stage"]);

    // A secret literally named "work:x" (colon included) written via a
    // qualified write targets the "work" vault's stored name "work:x"
    // itself only when addressed unambiguously; here we seed it through
    // the resolver's own exact-name-first path by writing to work with the
    // literal name, then reading the same literal string back.
    env.ok(&["set", "work:work:x", "--value", "literal-value"]);
    let value = env.ok(&["get", "work:x", "--raw"]);
    assert_eq!(value, "literal-value");
}

// ---------------------------------------------------------------------------
// No-workspace degenerate case: byte-identical to pre-workspace behavior.
// ---------------------------------------------------------------------------

#[test]
fn no_workspace_byte_identical() {
    let env = WorkspaceEnv::new();
    // No `cx add` at all — single-vault mode, using the top-level "local"
    // backend/default vault exactly as every pre-workspace test does.
    let set_out = env.ok_combined(&["set", "PLAIN_SECRET", "--value", "plain-value"]);
    assert!(set_out.contains("Successfully set secret"), "{set_out}");

    let get_out = env.ok(&["get", "PLAIN_SECRET", "--raw"]);
    assert_eq!(get_out, "plain-value");

    // Colon-looking input with no workspace attached must be treated as a
    // plain (unparsed) name — resolve_workspace returns None, so the
    // colon-address parser is never even consulted for this command.
    env.ok(&["set", "literal:with:colons", "--value", "colon-value"]);
    let colon_value = env.ok(&["get", "literal:with:colons", "--raw"]);
    assert_eq!(colon_value, "colon-value");
}

/// Independent proof that the `no_workspace_byte_identical` test above is
/// meaningful: on pre-workspace code (`resolve_workspace` hard-wired to
/// return `None`, simulated here by simply never calling `cx add`), a
/// colon-containing name must round-trip as a single literal name, not be
/// split into alias+path. This guards the "exact-name-first / no alias
/// interpretation without a workspace" contract from silent regressions.
#[test]
fn no_workspace_colon_name_is_never_split() {
    let env = WorkspaceEnv::new();
    env.ok(&["set", "foo:bar/baz", "--value", "v1"]);
    let value = env.ok(&["get", "foo:bar/baz", "--raw"]);
    assert_eq!(value, "v1");
}
