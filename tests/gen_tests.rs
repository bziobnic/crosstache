//! Integration tests for the `xv gen` command.
//!
//! Tests marked `#[ignore]` require live Azure credentials and a configured default vault.
//! Run Azure tests with:
//!   cargo test --test gen_tests -- --ignored --nocapture --test-threads=1

#[cfg(test)]
mod gen_integration_tests {
    use std::process::Command;

    /// Helper: run the compiled `xv` binary with the given args.
    /// Returns (exit_success, stdout, stderr).
    fn run_xv(args: &[&str]) -> (bool, String, String) {
        let binary = env!("CARGO_BIN_EXE_xv");
        let output = Command::new(binary)
            .args(args)
            .output()
            .expect("failed to execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (output.status.success(), stdout, stderr)
    }

    /// Does not require Azure — just tests CLI argument validation.
    #[test]
    fn test_gen_length_too_short_fails() {
        let (ok, _, stderr) = run_xv(&["gen", "--length", "5", "--raw"]);
        assert!(!ok, "gen with length 5 should fail");
        assert!(
            stderr.contains("6") || stderr.contains("between"),
            "Error message should mention valid range: {stderr}"
        );
    }

    /// Does not require Azure — just tests CLI argument validation.
    #[test]
    fn test_gen_length_too_long_fails() {
        let (ok, _, stderr) = run_xv(&["gen", "--length", "101", "--raw"]);
        assert!(!ok, "gen with length 101 should fail");
        assert!(
            stderr.contains("100") || stderr.contains("between"),
            "Error message should mention valid range: {stderr}"
        );
    }

    /// Requires live Azure credentials and a configured default vault.
    #[test]
    #[ignore]
    fn test_gen_save_creates_secret_and_can_be_retrieved() {
        let test_secret_name = format!("xv-gen-test-{}", std::process::id());

        // Generate and save; --raw prints the value to stdout before the success message
        let (ok, stdout, _stderr) = run_xv(&[
            "gen",
            "--length", "16",
            "--charset", "alphanumeric",
            "--save", &test_secret_name,
            "--raw",
        ]);
        assert!(ok, "gen --save should succeed");
        // First non-empty line is the password (println! adds a newline)
        let password_line = stdout.lines().next().unwrap_or("").trim().to_string();
        assert_eq!(password_line.len(), 16, "Generated password should be 16 chars");

        // Retrieve and verify the saved secret matches
        let (ok2, stdout2, _) = run_xv(&["get", &test_secret_name, "--raw"]);
        assert!(ok2, "get should succeed after gen --save");
        assert_eq!(stdout2.trim(), password_line, "Retrieved value should match generated password");

        // Cleanup
        let (ok3, _, _) = run_xv(&["delete", &test_secret_name, "--force"]);
        assert!(ok3, "cleanup delete should succeed");
    }
}
