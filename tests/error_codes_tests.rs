//! Integration tests asserting the `xv` binary exits with the documented
//! exit code per error family. These tests build and run the binary.

use std::process::Command;

fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

#[test]
fn invalid_argument_exits_2() {
    let out = xv().args(["--this-flag-does-not-exist"]).output().unwrap();
    assert!(!out.status.success());
    // clap parse failures use exit 2 on its own; we rely on that being our
    // family code as well, which the new exit_code() preserves.
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = xv().args(["this-subcommand-does-not-exist"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}
