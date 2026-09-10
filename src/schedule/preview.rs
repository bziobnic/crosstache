//! The `xv schedule install --print` preview.
//!
//! `--print` is a read-only rehearsal of an install: it renders the exact
//! manifest and the exact native unit(s) that installation would write, and
//! writes nothing. Keeping it in its own pure function has two payoffs — the
//! output can be asserted against the golden with fixed inputs, and switching
//! the *installed* command over to the pinned runner is a change in the caller
//! rather than another redesign of what the user reads.

use crate::schedule::manifest::{serialize_manifest_preview, ScheduleManifestV1};
use crate::schedule::{self, Platform, RotationSchedule, UnitPaths};

/// Column the header values start at, so the labels line up as in the golden.
const LABEL_WIDTH: usize = 11;

fn header(label: &str, value: &str) -> String {
    format!("# {:<LABEL_WIDTH$}{value}\n", format!("{label}:"))
}

/// `<alias-or-vault> -> <backend>/<vault>`.
///
/// In the degenerate workspace-of-one there is no alias, so the real vault
/// names both sides rather than leaving the left half blank.
fn target_summary(manifest: &ScheduleManifestV1) -> String {
    let target = &manifest.target;
    let left = target.workspace_alias.as_deref().unwrap_or(&target.vault);
    format!("{left} -> {}/{}", target.backend_name, target.vault)
}

/// The `.xv.toml` that participated, and the environment it selected.
fn project_summary(manifest: &ScheduleManifestV1) -> String {
    match (&manifest.target.project_path, &manifest.target.environment) {
        (Some(path), Some(env)) => format!("{path} (environment {env})"),
        (Some(path), None) => path.clone(),
        // Said explicitly: "no project file participated" is a fact about the
        // pinned target, not a rendering gap.
        (None, _) => "(none)".to_string(),
    }
}

/// The complete `--print` text, ending with a newline. Pure: renders only,
/// touches neither the filesystem nor the scheduler.
pub fn render_install_preview(
    platform: Platform,
    schedule: &RotationSchedule,
    paths: &UnitPaths,
    manifest: &ScheduleManifestV1,
) -> String {
    let mut out = String::new();

    out.push_str(&header("scheduler", platform.name()));
    out.push_str(&header("schedule", &schedule.interval.describe()));
    out.push_str(&header("target", &target_summary(manifest)));
    out.push_str(&header(
        "backend",
        &format!(
            "{} ({}, {})",
            manifest.target.backend_name,
            manifest.target.backend_kind,
            manifest.target.backend_identity
        ),
    ));
    out.push_str(&header("config", &manifest.target.config_path));
    out.push_str(&header("project", &project_summary(manifest)));
    out.push_str(&header("cwd", &manifest.execution.working_directory));
    out.push_str(&header("command", &schedule.command_line()));
    out.push_str(&header("log", &schedule.log_path.display().to_string()));

    out.push_str("\n# --- manifest.json (preview; installed_at is assigned during install) ---\n");
    out.push_str(&serialize_manifest_preview(manifest));
    out.push('\n');

    for unit in schedule::render(platform, schedule, paths) {
        out.push_str(&format!("\n# --- {} ---\n", unit.path.display()));
        out.push_str(&unit.contents);
    }

    if platform == Platform::Schtasks {
        out.push_str(&format!(
            "\n# --- schtasks invocation ---\nschtasks {}\n",
            // The arguments are passed to `schtasks` directly, not through a
            // shell; the `/TR` value already carries the per-argument quoting
            // from `command_line()`, so joining here is a faithful display.
            schedule::schtasks_create_args(schedule).join(" ")
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::schedule::manifest::{ManifestCadence, ManifestExecution, ManifestTarget};
    use crate::schedule::{ScheduleCommand, ScheduleInterval};

    const CONFIG_DIGEST: &str =
        "sha256:9c1e9c85ec2f2701ac6f8feccdc5b9d12f9ba72f15cc32eb590cd724e34b8e92";
    const PROJECT_DIGEST: &str =
        "sha256:83ad20d5db8cc85526a362d78c7436bb5d144733eb3a16b44132620313098c4f";
    const BACKEND_IDENTITY: &str =
        "sha256:55db8b6f5e64ef7c0af4c5b1f9b4d40d45994e8dcfb7d1f7b995f55f7cad2213";
    const MANIFEST_PATH: &str =
        "/home/alice/.local/state/xv/schedules/rotation-default/manifest.json";
    const LOG_PATH: &str = "/home/alice/.local/state/xv/rotate.log";
    const WORKING_DIRECTORY: &str = "/home/alice/work/service";
    const UNIT_DIR: &str = "/home/alice/.config/systemd/user";

    /// The golden's fixture target: an aliased AWS vault selected by a project
    /// environment.
    fn manifest() -> ScheduleManifestV1 {
        ScheduleManifestV1 {
            schema_version: 1,
            schedule_id: "rotation-default".to_string(),
            // Replaced by the placeholder in the preview; a real value here
            // proves the substitution actually happens.
            installed_at: "2026-09-10T03:00:00Z".to_string(),
            cadence: ManifestCadence {
                kind: "daily".to_string(),
                hour: 3,
                minute: 0,
            },
            execution: ManifestExecution {
                binary_path: "/home/alice/bin/xv".to_string(),
                installed_version: "0.39.0".to_string(),
                working_directory: "/home/alice/work/service".to_string(),
                log_path: LOG_PATH.to_string(),
            },
            target: ManifestTarget {
                config_path: "/home/alice/.config/xv/xv.conf".to_string(),
                config_digest: CONFIG_DIGEST.to_string(),
                project_path: Some("/home/alice/work/service/.xv.toml".to_string()),
                project_digest: Some(PROJECT_DIGEST.to_string()),
                environment: Some("production".to_string()),
                context_path: None,
                context_digest: None,
                workspace_source: "project".to_string(),
                workspace_alias: Some("payments".to_string()),
                backend_name: "aws-prod".to_string(),
                backend_kind: "aws".to_string(),
                backend_identity: BACKEND_IDENTITY.to_string(),
                vault: "payments-production".to_string(),
            },
        }
    }

    fn schedule() -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily { hour: 3, minute: 0 },
            command: ScheduleCommand::ManifestRun {
                manifest: PathBuf::from(MANIFEST_PATH),
                working_directory: PathBuf::from(WORKING_DIRECTORY),
            },
            binary: PathBuf::from("/home/alice/bin/xv"),
            log_path: PathBuf::from(LOG_PATH),
            home: PathBuf::from("/home/alice"),
            state_home: None,
        }
    }

    fn unit_paths() -> UnitPaths {
        UnitPaths {
            dir: PathBuf::from(UNIT_DIR),
        }
    }

    /// Unix-only, and deliberately so: this is a byte-for-byte golden of a
    /// systemd user unit, whose fixture paths are Unix paths and whose
    /// `# --- <path> ---` separators are rendered by `Path::join`, which uses
    /// a backslash on Windows. Re-deriving the expectation from `join` would
    /// make the test agree with whatever the renderer does; instead the
    /// Windows side gets its own structural test below.
    #[cfg(unix)]
    #[test]
    fn systemd_preview_matches_the_golden() {
        let manifest = manifest();
        let schedule = schedule();
        let paths = unit_paths();
        let rendered = render_install_preview(Platform::Systemd, &schedule, &paths, &manifest);

        // The header and manifest block are the contract; they are written out
        // literally here from the golden's fixture values.
        let expected_prefix = format!(
            "\
# scheduler: systemd user timer
# schedule:  daily at 03:00
# target:    payments -> aws-prod/payments-production
# backend:   aws-prod (aws, {BACKEND_IDENTITY})
# config:    /home/alice/.config/xv/xv.conf
# project:   /home/alice/work/service/.xv.toml (environment production)
# cwd:       /home/alice/work/service
# command:   /home/alice/bin/xv schedule run --manifest {MANIFEST_PATH}
# log:       {LOG_PATH}

# --- manifest.json (preview; installed_at is assigned during install) ---
{{
  \"schema_version\": 1,
  \"schedule_id\": \"rotation-default\",
  \"installed_at\": \"<set-at-install>\",
  \"cadence\": {{
    \"kind\": \"daily\",
    \"hour\": 3,
    \"minute\": 0
  }},
  \"execution\": {{
    \"binary_path\": \"/home/alice/bin/xv\",
    \"installed_version\": \"0.39.0\",
    \"working_directory\": \"/home/alice/work/service\",
    \"log_path\": \"{LOG_PATH}\"
  }},
  \"target\": {{
    \"config_path\": \"/home/alice/.config/xv/xv.conf\",
    \"config_digest\": \"{CONFIG_DIGEST}\",
    \"project_path\": \"/home/alice/work/service/.xv.toml\",
    \"project_digest\": \"{PROJECT_DIGEST}\",
    \"environment\": \"production\",
    \"context_path\": null,
    \"context_digest\": null,
    \"workspace_source\": \"project\",
    \"workspace_alias\": \"payments\",
    \"backend_name\": \"aws-prod\",
    \"backend_kind\": \"aws\",
    \"backend_identity\": \"{BACKEND_IDENTITY}\",
    \"vault\": \"payments-production\"
  }}
}}
"
        );

        // The unit bodies are written out literally, NOT re-derived from
        // `schedule::render` — a test that renders its own expectation cannot
        // catch the unit drifting away from the golden.
        //
        // Deviations from goldens 67-88 below, all of them the "required
        // metadata" the golden's preamble allows unit bodies to gain:
        //   - the managed-by comment and `Documentation=`, so a reader who
        //     finds the file knows what owns it;
        //   - crosstache's own `Description=` wording;
        //   - per-argument quoting on `ExecStart=` and `Environment=`, without
        //     which a manifest path or HOME containing a space silently
        //     becomes two arguments (asserted separately below);
        //   - `RandomizedDelaySec=` on the timer.
        // Everything the golden makes a contract — the ExecStart command, the
        // WorkingDirectory, the single HOME environment pair, the append
        // redirections, the calendar and the install target — is literal here.
        let expected_service = format!(
            "\
# Managed by crosstache (xv schedule). Edits are overwritten on reinstall.
[Unit]
Description=crosstache due-secret rotation sweep
Documentation=https://github.com/bziobnic/crosstache/blob/main/docs/rotation.md

[Service]
Type=oneshot
ExecStart=\"/home/alice/bin/xv\" \"schedule\" \"run\" \"--manifest\" \"{MANIFEST_PATH}\"
WorkingDirectory={WORKING_DIRECTORY}
Environment=\"HOME=/home/alice\"
StandardOutput=append:{LOG_PATH}
StandardError=append:{LOG_PATH}
"
        );
        let expected_timer = "\
# Managed by crosstache (xv schedule). Edits are overwritten on reinstall.
[Unit]
Description=crosstache due-secret rotation schedule

[Timer]
OnCalendar=*-*-* 03:00:00
# Run a missed sweep once the machine is back, rather than skipping a day.
Persistent=true
# Spread load and avoid every host rotating at the same instant.
RandomizedDelaySec=300

[Install]
WantedBy=timers.target
";

        let expected = format!(
            "{expected_prefix}\n# --- {UNIT_DIR}/xv-rotate.service ---\n{expected_service}\
             \n# --- {UNIT_DIR}/xv-rotate.timer ---\n{expected_timer}"
        );

        assert_eq!(rendered, expected);

        // The golden's contract lines, asserted as text so a renderer change
        // that quietly drops one is a failure with a name.
        for required in [
            format!("ExecStart=\"/home/alice/bin/xv\" \"schedule\" \"run\" \"--manifest\" \"{MANIFEST_PATH}\"\n"),
            format!("WorkingDirectory={WORKING_DIRECTORY}\n"),
            "Environment=\"HOME=/home/alice\"\n".to_string(),
            format!("StandardOutput=append:{LOG_PATH}\n"),
            format!("StandardError=append:{LOG_PATH}\n"),
            "OnCalendar=*-*-* 03:00:00\n".to_string(),
            "Persistent=true\n".to_string(),
            "WantedBy=timers.target\n".to_string(),
        ] {
            assert!(rendered.contains(&required), "missing {required:?}\n{rendered}");
        }

        // goldens:73 — WorkingDirectory sits after ExecStart and before the
        // environment, exactly as the golden orders them.
        let exec = rendered.find("ExecStart=").unwrap();
        let workdir = rendered.find("WorkingDirectory=").unwrap();
        let env = rendered.find("Environment=").unwrap();
        assert!(exec < workdir && workdir < env, "{rendered}");

        // goldens:91-92 — no target-selection environment variables, and no
        // ambient sweep in place of the manifest runner.
        assert!(!rendered.contains("XDG_CONFIG_HOME"), "{rendered}");
        assert!(!rendered.contains("--vault"), "{rendered}");
        assert!(!rendered.contains("rotate --due"), "{rendered}");

        // Order is service, then timer — a reader enables the timer last.
        assert!(
            rendered.find("xv-rotate.service ---") < rendered.find("xv-rotate.timer ---"),
            "{rendered}"
        );
    }

    /// The Windows counterpart to the golden: the header block and the
    /// manifest preview are asserted with *Windows* fixture paths, so a
    /// renderer that mangles a drive-letter path (or drops a header line)
    /// fails on the platform where the golden cannot run. Structural
    /// assertions only — no pinned unit body, since the `schtasks` path is
    /// covered by `schtasks_preview_shows_the_creation_invocation`.
    #[cfg(windows)]
    #[test]
    fn schtasks_preview_carries_windows_paths_through_the_header_and_manifest() {
        let mut m = manifest();
        m.execution.binary_path = r"C:\Program Files\xv\xv.exe".to_string();
        m.execution.working_directory = r"C:\Users\alice\work\service".to_string();
        m.execution.log_path = r"C:\Users\alice\AppData\Local\xv\rotate.log".to_string();
        m.target.config_path = r"C:\Users\alice\AppData\Roaming\xv\xv.conf".to_string();
        m.target.project_path = Some(r"C:\Users\alice\work\service\.xv.toml".to_string());

        let mut s = schedule();
        s.binary = PathBuf::from(r"C:\Program Files\xv\xv.exe");
        s.log_path = PathBuf::from(&m.execution.log_path);
        s.command = ScheduleCommand::ManifestRun {
            manifest: PathBuf::from(
                r"C:\Users\alice\AppData\Local\xv\schedules\rotation-default\manifest.json",
            ),
            working_directory: PathBuf::from(&m.execution.working_directory),
        };

        let out = render_install_preview(Platform::Schtasks, &s, &unit_paths(), &m);

        for required in [
            "# scheduler: Task Scheduler\n".to_string(),
            "# schedule:  daily at 03:00\n".to_string(),
            "# target:    payments -> aws-prod/payments-production\n".to_string(),
            format!("# config:    {}\n", m.target.config_path),
            format!(
                "# project:   {} (environment production)\n",
                m.target.project_path.as_deref().unwrap()
            ),
            format!("# cwd:       {}\n", m.execution.working_directory),
            format!("# log:       {}\n", m.execution.log_path),
            format!(
                "\"binary_path\": \"{}\"",
                m.execution.binary_path.escape_default()
            ),
        ] {
            assert!(out.contains(&required), "missing {required:?}\n{out}");
        }

        // The manifest preview still masks the stamp and the run is still the
        // pinned runner, not an ambient sweep.
        assert!(
            out.contains("\"installed_at\": \"<set-at-install>\""),
            "{out}"
        );
        assert!(out.contains("schedule run --manifest"), "{out}");
        assert!(!out.contains("rotate --due"), "{out}");
        assert!(!out.contains("XDG_CONFIG_HOME"), "{out}");
    }

    #[test]
    fn rendering_is_deterministic() {
        let (m, s, p) = (manifest(), schedule(), unit_paths());
        assert_eq!(
            render_install_preview(Platform::Systemd, &s, &p, &m),
            render_install_preview(Platform::Systemd, &s, &p, &m)
        );
    }

    #[test]
    fn a_degenerate_target_names_the_real_vault_on_both_sides() {
        let mut m = manifest();
        m.target.workspace_alias = None;
        m.target.workspace_source = "degenerate".to_string();
        let out = render_install_preview(Platform::Systemd, &schedule(), &unit_paths(), &m);
        assert!(
            out.contains("# target:    payments-production -> aws-prod/payments-production"),
            "{out}"
        );
    }

    #[test]
    fn a_target_with_no_project_says_so() {
        let mut m = manifest();
        m.target.project_path = None;
        m.target.project_digest = None;
        m.target.environment = None;
        let out = render_install_preview(Platform::Systemd, &schedule(), &unit_paths(), &m);
        assert!(out.contains("# project:   (none)\n"), "{out}");
    }

    #[test]
    fn a_project_without_a_selected_environment_omits_the_parenthetical() {
        let mut m = manifest();
        m.target.environment = None;
        let out = render_install_preview(Platform::Systemd, &schedule(), &unit_paths(), &m);
        assert!(
            out.contains("# project:   /home/alice/work/service/.xv.toml\n"),
            "{out}"
        );
    }

    #[test]
    fn launchd_preview_escapes_a_manifest_path_for_xml() {
        let mut s = schedule();
        s.command = ScheduleCommand::ManifestRun {
            manifest: PathBuf::from("/home/a & b/manifest.json"),
            working_directory: PathBuf::from(WORKING_DIRECTORY),
        };
        let out = render_install_preview(Platform::Launchd, &s, &unit_paths(), &manifest());
        assert!(
            out.contains("<string>/home/a &amp; b/manifest.json</string>"),
            "{out}"
        );
    }

    #[test]
    fn schtasks_preview_shows_the_creation_invocation() {
        let mut s = schedule();
        s.command = ScheduleCommand::ManifestRun {
            manifest: PathBuf::from("/home/a b/manifest.json"),
            working_directory: PathBuf::from("/home/a b/work"),
        };
        let out = render_install_preview(Platform::Schtasks, &s, &unit_paths(), &manifest());
        assert!(
            out.contains("\n# --- schtasks invocation ---\nschtasks /Create"),
            "{out}"
        );
        // A manifest path with a space must stay one argument, and so must the
        // working directory the task starts in.
        assert!(out.contains("\"/home/a b/manifest.json\""), "{out}");
        assert!(out.contains("cd /d \"/home/a b/work\" && "), "{out}");
    }
}
