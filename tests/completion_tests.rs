mod common;

#[test]
fn completion_bash_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "bash"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty(), "bash completion should be non-empty");
    // Bash completion scripts always reference `complete` and the binary.
    assert!(stdout.contains("complete"), "should contain 'complete' builtin: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_zsh_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "zsh"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    assert!(stdout.contains("compdef") || stdout.contains("_xv"),
        "zsh completion should contain compdef or _xv: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_fish_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "fish"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    assert!(stdout.contains("complete"),
        "fish completion should reference complete: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_powershell_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "powershell"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    // PowerShell completion uses Register-ArgumentCompleter.
    assert!(stdout.contains("Register-ArgumentCompleter") || stdout.to_lowercase().contains("powershell"),
        "powershell completion should reference Register-ArgumentCompleter: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_unknown_shell_exits_2() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "unknown-shell"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}
