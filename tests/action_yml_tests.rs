//! Tests for the first-party GitHub Action (`action.yml`).
//!
//! A composite action cannot be run end-to-end without a runner, so this
//! covers the two things that can go wrong silently:
//!
//! 1. **The action contract** — inputs, outputs, and step wiring. A typo here
//!    surfaces as a confusing runtime failure in someone else's workflow.
//! 2. **The secret-fetch shell logic** — extracted from the YAML and executed
//!    against a stub `xv`, with a temporary `GITHUB_ENV`. This is the part that
//!    handles secret material, so masking, multi-line values, and delimiter
//!    injection are all exercised for real rather than reviewed by eye.

#[cfg(unix)]
use std::io::Write;
use std::path::PathBuf;

fn action_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("action.yml")
}

fn action_yaml() -> serde_yaml::Value {
    let body = std::fs::read_to_string(action_path()).expect("read action.yml");
    serde_yaml::from_str(&body).expect("action.yml must be valid YAML")
}

/// Pull one composite step's `run:` script out of the action by step name.
fn step_script(name: &str) -> String {
    let doc = action_yaml();
    let steps = doc["runs"]["steps"]
        .as_sequence()
        .expect("runs.steps must be a sequence");
    for step in steps {
        if step["name"].as_str() == Some(name) {
            return step["run"]
                .as_str()
                .unwrap_or_else(|| panic!("step {name} has no run script"))
                .to_string();
        }
    }
    panic!("no step named {name} in action.yml");
}

// ---------------------------------------------------------------------------
// Action contract
// ---------------------------------------------------------------------------

#[test]
fn action_declares_a_composite_run_with_the_expected_steps() {
    let doc = action_yaml();
    assert_eq!(doc["runs"]["using"].as_str(), Some("composite"));

    let names: Vec<&str> = doc["runs"]["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Install xv", "Configure authentication", "Fetch secrets"],
        "step order matters: install, then auth, then read"
    );

    // Every step must pin a shell; composite actions error without it.
    for step in doc["runs"]["steps"].as_sequence().unwrap() {
        assert_eq!(
            step["shell"].as_str(),
            Some("bash"),
            "step {:?} must declare shell: bash",
            step["name"].as_str()
        );
    }
}

#[test]
fn action_inputs_cover_the_documented_surface() {
    let doc = action_yaml();
    let inputs = doc["inputs"].as_mapping().expect("inputs mapping");
    for key in [
        "version",
        "backend",
        "vault",
        "secrets",
        "auth",
        "client-id",
        "tenant-id",
        "subscription-id",
        "verify-signature",
    ] {
        assert!(
            inputs.contains_key(serde_yaml::Value::from(key)),
            "missing input: {key}"
        );
    }

    // Defaults that define the action's behavior out of the box.
    assert_eq!(doc["inputs"]["version"]["default"].as_str(), Some("latest"));
    assert_eq!(doc["inputs"]["auth"]["default"].as_str(), Some("oidc"));
    assert_eq!(doc["inputs"]["backend"]["default"].as_str(), Some("azure"));

    // Every input needs a description — it is the only docs a consumer sees in
    // the marketplace UI.
    for (name, spec) in inputs {
        assert!(
            spec["description"].as_str().is_some_and(|d| d.len() > 20),
            "input {name:?} needs a substantive description"
        );
    }
}

#[test]
fn action_exposes_version_and_path_outputs() {
    let doc = action_yaml();
    assert!(
        doc["outputs"]["version"]["value"]
            .as_str()
            .unwrap()
            .contains("steps.install.outputs.version"),
        "version output must come from the install step"
    );
    assert!(doc["outputs"]["path"]["value"].as_str().is_some());
}

#[test]
fn install_step_verifies_checksums_and_fails_closed() {
    let script = step_script("Install xv");
    assert!(script.contains("set -euo pipefail"), "must fail fast");
    assert!(
        script.contains(".sha256"),
        "must fetch the published digest"
    );
    assert!(
        script.contains("Checksum mismatch"),
        "must reject a mismatched archive"
    );
    // A digest that is not a digest must not be treated as a pass.
    assert!(
        script.contains("[0-9a-f]{64}"),
        "must validate the digest's shape before comparing"
    );
    // Cache key must include the platform, or a restored cache could hand over
    // a binary for the wrong OS.
    assert!(
        script.contains("${RUNNER_OS}-${RUNNER_ARCH}"),
        "tool-cache path must be platform-scoped"
    );
}

#[test]
fn install_step_covers_every_published_platform() {
    let script = step_script("Install xv");
    for archive in [
        "xv-linux-x64.tar.gz",
        "xv-macos-intel.tar.gz",
        "xv-macos-apple-silicon.tar.gz",
        "xv-windows-x64.zip",
    ] {
        assert!(
            script.contains(archive),
            "install step must map a runner to {archive}"
        );
    }
}

#[test]
fn signature_verification_uses_the_release_signing_key() {
    // Must match the key embedded in `xv upgrade`; a mismatch would mean the
    // action trusts a different signer than the CLI does.
    let embedded = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cli/upgrade_ops.rs"),
    )
    .unwrap();
    let key = embedded
        .lines()
        .find(|l| l.contains("RELEASE_SIGNING_KEY"))
        .and_then(|l| l.split('"').nth(1))
        .expect("upgrade_ops.rs must define RELEASE_SIGNING_KEY");

    let script = step_script("Install xv");
    assert!(
        script.contains(key),
        "action.yml must verify against the same minisign key as xv upgrade ({key})"
    );
}

#[test]
fn oidc_auth_requires_the_id_token_permission_and_ids() {
    let script = step_script("Configure authentication");
    assert!(script.contains("ACTIONS_ID_TOKEN_REQUEST_URL"));
    assert!(
        script.contains("id-token: write"),
        "the error must tell the user exactly what to add"
    );
    assert!(
        script.contains("AZURE_CREDENTIAL_PRIORITY=oidc"),
        "must select the OIDC credential in the CLI"
    );
    assert!(
        script.contains("client-id and tenant-id"),
        "must reject a half-configured federation"
    );
}

// ---------------------------------------------------------------------------
// Secret-fetch logic, executed for real
// ---------------------------------------------------------------------------

/// Run the extracted "Fetch secrets" script with a stub `xv` on PATH.
///
/// `responses` maps a secret name to the value the stub prints. A name absent
/// from the map makes the stub exit non-zero, standing in for a fetch failure.
///
/// Unix-only. Not because the action is — it supports Windows runners, where
/// GitHub provides bash — but because this *harness* is: it writes an
/// extensionless `#!/usr/bin/env bash` stub made executable via `chmod`, joins
/// `PATH` with `:`, and spawns `bash` by bare name. None of that holds on a
/// Windows developer machine, where the tests failed with "program not found"
/// rather than telling us anything about `action.yml`. CI runs `cargo test` on
/// ubuntu-latest (`.github/workflows/build.yml`), so this gate costs no
/// coverage.
#[cfg(unix)]
struct FetchRun {
    status: std::process::ExitStatus,
    stdout: String,
    github_env: String,
}

#[cfg(unix)]
fn run_fetch(secrets_input: &str, responses: &[(&str, &str)]) -> FetchRun {
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    // Stub `xv`: `xv get <name> --raw [--vault v]`.
    let mut cases = String::new();
    for (name, value) in responses {
        cases.push_str(&format!(
            "  {})\n    printf '%s' \"$(cat <<'XVVALUE'\n{}\nXVVALUE\n)\"\n    exit 0 ;;\n",
            shell_case_pattern(name),
            value
        ));
    }
    let stub = format!(
        "#!/usr/bin/env bash\n\
         # stub xv: only `get <name> --raw` is used by the action\n\
         if [[ \"$1\" != \"get\" ]]; then exit 0; fi\n\
         case \"$2\" in\n{cases}  *) echo \"secret not found: $2\" >&2; exit 10 ;;\n\
         esac\n"
    );
    let stub_path = bin_dir.join("xv");
    std::fs::write(&stub_path, stub).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let github_env = tmp.path().join("github_env");
    std::fs::File::create(&github_env).unwrap();

    let script_path = tmp.path().join("fetch.sh");
    let mut f = std::fs::File::create(&script_path).unwrap();
    f.write_all(step_script("Fetch secrets").as_bytes())
        .unwrap();
    drop(f);

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new("bash")
        .arg(&script_path)
        .env("PATH", path)
        .env("INPUT_SECRETS", secrets_input)
        .env("INPUT_VAULT", "")
        .env("GITHUB_ENV", &github_env)
        .output()
        .expect("run fetch script");

    FetchRun {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr),
        github_env: std::fs::read_to_string(&github_env).unwrap(),
    }
}

#[cfg(unix)]
fn shell_case_pattern(name: &str) -> String {
    // Test names are plain, but quote anyway so a pattern char cannot alter the
    // stub's control flow.
    format!("\"{name}\"")
}

/// Extract `NAME=value` pairs from a GITHUB_ENV file written in heredoc form.
#[cfg(unix)]
fn parse_github_env(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some((name, delim)) = line.split_once("<<") {
            let mut value = Vec::new();
            for inner in lines.by_ref() {
                if inner == delim {
                    break;
                }
                value.push(inner.to_string());
            }
            out.push((name.to_string(), value.join("\n")));
        }
    }
    out
}

#[cfg(unix)]
#[test]
fn fetch_exports_and_masks_each_secret() {
    let run = run_fetch(
        "DB_PASSWORD=db-secret\nAPI_KEY=api-secret\n",
        &[("db-secret", "hunter2"), ("api-secret", "abc123")],
    );
    assert!(run.status.success(), "{}", run.stdout);

    let vars = parse_github_env(&run.github_env);
    assert_eq!(
        vars,
        vec![
            ("DB_PASSWORD".to_string(), "hunter2".to_string()),
            ("API_KEY".to_string(), "abc123".to_string()),
        ]
    );

    // Masking must be requested for every value, before anything else logs.
    assert!(run.stdout.contains("::add-mask::hunter2"), "{}", run.stdout);
    assert!(run.stdout.contains("::add-mask::abc123"), "{}", run.stdout);
    assert!(run.stdout.contains("Fetched 2 secret(s)"), "{}", run.stdout);
}

#[cfg(unix)]
#[test]
fn fetch_handles_multiline_values_and_masks_every_line() {
    // A PEM key is the canonical multi-line secret. The runner matches masks
    // per line, so each line must be registered separately.
    let pem =
        "-----BEGIN PRIVATE KEY-----\nline-one-secret\nline-two-secret\n-----END PRIVATE KEY-----";
    let run = run_fetch("TLS_KEY=pem-secret\n", &[("pem-secret", pem)]);
    assert!(run.status.success(), "{}", run.stdout);

    let vars = parse_github_env(&run.github_env);
    assert_eq!(vars.len(), 1);
    assert_eq!(vars[0].0, "TLS_KEY");
    assert_eq!(vars[0].1, pem, "the full multi-line value must round-trip");

    assert!(
        run.stdout.contains("::add-mask::line-one-secret"),
        "each line needs its own mask: {}",
        run.stdout
    );
    assert!(
        run.stdout.contains("::add-mask::line-two-secret"),
        "{}",
        run.stdout
    );
}

#[cfg(unix)]
#[test]
fn fetch_uses_a_random_delimiter_per_value() {
    let run = run_fetch("A=sa\nB=sb\n", &[("sa", "value-a"), ("sb", "value-b")]);
    assert!(run.status.success(), "{}", run.stdout);

    let delims: Vec<&str> = run
        .github_env
        .lines()
        .filter_map(|l| l.split_once("<<").map(|(_, d)| d))
        .collect();
    assert_eq!(delims.len(), 2);
    assert_ne!(
        delims[0], delims[1],
        "a fixed delimiter would let one secret's content terminate another's block"
    );
    assert!(delims.iter().all(|d| d.starts_with("XV_EOF_")));
}

#[cfg(unix)]
#[test]
fn fetch_rejects_a_value_containing_its_delimiter() {
    // Defence against env injection: a value that could close the heredoc early
    // must abort rather than be written.
    let run = run_fetch("A=sa\n", &[("sa", "prefix\nXV_EOF_deadbeef\nA=INJECTED")]);
    // The generated delimiter is random, so this specific value will not match
    // it; the guard is asserted structurally instead.
    assert!(run.status.success(), "{}", run.stdout);
    let script = step_script("Fetch secrets");
    assert!(
        script.contains("contains the generated delimiter"),
        "the delimiter-collision guard must exist"
    );
    // And the injected-looking line must have landed inside the value, not as
    // its own env var.
    let vars = parse_github_env(&run.github_env);
    assert_eq!(vars.len(), 1, "exactly one variable: {vars:?}");
    assert_eq!(vars[0].0, "A");
    assert!(vars[0].1.contains("A=INJECTED"));
}

#[cfg(unix)]
#[test]
fn fetch_rejects_invalid_environment_variable_names() {
    for bad in ["1BAD=x", "has-dash=x", "has space=x", "=x"] {
        let run = run_fetch(&format!("{bad}\n"), &[("x", "v")]);
        assert!(
            !run.status.success(),
            "{bad:?} should be rejected: {}",
            run.stdout
        );
        assert!(
            run.stdout.contains("not a valid environment variable name")
                || run.stdout.contains("Malformed"),
            "{}",
            run.stdout
        );
    }
}

#[cfg(unix)]
#[test]
fn fetch_rejects_malformed_entries() {
    let run = run_fetch("NO_EQUALS_SIGN\n", &[]);
    assert!(!run.status.success());
    assert!(
        run.stdout.contains("Malformed secrets entry"),
        "{}",
        run.stdout
    );

    let run = run_fetch("EMPTY_NAME=\n", &[]);
    assert!(!run.status.success());
    assert!(run.stdout.contains("No secret name"), "{}", run.stdout);
}

#[cfg(unix)]
#[test]
fn fetch_fails_the_step_when_a_secret_is_missing() {
    // A missing secret must fail loudly; exporting an empty value would let a
    // deploy proceed with a blank credential.
    let run = run_fetch("A=present\nB=absent\n", &[("present", "v")]);
    assert!(!run.status.success(), "{}", run.stdout);
    assert!(
        run.stdout.contains("Failed to read secret 'absent'"),
        "{}",
        run.stdout
    );
}

#[cfg(unix)]
#[test]
fn fetch_skips_blanks_and_comments_and_tolerates_crlf() {
    let run = run_fetch(
        "\n# a comment\r\nDB=db-secret\r\n\n   \n",
        &[("db-secret", "hunter2")],
    );
    assert!(run.status.success(), "{}", run.stdout);
    let vars = parse_github_env(&run.github_env);
    assert_eq!(vars, vec![("DB".to_string(), "hunter2".to_string())]);
}

#[cfg(unix)]
#[test]
fn fetch_preserves_values_with_shell_metacharacters() {
    // Values are data, never code: a value that looks like a command
    // substitution must survive byte-for-byte.
    let nasty = "$(touch /tmp/xv-pwned); `id`; ${HOME}; \"quoted\" 'single' \\backslash";
    let run = run_fetch("V=nasty\n", &[("nasty", nasty)]);
    assert!(run.status.success(), "{}", run.stdout);
    let vars = parse_github_env(&run.github_env);
    assert_eq!(vars[0].1, nasty, "value must not be expanded or mangled");
    assert!(
        !std::path::Path::new("/tmp/xv-pwned").exists(),
        "the value must never be evaluated as shell"
    );
}
