//! Phase 1 (B6) convergence tests: the **degenerate workspace-of-one** — a
//! bare `xv` invocation with no `cx`/`.xv.toml` workspace configured — must
//! behave byte-identically to pre-workspace single-vault usage now that every
//! command resolves through the single (collapsed) workspace seam.
//!
//! Overlap note (deliberately NOT duplicated here):
//! - Exact byte-identity of bare/colon `set`/`get` stdout+stderr is pinned by
//!   `tests/e2e_workspaces.rs::no_workspace_byte_identical` (golden output).
//! - Union `ls`/`find` for CONFIGURED multi-vault workspaces (the branch a
//!   degenerate workspace must NOT take) is covered throughout
//!   `tests/e2e_workspaces.rs`.
//! - `run`/`inject` alias resolution WITH a configured workspace is covered by
//!   `e2e_workspaces.rs::inject_alias_uri_resolves` /
//!   `inject_raw_vault_name_still_works_when_no_alias_matches`.
//!
//! These tests fill the degenerate-specific gaps: `context use` bootstrap with
//! no workspace (guarding the `config_ops.rs` presence-gate regression),
//! single-vault `ls` output SHAPE, and no-workspace `xv://` URI resolution
//! through the collapsed seam.

mod common;

use common::{xv, xv_isolated, xv_isolated_local};
use std::path::Path;
use std::process::Command;

/// Build a fresh isolated `xv` command against the SAME local-backend env that
/// [`xv_isolated_local`] set up (its `xv.conf` already lives under
/// `temp/.config/xv/`). Mirrors that helper's env exactly so repeated commands
/// (`set` then `get` then `ls`) hit the same hermetic store.
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

fn run_ok(temp: &Path, args: &[&str]) -> std::process::Output {
    let out = local_cmd(temp).args(args).output().expect("spawn xv");
    assert!(
        out.status.success(),
        "xv {args:?} must succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout utf-8")
}

// ---------------------------------------------------------------------------
// 1. `context use` bootstrap with no configured workspace
// ---------------------------------------------------------------------------

/// Regression guard for the `config_ops.rs` `context use` presence gate. Mid
/// branch this gate resolved the degenerate workspace and (via the naive
/// `is_some()` form) tripped "a multi-vault workspace is attached", so
/// `xv context use <vault>` errored in an env that had no workspace at all —
/// you could not set your first vault. The gate now consults
/// `resolve_configured_workspace` (which is `None` here), so it must succeed.
#[test]
fn context_use_with_no_configured_workspace_succeeds_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = local_cmd(temp.path())
        .args(["context", "use", "myvault"])
        .output()
        .expect("spawn xv");
    assert!(
        out.status.success(),
        "`context use` in a fresh no-workspace env must succeed:\nstdout: {}\nstderr: {}",
        stdout_of(&out),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The harder variant of the same regression: an ACTIVE Azure backend with no
/// vault configured. The degenerate builder raises the Azure no-vault
/// hard-error, so if the presence gate went through the converged
/// `resolve_workspace` it would propagate that error and fail `context use`
/// before the vault is ever set. `resolve_configured_workspace` does not build
/// the degenerate, so this must succeed without ever touching the network
/// (`context use` only writes the local context store).
#[test]
fn context_use_with_no_workspace_azure_no_vault_succeeds() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["context", "use", "some-azure-vault"])
        .output()
        .expect("spawn xv");
    assert!(
        out.status.success(),
        "`context use` on an unconfigured Azure env (no vault) must succeed \
         (the degenerate no-vault hard-error must NOT reach this presence gate):\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---------------------------------------------------------------------------
// 2. Single-vault degenerate `ls` output shape parity
// ---------------------------------------------------------------------------

/// A degenerate `ls` must render in the pre-workspace single-vault shape — a
/// FLAT array under `--format json` (not the per-vault grouping a configured
/// multi-vault union `ls` produces) — and list the secret at exit 0. This is
/// the observable half of the "single-entry degenerate `ls` matches
/// no-workspace `ls`" acceptance (the cache-key-identity half is unit-tested in
/// `src/workspace/mod.rs`).
#[test]
fn degenerate_ls_is_flat_single_vault_shape() {
    let (_seed, temp) = xv_isolated_local();
    run_ok(temp.path(), &["set", "ALPHA", "--value", "v1"]);
    run_ok(temp.path(), &["set", "BETA", "--value", "v2"]);

    // JSON: a flat array of secret objects, exactly like the pre-workspace
    // no-workspace read path (cf. `e2e_local_backend.rs::list_json_format`).
    let json_out = run_ok(temp.path(), &["ls", "--format", "json"]);
    let json = stdout_of(&json_out);
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("degenerate `ls --format json` must be valid JSON");
    assert!(
        parsed.is_array(),
        "degenerate `ls --format json` must be a FLAT array (single-vault shape), \
         not a per-vault grouped object: {json}"
    );
    assert!(json.contains("ALPHA") && json.contains("BETA"), "{json}");

    // names-only: a plain newline list of bare names — no `alias/name`
    // qualification that the multi-vault union view prefixes rows with.
    let names_out = run_ok(temp.path(), &["ls", "--names-only"]);
    let names = stdout_of(&names_out);
    assert!(
        names.lines().any(|l| l.trim() == "ALPHA"),
        "names-only must list the bare name `ALPHA` (no alias prefix): {names}"
    );
    assert!(
        !names.contains('/'),
        "degenerate names-only must not qualify names with an `alias/` prefix: {names}"
    );
}

// ---------------------------------------------------------------------------
// 3. No-workspace `xv://` URI resolution (run + inject) parity
// ---------------------------------------------------------------------------

/// `xv inject` of a bare `xv://<vault>/<name>` URI with no workspace attached
/// must resolve byte-identically to pre-workspace behaviour: post-collapse
/// `resolve_workspace_and_registry` returns `(None, None)` for the degenerate
/// case, so the URI falls straight through to the raw-vault-name resolver.
#[test]
fn no_workspace_inject_resolves_bare_xv_uri() {
    let (_seed, temp) = xv_isolated_local();
    run_ok(temp.path(), &["set", "TOKEN", "--value", "sekret"]);

    let template_path = temp.path().join("tpl.txt");
    std::fs::write(&template_path, "value: xv://default/TOKEN\n").expect("write template");
    let out_path = temp.path().join("out.txt");
    run_ok(
        temp.path(),
        &[
            "inject",
            "--template",
            template_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ],
    );
    let rendered = std::fs::read_to_string(&out_path).expect("read rendered output");
    assert!(
        rendered.contains("sekret"),
        "no-workspace `xv://default/TOKEN` must resolve to the secret value: {rendered}"
    );
}

/// `xv run` resolving an inherited `xv://` reference (parent-env var +
/// `--inherit-env`, mirroring `e2e_local_backend.rs::run_aborts_on_failing_uri_reference`
/// but with an EXISTING secret) must succeed and hand the child the resolved
/// value — proving the no-workspace run path stays byte-identical through the
/// collapsed seam.
#[test]
fn no_workspace_run_resolves_inherited_xv_uri() {
    let (_seed, temp) = xv_isolated_local();
    run_ok(temp.path(), &["set", "RUNSECRET", "--value", "runval"]);

    let marker = temp.path().join("child_saw.txt");
    let out = local_cmd(temp.path())
        .env("REF_VAR", "xv://default/RUNSECRET")
        .args([
            "run",
            "--inherit-env",
            "--",
            "sh",
            "-c",
            &format!("printf '%s' \"$REF_VAR\" > '{}'", marker.display()),
        ])
        .output()
        .expect("spawn xv run");
    assert!(
        out.status.success(),
        "`run` must resolve xv://default/RUNSECRET with no workspace:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let child_saw = std::fs::read_to_string(&marker).expect("child marker");
    assert_eq!(
        child_saw, "runval",
        "child process must see the resolved secret value in place of the xv:// reference"
    );
}

// ---------------------------------------------------------------------------
// 4. Bare set/get round-trip incl. colon-literal (degenerate has no aliases)
// ---------------------------------------------------------------------------

/// Degenerate `set`/`get` round-trips a bare name AND a `:`-containing name.
/// A workspace-of-one has no user aliases, so a colon is part of the LITERAL
/// secret name (never an `alias:path` qualifier) — the behaviour the
/// literal-degenerate branch in `resolve_secret_target` restores. Exact golden
/// stdout/stderr for this is pinned separately by
/// `e2e_workspaces.rs::no_workspace_byte_identical`; here we assert the
/// end-to-end round-trip via the shared local helper.
#[test]
fn degenerate_bare_and_colon_literal_roundtrip() {
    let (_seed, temp) = xv_isolated_local();

    run_ok(temp.path(), &["set", "FOO", "--value", "bar"]);
    let got = run_ok(temp.path(), &["get", "FOO", "--raw"]);
    assert_eq!(stdout_of(&got), "bar");

    run_ok(temp.path(), &["set", "lit:with:colons", "--value", "v1"]);
    let got_colon = run_ok(temp.path(), &["get", "lit:with:colons", "--raw"]);
    assert_eq!(
        stdout_of(&got_colon),
        "v1",
        "a `:`-containing name must be stored and read as a LITERAL name under the \
         degenerate workspace-of-one (no alias split)"
    );

    // And it is genuinely one literal secret, not two vaults' worth of split
    // addressing: both names show up as plain rows in a flat `ls`.
    let names = stdout_of(&run_ok(temp.path(), &["ls", "--names-only"]));
    assert!(names.lines().any(|l| l.trim() == "FOO"), "{names}");
    assert!(
        names.lines().any(|l| l.trim() == "lit:with:colons"),
        "colon name must appear verbatim as a single literal secret: {names}"
    );
}
