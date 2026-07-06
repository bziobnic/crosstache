//! Behavior locks for the vault-alias ergonomics: the `--alias` spelling on
//! `cx add`, and the long (`-l`) listing surfacing the real vault name behind
//! an alias. Fully hermetic against the local (age-encrypted file) backend.
//!
//! Note on output format: `xv`'s `Auto` format resolves to JSON when stdout is
//! not a TTY (as under `cargo test`), so these tests pass `--format table`
//! explicitly to reach the human table / long views.

mod common;

use common::{xv, xv_isolated_local, xv_isolated_local_with_profile};
use std::path::Path;
use std::process::{Command, Output};

fn local_cmd(temp: &Path) -> Command {
    let mut c = xv();
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp)
        .env("XDG_CONFIG_HOME", temp.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp);
    c
}

fn run(temp: &Path, args: &[&str]) -> Output {
    local_cmd(temp).args(args).output().expect("spawn xv")
}

/// Run and assert success, returning stdout as a String.
fn ok(temp: &Path, args: &[&str]) -> String {
    let out = run(temp, args);
    assert!(
        out.status.success(),
        "expected success for {args:?}\n--stdout--\n{}\n--stderr--\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// 1. `--alias` spelling on `cx add`
// ---------------------------------------------------------------------------

#[test]
fn cx_add_alias_flag_attaches_with_that_alias() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    ok(t, &["vault", "create", "kv-scottzionic"]);
    // The user reaches for `--alias` (a visible alias of `--as`).
    ok(t, &["cx", "add", "kv-scottzionic", "--alias", "kv"]);

    let json = ok(t, &["cx", "ls", "--format", "json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("valid json");
    assert!(
        rows.iter()
            .any(|r| r["alias"] == "kv" && r["vault"] == "kv-scottzionic"),
        "cx ls should show the entry attached under alias 'kv': {json}"
    );
}

// ---------------------------------------------------------------------------
// 2. Union listing: Vault column keeps the alias; long view adds the vault name
// ---------------------------------------------------------------------------

/// Attach `kv-scottzionic` as `kv` (alias differs from vault) plus the
/// auto-attached `default` (alias == vault), and seed one secret in each.
fn workspace_with_kv(t: &Path) {
    ok(t, &["vault", "create", "kv-scottzionic"]);
    ok(t, &["cx", "add", "kv-scottzionic", "--alias", "kv"]);
    ok(t, &["set", "S_DEF", "--value", "v0"]);
    ok(t, &["set", "kv:S_KV", "--value", "v1"]);
}

#[test]
fn union_table_vault_column_shows_the_alias_not_the_vault_name() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    workspace_with_kv(t);

    // Default/table view: the Vault column carries the alias, never the raw
    // vault name — unchanged by the long-view work.
    let table = ok(t, &["--format", "table", "ls"]);
    assert!(
        table.contains("kv"),
        "Vault column should show the alias: {table}"
    );
    assert!(
        !table.contains("kv-scottzionic"),
        "the legacy table shows the alias, not the real vault name: {table}"
    );
}

#[test]
fn long_view_surfaces_the_real_vault_name_when_alias_differs() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    workspace_with_kv(t);

    let long = ok(t, &["--format", "table", "ls", "-l"]);
    // Alias 'kv' differs from vault 'kv-scottzionic' -> the vault name is shown.
    assert!(
        long.contains("kv/S_KV (kv-scottzionic)"),
        "-l must surface the real vault name when it differs from the alias: {long}"
    );
    // Alias 'default' equals its vault -> no redundant suffix.
    assert!(long.contains("default/S_DEF"), "{long}");
    assert!(
        !long.contains("default/S_DEF ("),
        "no vault suffix when alias == vault: {long}"
    );
}

// ---------------------------------------------------------------------------
// 3. No-workspace long view is unchanged
// ---------------------------------------------------------------------------

#[test]
fn long_view_without_workspace_is_plain() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    ok(t, &["set", "AAA", "--value", "1"]);
    ok(t, &["set", "BBB", "--value", "2"]);

    // Single vault, no workspace: names render plainly — no `alias/` prefix and
    // no `(vault)` suffix (the union path is skipped entirely).
    let long = ok(t, &["--format", "table", "ls", "-l"]);
    assert!(long.contains("AAA") && long.contains("BBB"), "{long}");
    assert!(
        !long.contains("/AAA") && !long.contains("/BBB"),
        "no alias prefix without a workspace: {long}"
    );
    assert!(
        !long.contains('('),
        "no vault suffix without a workspace: {long}"
    );
}

// ---------------------------------------------------------------------------
// 4. `cx alias` — rename / reset the alias on an attached entry
// ---------------------------------------------------------------------------

/// The aliases currently attached, read from `cx ls --format json`.
fn cx_aliases(t: &Path) -> Vec<String> {
    let json = ok(t, &["cx", "ls", "--format", "json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("valid json");
    rows.iter()
        .filter_map(|r| r["alias"].as_str().map(String::from))
        .collect()
}

fn seed_kv_workspace(t: &Path) {
    ok(t, &["vault", "create", "kv-scottzionic"]);
    ok(t, &["cx", "add", "kv-scottzionic", "--alias", "kv"]);
}

#[test]
fn cx_alias_renames_non_default_and_default_entries() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    seed_kv_workspace(t);

    // Non-default entry: kv -> prod.
    ok(t, &["cx", "alias", "kv", "prod"]);
    let a = cx_aliases(t);
    assert!(
        a.contains(&"prod".to_string()) && !a.contains(&"kv".to_string()),
        "{a:?}"
    );

    // The default entry re-aliases like any other: default -> home.
    ok(t, &["cx", "alias", "default", "home"]);
    let a = cx_aliases(t);
    assert!(
        a.contains(&"home".to_string()) && a.contains(&"prod".to_string()),
        "{a:?}"
    );
    assert!(!a.contains(&"default".to_string()), "{a:?}");
}

#[test]
fn cx_alias_reset_restores_the_vault_name() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    seed_kv_workspace(t);

    ok(t, &["cx", "alias", "kv", "--reset"]);
    let a = cx_aliases(t);
    assert!(
        a.contains(&"kv-scottzionic".to_string()) && !a.contains(&"kv".to_string()),
        "--reset should restore the vault name: {a:?}"
    );
}

#[test]
fn cx_alias_looks_up_the_entry_by_vault_name() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    seed_kv_workspace(t);

    // The user thinks in vault names: address the entry by its vault, not alias.
    ok(t, &["cx", "alias", "kv-scottzionic", "renamed"]);
    assert!(cx_aliases(t).contains(&"renamed".to_string()));
}

#[test]
fn cx_alias_rejects_duplicate_and_backend_name_collisions() {
    let (_seed, temp) = xv_isolated_local();
    let t = temp.path();
    seed_kv_workspace(t);

    // Duplicate: 'default' is already the auto-attached entry's alias.
    let dup = run(t, &["cx", "alias", "kv", "default"]);
    assert!(!dup.status.success(), "duplicate alias must be rejected");
    assert!(
        String::from_utf8_lossy(&dup.stderr).contains("duplicate workspace alias"),
        "stderr: {}",
        String::from_utf8_lossy(&dup.stderr)
    );

    // Backend-name collision: 'local' is a registry backend name.
    let col = run(t, &["cx", "alias", "kv", "local"]);
    assert!(
        !col.status.success(),
        "backend-name collision must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&col.stderr).contains("collides with a registry backend name"),
        "stderr: {}",
        String::from_utf8_lossy(&col.stderr)
    );

    // Both rejections left the workspace untouched.
    assert!(
        cx_aliases(t).contains(&"kv".to_string()),
        "alias must be unchanged after rejection"
    );
}

#[test]
fn cx_alias_errors_from_an_xv_toml_workspace() {
    // A `.xv.toml`-sourced workspace is the explicit source of truth; cx
    // mutations must refuse and point at the file (cx add/rm/default precedent).
    let toml = r#"
default_env = "dev"

[env.dev]
vaults = [
  { vault = "projvault", backend = "local", alias = "proj", default = true },
]
"#;
    let (mut cmd, _temp) = xv_isolated_local_with_profile(toml);
    let out = cmd
        .args(["cx", "alias", "proj", "renamed"])
        .output()
        .expect("spawn xv");
    assert!(
        !out.status.success(),
        "cx alias must refuse a .xv.toml workspace"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(".xv.toml"),
        "expected the .xv.toml guard error: {stderr}"
    );
}
