//! Behavior lock for the Phase 2 / US-104 share migration: `xv share` and
//! `xv vault share` now route RBAC through the `VaultBackend` trait and gate on
//! `capabilities().has_rbac` (not a hardcoded backend kind). On a non-RBAC
//! backend (local) every share verb must fail with the capability-gated
//! message and the stable `InvalidArgument` exit code (2), never a panic or a
//! silent success.
//!
//! Fully hermetic against the local (age-encrypted file) backend.

mod common;

use common::{xv, xv_isolated_local};
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

/// Assert the command failed with exit code 2 (`InvalidArgument`) and its
/// stderr names the capability gap for `operation`.
fn assert_share_gated(out: &Output, operation: &str) {
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (InvalidArgument); stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("does not support {operation}")),
        "expected capability-gated message for '{operation}', got:\n{stderr}"
    );
}

#[test]
fn secret_share_grant_gated_on_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = run(
        temp.path(),
        &[
            "share",
            "grant",
            "SEC",
            "user@example.com",
            "--level",
            "reader",
        ],
    );
    assert_share_gated(&out, "access sharing");
}

#[test]
fn secret_share_revoke_gated_on_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = run(temp.path(), &["share", "revoke", "SEC", "user@example.com"]);
    assert_share_gated(&out, "access sharing");
}

#[test]
fn secret_share_list_gated_on_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = run(temp.path(), &["share", "list", "SEC"]);
    assert_share_gated(&out, "access sharing");
}

#[test]
fn vault_share_grant_gated_on_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = run(
        temp.path(),
        &[
            "vault",
            "share",
            "grant",
            "myvault",
            "user@example.com",
            "--level",
            "reader",
        ],
    );
    assert_share_gated(&out, "vault sharing");
}

#[test]
fn vault_share_list_gated_on_local() {
    let (_seed, temp) = xv_isolated_local();
    let out = run(temp.path(), &["vault", "share", "list", "myvault"]);
    assert_share_gated(&out, "vault sharing");
}
