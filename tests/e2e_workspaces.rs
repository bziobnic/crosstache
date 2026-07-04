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

[named_backends.aws-mock]
type = "aws"
region = "us-east-1"
# No real AWS calls are ever made against this entry in tests that don't
# need one — endpoint_url only needs to be syntactically present so client
# construction doesn't reach for real credentials at build time (mirrors
# tests/aws_backend_tests.rs's workspace_touching_only_aws_never_builds_azure).
endpoint_url = "http://127.0.0.1:1"
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

    /// Like `xv()`, but overrides the working directory — for tests that
    /// need a project subdirectory (with its own `.xv.toml`) while keeping
    /// the SAME global context store (`XDG_CONFIG_HOME` is unchanged).
    fn xv_in(&self, cwd: &std::path::Path) -> Command {
        let mut cmd = self.xv();
        cmd.current_dir(cwd);
        cmd
    }

    fn run_in(&self, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
        self.xv_in(cwd)
            .args(args)
            .output()
            .expect("execute xv binary")
    }

    /// Path to the global context store file this environment's `cx`
    /// commands read/write (`ContextManager::global_context_path()`).
    fn context_file_path(&self) -> PathBuf {
        self.config_dir.join("xv").join("context")
    }

    /// Raw bytes of the context store file, for byte-identity assertions
    /// (empty `Vec` if the file doesn't exist yet).
    fn context_file_bytes(&self) -> Vec<u8> {
        std::fs::read(self.context_file_path()).unwrap_or_default()
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
    #[allow(dead_code)]
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

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    // Single entry in this workspace, so the default marker ("*") appearing
    // anywhere in the output must be "work"'s.
    assert!(ls.contains("work"), "{ls}");
    assert!(ls.contains('*'), "{ls}");
}

#[test]
fn cx_add_duplicate_alias_errors() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let out = env.err(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "work",
    ]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
}

#[test]
fn cx_add_alias_colliding_with_backend_name_errors() {
    let env = WorkspaceEnv::new();
    let out = env.err(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "azure",
    ]);
    let msg = combined(&out);
    assert!(msg.contains("azure") || msg.contains("collid"), "{msg}");
}

#[test]
fn cx_rm_default_requires_replacement() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let out = env.err(&["context", "use", "some-vault"]);
    let msg = combined(&out);
    assert!(msg.contains("cx default"), "{msg}");
}

/// Bugbot MEDIUM fix: `cx add`/`rm`/`default` must ERROR (not silently
/// no-op) when the cwd's active `.xv.toml` env profile declares a `vaults`
/// overlay — that overlay REPLACES the context workspace entirely per
/// `resolve_workspace`'s precedence, so a context-store mutation here
/// would persist and report success while every secret command kept using
/// the project overlay instead.
fn write_vaults_overlay(project_dir: &std::path::Path) {
    std::fs::create_dir_all(project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join(".xv.toml"),
        r#"
default_env = "dev"

[env.dev]
vault = "ignored"
vaults = [
  { vault = "proj-vault", backend = "local-b", alias = "proj", default = true },
]
"#,
    )
    .expect("write .xv.toml");
}

#[test]
fn cx_add_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(
        &project_dir,
        &[
            "cx",
            "add",
            "default",
            "--backend",
            "local-b",
            "--as",
            "extra",
        ],
    );
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");
    assert!(msg.contains("dev"), "{msg}");

    // The context store must be entirely UNCHANGED — the attempted
    // mutation must never reach disk.
    assert_eq!(env.context_file_bytes(), context_before);
}

#[test]
fn cx_rm_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(&project_dir, &["cx", "rm", "work"]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");

    assert_eq!(env.context_file_bytes(), context_before);
}

#[test]
fn cx_default_errors_under_project_vaults_overlay() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    let context_before = env.context_file_bytes();

    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    let out = env.run_in(&project_dir, &["cx", "default", "stage"]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_eq!(out.status.code(), Some(3), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains(".xv.toml"), "{msg}");

    assert_eq!(env.context_file_bytes(), context_before);
}

/// Guard test: the same context store, from a cwd WITHOUT the overlay in
/// effect, must be entirely unaffected by the new guard — `cx add` (and by
/// extension rm/default) keeps working exactly as before outside the
/// project directory.
#[test]
fn cx_add_outside_project_still_works() {
    let env = WorkspaceEnv::new();
    // A project dir with a real vaults overlay exists on disk, but cwd
    // (env.home, the default for `env.ok`) is NOT it and has no ancestor
    // .xv.toml (XV_NO_PARENT_CONFIG=1 blocks walk-up regardless).
    let project_dir = env.home.join("project");
    write_vaults_overlay(&project_dir);

    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    let ls = env.ok(&["cx", "ls"]);
    assert!(ls.contains("work"), "{ls}");
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
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
        "--force",
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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ONLY_IN_STAGE", "--value", "v1"]);
    let value = env.ok(&["get", "ONLY_IN_STAGE", "--raw"]);
    assert_eq!(value, "v1");
}

#[test]
fn get_ambiguous_errors_exit_13_lists_qualified_forms() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:API_KEY", "--value", "stage-secret"]);
    let value = env.ok(&["get", "stage:API_KEY", "--raw"]);
    assert_eq!(value, "stage-secret");
}

#[test]
fn get_unknown_alias_lists_attached() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    let out = env.err(&["get", "nope:SOMETHING"]);
    let msg = combined(&out);
    assert!(msg.contains("work"), "{msg}");
    assert!(msg.contains("stage"), "{msg}");
}

#[test]
fn set_unqualified_writes_default_only() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

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
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // A secret literally named "work:x" (colon included) written via a
    // qualified write targets the "work" vault's stored name "work:x"
    // itself only when addressed unambiguously; here we seed it through
    // the resolver's own exact-name-first path by writing to work with the
    // literal name, then reading the same literal string back.
    env.ok(&["set", "work:work:x", "--value", "literal-value"]);
    let value = env.ok(&["get", "work:x", "--raw"]);
    assert_eq!(value, "literal-value");
}

/// Bugbot MEDIUM fix: write-mode verbs (`update`/`delete`) must also apply
/// exact-name-first, scoped to the default vault — not just `set`.
///
/// How a literal `work:x` ends up in the default vault at all (since a
/// *qualified write* always wins on the fresh-secret path — there is no way
/// to `set` a literal `work:x` into the default vault once the `work`
/// alias is attached, because `xv set work:x` would target the `work`
/// vault's `x`, not create a literal in the default): this test creates
/// the literal BEFORE any workspace is attached (single-vault mode, where
/// "work:x" is just an ordinary — if colon-containing — name with no alias
/// interpretation at all), then attaches a workspace afterward whose
/// DEFAULT vault is that same pre-existing store under a different alias
/// ("home") while a SEPARATE alias ("work") points elsewhere. This is the
/// realistic scenario: pre-existing secrets predate the workspace; new
/// ones go through qualified addressing from the start.
#[test]
fn update_and_delete_exact_name_with_colon_wins_over_alias_in_default() {
    let env = WorkspaceEnv::new();

    // Single-vault mode: "work:x" is an ordinary literal name in the
    // top-level default store (the local backend's charset allows ':').
    env.ok(&["set", "work:x", "--value", "original-value"]);

    // Attach a workspace where "work" points elsewhere (local-a) but the
    // DEFAULT ("home") is the top-level store that already holds "work:x".
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local",
        "--as",
        "home",
        "--default",
    ]);

    // "work:x" must resolve the literal secret in the DEFAULT vault
    // ("home"), not alias-interpret "work" (-> local-a, path "x", which
    // doesn't exist there — `update_secret` on a nonexistent local-a
    // target would fail, so this `update` succeeding at all is itself
    // part of the proof).
    env.ok(&["update", "work:x", "updated-value"]);
    // Read back via "home:work:x": a QUALIFIED read ("home" is a real
    // alias) always targets that vault directly with the remainder as the
    // path, bypassing exact-name-first entirely — the cleanest way to
    // confirm placement without depending on read's own exact-name-first
    // (which would otherwise intercept a bare "work:x" query too, since
    // read searches every vault for literal matches first).
    assert_eq!(env.ok(&["get", "home:work:x", "--raw"]), "updated-value");

    env.ok(&["delete", "work:x", "--force"]);
    let deleted_lookup = env.err(&["get", "home:work:x"]);
    assert!(!deleted_lookup.status.success());
}

// ---------------------------------------------------------------------------
// No-workspace degenerate case: byte-identical to pre-workspace behavior.
// ---------------------------------------------------------------------------

/// True byte-golden comparison: no `cx add` is ever called in this test (the
/// workspace code path is provably inert — `resolve_workspace` sees no
/// context workspace and no `.xv.toml` overlay, so it returns `None` and
/// every verb takes the exact pre-workspace branch), so `set`/`get`'s full
/// stdout AND stderr must match these fixed golden strings byte-for-byte —
/// not just "contains a success-looking substring". A fresh hermetic store
/// makes both the vault name ("default") and the version ("v1", first
/// write) deterministic, so the golden is stable across runs.
#[test]
fn no_workspace_byte_identical() {
    let env = WorkspaceEnv::new();

    let set_result = env.run(&["set", "PLAIN_SECRET", "--value", "plain-value"]);
    assert!(set_result.status.success(), "{}", combined(&set_result));
    // `output::success`/`hint` go to stderr; the "Vault:"/"Version:" lines
    // are plain `println!` and land on stdout — both streams are pinned so
    // a future change that moves a line between them would fail loud here.
    assert_eq!(
        stdout_str(&set_result),
        "   Vault: default\n   Version: v1\n",
        "set's Vault/Version lines must be byte-identical to the pre-workspace golden"
    );
    assert_eq!(
        stderr_str(&set_result),
        "[ok] Successfully set secret 'PLAIN_SECRET'\n\
         [hint] Verify with 'xv get PLAIN_SECRET'\n",
        "set's human-readable confirmation output must be byte-identical to the pre-workspace golden"
    );

    let get_result = env.run(&["get", "PLAIN_SECRET", "--raw"]);
    assert!(get_result.status.success(), "{}", combined(&get_result));
    assert_eq!(stdout_str(&get_result), "plain-value");
    assert_eq!(
        stderr_str(&get_result),
        "",
        "get --raw must write nothing to stderr on success"
    );

    // Colon-looking input with no workspace attached must be treated as a
    // plain (unparsed) name — resolve_workspace returns None, so the
    // colon-address parser is never even consulted for this command.
    let colon_set = env.run(&["set", "literal:with:colons", "--value", "colon-value"]);
    assert!(colon_set.status.success(), "{}", combined(&colon_set));
    assert_eq!(
        stdout_str(&colon_set),
        "   Vault: default\n   Version: v1\n",
    );
    assert_eq!(
        stderr_str(&colon_set),
        "[ok] Successfully set secret 'literal:with:colons'\n\
         [hint] Verify with 'xv get literal:with:colons'\n",
    );
    let colon_get = env.run(&["get", "literal:with:colons", "--raw"]);
    assert!(colon_get.status.success(), "{}", combined(&colon_get));
    assert_eq!(stdout_str(&colon_get), "colon-value");
}

fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
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

// ---------------------------------------------------------------------------
// Code-review follow-up: bulk `set` per-key workspace resolution (BLOCKER).
// ---------------------------------------------------------------------------

#[test]
fn bulk_set_unqualified_lands_in_workspace_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);
    // "work" is the default (first add), which differs from the top-level
    // active backend/vault a no-workspace command would have used.

    env.ok(&["set", "BULK_B=b", "BULK_C=c"]);

    assert_eq!(env.ok(&["get", "work:BULK_B", "--raw"]), "b");
    assert_eq!(env.ok(&["get", "work:BULK_C", "--raw"]), "c");

    // Must be absent from "stage" — an unqualified bulk key never searches,
    // it always targets the default entry, same as the single-secret path.
    let stage_lookup = env.err(&["get", "stage:BULK_B"]);
    assert!(!stage_lookup.status.success());
}

#[test]
fn bulk_set_alias_qualified_key_lands_in_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Mixed bulk set: one key qualified to each vault in the same command.
    env.ok(&["set", "work:BULK_A=a", "stage:BULK_D=d"]);

    assert_eq!(env.ok(&["get", "work:BULK_A", "--raw"]), "a");
    assert_eq!(env.ok(&["get", "stage:BULK_D", "--raw"]), "d");

    // Cross-check: each landed ONLY in its qualified vault.
    let work_lookup_d = env.err(&["get", "work:BULK_D"]);
    assert!(!work_lookup_d.status.success());
    let stage_lookup_a = env.err(&["get", "stage:BULK_A"]);
    assert!(!stage_lookup_a.status.success());
}

#[test]
fn bulk_set_no_workspace_unchanged() {
    let env = WorkspaceEnv::new();
    // No `cx add` at all — bulk set must behave exactly as before workspaces.
    env.ok(&["set", "BULK_X=x", "BULK_Y=y"]);
    assert_eq!(env.ok(&["get", "BULK_X", "--raw"]), "x");
    assert_eq!(env.ok(&["get", "BULK_Y", "--raw"]), "y");
}

// ---------------------------------------------------------------------------
// Code-review follow-up: destructive/write verbs (delete/update/rotate/
// restore/purge) and read-resolution verbs (history/rollback) now route
// through the same workspace resolver as get/set (MAJOR).
// ---------------------------------------------------------------------------

#[test]
fn delete_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in the non-default vault ("stage"). An unqualified
    // delete must target the default ("work") only — erroring not-found —
    // rather than searching and deleting the stage copy.
    env.ok(&["set", "stage:DEL_ONLY_STAGE", "--value", "v"]);
    let delete_result = env.err(&["delete", "DEL_ONLY_STAGE", "--force"]);
    assert!(!delete_result.status.success());

    // The stage copy must be untouched.
    assert_eq!(env.ok(&["get", "stage:DEL_ONLY_STAGE", "--raw"]), "v");
}

#[test]
fn delete_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:DEL_ME", "--value", "v"]);
    env.ok(&["delete", "stage:DEL_ME", "--force"]);
    let lookup = env.err(&["get", "stage:DEL_ME"]);
    assert!(!lookup.status.success());
}

#[test]
fn delete_group_unqualified_targets_default_only() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // `--group` has no single secret name to alias-qualify — it must still
    // target the default vault only, never the whole workspace.
    env.ok(&["set", "--group", "demo", "work:GRP_B", "--value", "b"]);
    env.ok(&["set", "--group", "demo", "stage:GRP_C", "--value", "c"]);

    env.ok(&["delete", "--group", "demo", "--force"]);

    let work_lookup = env.err(&["get", "work:GRP_B"]);
    assert!(!work_lookup.status.success());
    // "stage" must be untouched — group delete never spans the workspace.
    assert_eq!(env.ok(&["get", "stage:GRP_C", "--raw"]), "c");
}

#[test]
fn update_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in "stage"; an unqualified update must target
    // "work" only and fail not-found rather than updating the stage copy.
    env.ok(&["set", "stage:UPD_ONLY_STAGE", "--value", "orig"]);
    let update_result = env.err(&["update", "UPD_ONLY_STAGE", "--note", "changed"]);
    assert!(!update_result.status.success());
}

#[test]
fn update_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:UPD_ME", "--value", "orig"]);
    env.ok(&["update", "stage:UPD_ME", "orig-updated"]);
    assert_eq!(env.ok(&["get", "stage:UPD_ME", "--raw"]), "orig-updated");
}

#[test]
fn rotate_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists ONLY in "stage"; an unqualified rotate must target
    // "work" only and fail not-found rather than rotating the stage copy.
    env.ok(&["set", "stage:ROT_ONLY_STAGE", "--value", "orig"]);
    let rotate_result = env.err(&["rotate", "ROT_ONLY_STAGE", "--force"]);
    assert!(!rotate_result.status.success());
    // The stage copy's value must be unchanged.
    assert_eq!(env.ok(&["get", "stage:ROT_ONLY_STAGE", "--raw"]), "orig");
}

#[test]
fn rotate_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ROT_ME", "--value", "orig"]);
    env.ok(&["rotate", "stage:ROT_ME", "--force"]);
    let rotated = env.ok(&["get", "stage:ROT_ME", "--raw"]);
    assert_ne!(rotated, "orig", "rotate must have generated a new value");
}

/// Bugbot round-3 MEDIUM fix: capability gates must evaluate the RESOLVED
/// workspace target's backend, not the process's top-level active
/// backend. `local` and `aws` genuinely differ on `has_secret_rotation`
/// (local: false, aws: true — the only capability flag that differs
/// across crosstache's backend kinds), so `rotate --native` is the one
/// verb where a real, hermetic, e2e-drivable capability MISMATCH exists:
/// the workspace's default is the `aws-mock` named backend (rotation
/// CAPABLE), while a non-default entry is `local-a` (rotation
/// INCAPABLE). Only the capability check itself needs to be hermetic —
/// AWS SDK client construction never makes a network call, so asserting
/// on which error comes back (capability-rejection vs. anything else) is
/// safe without real credentials.
#[test]
#[cfg_attr(not(feature = "aws"), ignore = "requires the aws feature")]
fn rotate_native_capability_gate_reflects_resolved_target_not_workspace_default() {
    let env = WorkspaceEnv::new();
    // aws-mock (rotation-capable) becomes the workspace default (first
    // add). `--force` skips the vault-exists probe, which would otherwise
    // need a real network round-trip against the fake endpoint.
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "aws-mock",
        "--as",
        "cloud",
        "--force",
    ]);

    // The process's top-level active backend (xv.conf's plain `backend =
    // "local"`, a THIRD store distinct from every named backend here) has
    // `has_secret_rotation: false` — the OLD, buggy gate (checking
    // `registry.active().capabilities()`) would have rejected this
    // unqualified rotate even though the workspace's actual default
    // (aws-mock) supports it. This is a bare (colon-free) name, so Write
    // mode's exact-name-first probe never fires (nothing to disambiguate
    // for a bare name) — resolution only MATERIALIZES aws-mock (no network
    // call; AWS SDK client construction is lazy about credentials) and
    // then evaluates its capabilities, both hermetic. The actual
    // `native_rotate` call after the gate DOES need real network and WILL
    // fail — that's fine; the only thing asserted is that the failure is
    // NOT the capability-rejection message, proving the gate itself
    // resolved and read the correct (aws-mock) capabilities.
    let out = env.run(&["rotate", "ANY_SECRET", "--native", "--force"]);
    let msg = combined(&out);
    assert!(
        !msg.contains("does not support native rotation"),
        "aws-mock (the resolved default) supports native rotation, so this must not be \
         capability-rejected — a rejection here would mean the gate is still reading some \
         other backend's (e.g. the top-level active local backend's) capabilities: {msg}"
    );
}

/// Guard: the no-workspace capability-rejection error keeps its EXACT
/// current message and exit code — the local backend has no native
/// rotation support, and `rotate --native` without any workspace attached
/// must reject with byte-identical text to the pre-refactor behavior
/// (`resolved backend == active backend` in the no-workspace case, so
/// moving the gate after resolution changes nothing here).
#[test]
fn rotate_native_capability_error_no_workspace_is_byte_stable() {
    let env = WorkspaceEnv::new();
    // No `cx add` at all.
    let out = env.err(&["rotate", "SOME_SECRET", "--native", "--force"]);
    assert_eq!(out.status.code(), Some(2), "{}", combined(&out));
    let msg = combined(&out);
    assert_eq!(
        msg.trim(),
        "error[xv-invalid-argument]: Invalid argument: The local backend does not support native rotation. Native rotation is currently available on the aws backend only; without --native, 'xv rotate' generates a new value client-side on any backend."
    );
}

#[test]
fn history_ambiguous_errors_exit_13() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "work:HIST_DUP", "--value", "v1"]);
    env.ok(&["set", "stage:HIST_DUP", "--value", "v1"]);

    let out = env.err(&["history", "HIST_DUP"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:HIST_DUP"), "{msg}");
    assert!(msg.contains("stage:HIST_DUP"), "{msg}");
}

#[test]
fn history_qualified_reads_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:HIST_ME", "--value", "v1"]);
    let out = env.ok(&["history", "stage:HIST_ME"]);
    assert!(out.contains("HIST_ME") || !out.is_empty(), "{out}");
}

#[test]
fn rollback_unqualified_unique_match_searches_and_resolves() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Rollback is grouped with `get`/`history` as a read-resolution verb
    // (searches attached vaults on an unqualified name), NOT a
    // default-vault-only write — unlike delete/update/rotate/restore/purge.
    // The secret exists ONLY in "stage" (not the default, "work"); an
    // unqualified rollback must still find and roll it back there.
    env.ok(&["set", "stage:ROLL_ONLY_STAGE", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_ONLY_STAGE", "v2val"]);
    env.ok(&["rollback", "ROLL_ONLY_STAGE", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "stage:ROLL_ONLY_STAGE", "--raw"]), "v1val");
}

#[test]
fn rollback_ambiguous_errors_exit_13() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Same name, with a version history, in both vaults — an unqualified
    // rollback must be ambiguous (exit 13), same as `get`.
    env.ok(&["set", "work:ROLL_DUP", "--value", "v1val"]);
    env.ok(&["update", "work:ROLL_DUP", "v2val"]);
    env.ok(&["set", "stage:ROLL_DUP", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_DUP", "v2val"]);

    let out = env.err(&["rollback", "ROLL_DUP", "--version", "v1", "--force"]);
    assert_eq!(out.status.code(), Some(13), "{}", combined(&out));
    let msg = combined(&out);
    assert!(msg.contains("work:ROLL_DUP"), "{msg}");
    assert!(msg.contains("stage:ROLL_DUP"), "{msg}");
}

#[test]
fn rollback_qualified_targets_named_vault() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    env.ok(&["set", "stage:ROLL_ME", "--value", "v1val"]);
    env.ok(&["update", "stage:ROLL_ME", "v2val"]);
    env.ok(&["rollback", "stage:ROLL_ME", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "stage:ROLL_ME", "--raw"]), "v1val");
}

#[test]
fn purge_unqualified_targets_default_never_searches() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Secret exists (soft-deleted) ONLY in "stage"; an unqualified purge
    // must target "work" only, never "stage". The local backend's purge is
    // idempotent on a name with nothing to purge (no trash entries — a
    // no-op `Ok(())`, not a not-found error), so "unqualified purge
    // succeeds" alone doesn't prove anything; the proof is that the stage
    // copy is STILL RESTORABLE afterward (i.e. still in trash, not
    // actually purged) — if the unqualified purge had wrongly searched and
    // purged the stage copy, this restore would fail. This is the
    // trait-path proof for the Bugbot HIGH fix: purge's legacy-vs-trait
    // decision now runs on the RESOLVED target, and on this (local-backend)
    // trait path that means default-only, never-searched addressing — same
    // contract as delete/update/rotate.
    env.ok(&["set", "stage:PURGE_ONLY_STAGE", "--value", "v"]);
    env.ok(&["delete", "stage:PURGE_ONLY_STAGE", "--force"]);
    env.ok(&["purge", "PURGE_ONLY_STAGE", "--force"]); // no-op against "work"

    env.ok(&["restore", "stage:PURGE_ONLY_STAGE"]);
    assert_eq!(env.ok(&["get", "stage:PURGE_ONLY_STAGE", "--raw"]), "v");

    // The stage copy must still be purgeable directly (qualified), for real.
    env.ok(&["delete", "stage:PURGE_ONLY_STAGE", "--force"]);
    env.ok(&["purge", "stage:PURGE_ONLY_STAGE", "--force"]);
}

#[test]
fn restore_and_purge_unqualified_target_default() {
    let env = WorkspaceEnv::new();
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-a",
        "--as",
        "work",
    ]);
    env.ok(&[
        "cx",
        "add",
        "default",
        "--backend",
        "local-b",
        "--as",
        "stage",
    ]);

    // Seed the SAME name in both vaults, soft-delete both, then restore the
    // default ("work") unqualified — the "stage" copy must stay deleted.
    env.ok(&["set", "work:RESTORE_ME", "--value", "v"]);
    env.ok(&["set", "stage:RESTORE_ME", "--value", "v"]);
    env.ok(&["delete", "work:RESTORE_ME", "--force"]);
    env.ok(&["delete", "stage:RESTORE_ME", "--force"]);

    env.ok(&["restore", "RESTORE_ME"]);
    assert_eq!(env.ok(&["get", "work:RESTORE_ME", "--raw"]), "v");
    let stage_still_deleted = env.err(&["get", "stage:RESTORE_ME"]);
    assert!(!stage_still_deleted.status.success());

    // Purge the (now-restored, then re-deleted) default copy; qualified
    // purge on "stage" must independently succeed on its own deleted copy.
    env.ok(&["delete", "work:RESTORE_ME", "--force"]);
    env.ok(&["purge", "RESTORE_ME", "--force"]);
    env.ok(&["purge", "stage:RESTORE_ME", "--force"]);
}

/// No-workspace guard, extended to every write/read verb touched by this
/// follow-up: with no `cx add` at all, delete/update/rotate/restore/purge/
/// history/rollback must all behave exactly as before workspaces existed.
#[test]
fn no_workspace_write_and_read_verbs_unchanged() {
    let env = WorkspaceEnv::new();

    env.ok(&["set", "GUARD_ME", "--value", "orig"]);
    let hist = env.ok(&["history", "GUARD_ME"]);
    assert!(!hist.is_empty());

    env.ok(&["update", "GUARD_ME", "orig-updated"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), "orig-updated");

    env.ok(&["rollback", "GUARD_ME", "--version", "v1", "--force"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), "orig");
    // Put it back to the updated value so the rest of the guard proceeds
    // as it did before rollback was added to this test.
    env.ok(&["update", "GUARD_ME", "orig-updated"]);

    env.ok(&["rotate", "GUARD_ME", "--force"]);
    let rotated = env.ok(&["get", "GUARD_ME", "--raw"]);
    assert_ne!(rotated, "orig-updated");

    env.ok(&["delete", "GUARD_ME", "--force"]);
    let deleted_lookup = env.err(&["get", "GUARD_ME"]);
    assert!(!deleted_lookup.status.success());

    env.ok(&["restore", "GUARD_ME"]);
    assert_eq!(env.ok(&["get", "GUARD_ME", "--raw"]), rotated);

    env.ok(&["delete", "GUARD_ME", "--force"]);
    env.ok(&["purge", "GUARD_ME", "--force"]);
}
