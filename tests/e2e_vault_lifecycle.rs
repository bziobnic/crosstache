//! Behavior-lock e2e for the vault lifecycle on the **local** backend, which
//! had no e2e coverage before Phase 2 migrated vault create/list/info/delete
//! onto the `VaultBackend` trait. Drives the real `xv` binary in an isolated
//! temp env (no Azure, no network): create → list (shows it) → info (shows
//! details) → delete (round-trip), including the non-interactive delete refusal.
//!
//! Note on the safe-delete warning UX: the Azure soft-delete/purge-protection
//! warnings live on the Azure path only; the local backend has no soft-delete
//! recovery window, so its delete goes through the shared `confirm_destructive`
//! guard (prompt / `--force`, hard-refuse in a non-TTY) with no extra warning.

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

fn run_ok(temp: &Path, args: &[&str]) -> Output {
    let out = run(temp, args);
    assert!(
        out.status.success(),
        "xv {args:?} must succeed:\nstdout: {}\nstderr: {}",
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

#[test]
fn vault_create_list_info_delete_round_trip() {
    let (_seed, temp) = xv_isolated_local();
    let temp = temp.path();
    let vault = "lifecyclevault";

    // create
    let create = run_ok(temp, &["vault", "create", vault]);
    assert!(
        combined(&create).contains(vault),
        "create output must name the vault:\n{}",
        combined(&create)
    );

    // list shows it
    let list = run_ok(temp, &["vault", "list"]);
    assert!(
        combined(&list).contains(vault),
        "`vault list` must show the created vault:\n{}",
        combined(&list)
    );

    // info shows details for it
    let info = run_ok(temp, &["vault", "info", vault]);
    let info_out = combined(&info);
    assert!(
        info_out.contains(vault),
        "`vault info` must name the vault:\n{info_out}"
    );
    assert!(
        info_out.contains("local"),
        "`vault info` must show the local backend location:\n{info_out}"
    );

    // delete WITHOUT --force in a non-interactive session is refused (exit 2),
    // and the vault must still exist afterwards.
    let refused = run(temp, &["vault", "delete", vault]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "non-interactive delete without --force must exit 2:\n{}",
        combined(&refused)
    );
    assert!(
        combined(&refused).to_lowercase().contains("force"),
        "refusal must point at --force:\n{}",
        combined(&refused)
    );
    assert!(
        combined(&run_ok(temp, &["vault", "list"])).contains(vault),
        "vault must still exist after a refused delete"
    );

    // delete WITH --force succeeds…
    let deleted = run_ok(temp, &["vault", "delete", vault, "--force"]);
    assert!(
        combined(&deleted).contains(vault),
        "delete output must name the vault:\n{}",
        combined(&deleted)
    );

    // …and the vault is gone from the listing.
    assert!(
        !combined(&run_ok(temp, &["vault", "list"])).contains(vault),
        "vault must be absent from the listing after delete"
    );
}
