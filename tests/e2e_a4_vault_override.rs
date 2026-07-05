//! A4 `--vault` composition (Phase 2 / US-102): an explicit `--vault` on
//! `run`/`inject`/`rotate` OVERRIDES the degenerate default entry — it targets
//! exactly that vault, never adds an entry, and never errors merely because of
//! the override. Without the flag, the verb targets the context/config default
//! vault, unchanged.
//!
//! Fully hermetic against the **local** (age-encrypted file) backend: two
//! vaults (`default` and `other`) each hold a secret `TARGET` with a distinct
//! value, and every assertion pins which vault a verb read from or wrote to by
//! the value it observed.

mod common;

use common::{xv, xv_isolated_local};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Build a fresh isolated `xv` command against the SAME local-backend env that
/// [`xv_isolated_local`] set up. Mirrors that helper's env exactly so repeated
/// commands hit the same hermetic store (same pattern as
/// `e2e_degenerate_workspace.rs::local_cmd`).
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

fn run_ok(temp: &Path, args: &[&str]) -> Output {
    let out = local_cmd(temp).args(args).output().expect("spawn xv");
    assert!(
        out.status.success(),
        "xv {args:?} must succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

/// Run `xv` feeding `stdin`, asserting success — used for `inject` reading a
/// template from stdin.
fn run_ok_stdin(temp: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = local_cmd(temp)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xv");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait xv");
    assert!(
        out.status.success(),
        "xv {args:?} (stdin) must succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Seed two vaults: `default` (config default) with `TARGET=from_default`, and
/// a freshly created `other` with `TARGET=from_other`. Writing to `other` goes
/// through `context use` because the local backend requires a vault to exist
/// before a `set`, and the degenerate seam resolves bare names literally
/// against the default entry (no colon addressing without a workspace).
fn seed_two_vaults(temp: &Path) {
    run_ok(temp, &["set", "TARGET=from_default"]);
    run_ok(temp, &["vault", "create", "other"]);
    run_ok(temp, &["context", "use", "other"]);
    run_ok(temp, &["set", "TARGET=from_other"]);
    run_ok(temp, &["context", "use", "default"]);
}

/// `xv run --no-masking -- sh -c 'printf GOT[%s] "$TARGET"'` prints the
/// injected value inside a marker; returns the combined child+xv output.
fn run_target(temp: &Path, extra: &[&str]) -> String {
    let mut args = vec!["run", "--no-masking"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "sh", "-c", "printf GOT[%s] \"$TARGET\""]);
    combined(&run_ok(temp, &args))
}

#[test]
fn run_vault_override_targets_named_vault() {
    let (_seed, temp) = xv_isolated_local();
    seed_two_vaults(temp.path());

    // No flag → default vault.
    let default_out = run_target(temp.path(), &[]);
    assert!(
        default_out.contains("GOT[from_default]"),
        "bare `run` must inject the default vault's TARGET:\n{default_out}"
    );
    assert!(
        !default_out.contains("from_other"),
        "bare `run` must NOT read the other vault:\n{default_out}"
    );

    // --vault other → the override vault.
    let other_out = run_target(temp.path(), &["--vault", "other"]);
    assert!(
        other_out.contains("GOT[from_other]"),
        "`run --vault other` must inject the other vault's TARGET:\n{other_out}"
    );
    assert!(
        !other_out.contains("from_default"),
        "`run --vault other` must NOT read the default vault:\n{other_out}"
    );
}

#[test]
fn inject_vault_override_targets_named_vault() {
    let (_seed, temp) = xv_isolated_local();
    seed_two_vaults(temp.path());

    let template = "value={{ secret:TARGET }}";

    // No flag → default vault.
    let default_out = String::from_utf8(run_ok_stdin(temp.path(), &["inject"], template).stdout)
        .expect("stdout utf-8");
    assert!(
        default_out.contains("value=from_default"),
        "bare `inject` must render the default vault's TARGET:\n{default_out}"
    );

    // --vault other → the override vault.
    let other_out = String::from_utf8(
        run_ok_stdin(temp.path(), &["inject", "--vault", "other"], template).stdout,
    )
    .expect("stdout utf-8");
    assert!(
        other_out.contains("value=from_other"),
        "`inject --vault other` must render the other vault's TARGET:\n{other_out}"
    );
}

#[test]
fn rotate_vault_override_targets_named_vault() {
    let (_seed, temp) = xv_isolated_local();
    seed_two_vaults(temp.path());

    // Rotate ONLY the other vault's TARGET.
    run_ok(
        temp.path(),
        &["rotate", "TARGET", "--vault", "other", "--force"],
    );

    // The default vault is untouched by the override rotate.
    let default_after = run_target(temp.path(), &[]);
    assert!(
        default_after.contains("GOT[from_default]"),
        "`rotate --vault other` must NOT rotate the default vault:\n{default_after}"
    );

    // The other vault's value changed (rotation generated a new value).
    let other_after = run_target(temp.path(), &["--vault", "other"]);
    assert!(
        !other_after.contains("from_other") && !other_after.contains("from_default"),
        "`rotate --vault other` must have rotated the other vault's TARGET:\n{other_after}"
    );

    // Now rotate WITHOUT the flag: it targets the default vault, leaving the
    // other vault's (already-rotated) value alone.
    run_ok(temp.path(), &["rotate", "TARGET", "--force"]);
    let default_final = run_target(temp.path(), &[]);
    assert!(
        !default_final.contains("from_default"),
        "bare `rotate` must rotate the default vault's TARGET:\n{default_final}"
    );
}
