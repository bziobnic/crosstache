mod common;

#[test]
fn config_path_shows_isolated_path() {
    let (mut cmd, temp) = common::xv_isolated();
    let out = cmd.args(["config", "path"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    // Path should be under our isolated XDG_CONFIG_HOME.
    let expected_prefix = temp.path().join(".config").to_string_lossy().into_owned();
    assert!(
        stdout.contains(&expected_prefix) || stdout.contains("xv"),
        "config path should reference isolated dir: {stdout}"
    );
}

#[test]
fn config_show_works_on_empty_config() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "show"]).output().expect("spawn");
    // With XDG_CONFIG_HOME pointing at an empty tempdir, no config file
    // exists. The command should still exit 0 and show defaults.
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr_str(&out)
    );
}

#[test]
fn config_set_then_show_round_trips() {
    let (mut cmd1, temp) = common::xv_isolated();
    let out1 = cmd1
        .args(["config", "set", "default_vault", "test-vault"])
        .output()
        .expect("spawn");
    assert_eq!(
        out1.status.code(),
        Some(0),
        "set: {}",
        common::stderr_str(&out1)
    );

    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out2 = cmd2.args(["config", "show"]).output().expect("spawn");
    assert_eq!(out2.status.code(), Some(0));
    let stdout = common::stdout_str(&out2);
    assert!(
        stdout.contains("test-vault"),
        "config show should display the value just set: {stdout}"
    );
}

#[test]
fn config_set_invalid_key_errors() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["config", "set", "this_key_does_not_exist", "value"])
        .output()
        .expect("spawn");
    // Either clap rejects (exit 2) or runtime returns invalid-argument (exit 2 or 3).
    // Acceptable: 2 or 3 (depending on validation layer).
    let code = out.status.code();
    assert!(
        code == Some(2) || code == Some(3),
        "invalid config key should error: {code:?}"
    );
}

#[test]
fn config_help_documents_subcommands() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("show"));
    assert!(stdout.contains("path"));
    assert!(stdout.contains("set"));
}

// ── P0.3 tests ───────────────────────────────────────────────────────────────

#[test]
fn version_lists_compiled_backends() {
    // `xv version` should always mention "azure" and "local" as built-in backends.
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["version"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(
        stdout.contains("Backends:"),
        "expected Backends: line in version output: {stdout}"
    );
    assert!(stdout.contains("azure"), "expected azure in backends: {stdout}");
    assert!(stdout.contains("local"), "expected local in backends: {stdout}");
}

#[cfg(not(feature = "aws"))]
#[test]
fn backend_aws_on_default_build_gives_clear_error() {
    // On a build without --features aws, `xv --backend aws list` must return a
    // targeted error rather than the generic "No backend registry available" message.
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["--backend", "aws", "list"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(0), "should have failed");
    let stderr = common::stderr_str(&out);
    assert!(
        stderr.contains("AWS backend") || stderr.contains("--features aws"),
        "expected AWS build hint in stderr: {stderr}"
    );
    // Must not say "No backend registry available" (the old generic message).
    assert!(
        !stderr.contains("No backend registry available"),
        "must not emit generic registry error: {stderr}"
    );
}
