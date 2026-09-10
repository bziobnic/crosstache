//! Unit coverage for [`super::validate_recorded_target`].
//!
//! One test per row of the design's drift table
//! (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
//! "Drift policy"), plus the two cross-cutting properties: reasons come out in
//! manifest-field order, and they never quote file contents or provider error
//! bodies.
//!
//! Everything here works on real files in a tempdir and never constructs a
//! backend — validation must reach its verdict before one exists.

use super::*;
use crate::schedule::manifest::{ManifestCadence, ManifestExecution};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

fn config_contents(store: &Path, key: &Path) -> String {
    format!(
        r#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = "{store}"
key_file = "{key}"
default_vault = "default"
"#,
        store = toml_path(store),
        key = toml_path(key),
    )
}

/// A recorded target and the tree it was recorded from.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config_path: PathBuf,
    manifest: ScheduleManifestV1,
}

impl Fixture {
    /// The running test binary, shaped the way `recorded_binary_path` shapes
    /// the installed one.
    fn binary() -> PathBuf {
        let exe = std::env::current_exe().expect("the test binary has a path");
        crate::utils::helpers::lexically_normalize_from(Path::new(""), &exe)
    }

    async fn new() -> Self {
        Self::with_context(None).await
    }

    /// Build a fixture whose `xv.conf` selects the local backend at a store
    /// path inside the tempdir. `context`, when given, is written to
    /// `<root>/.xv/context` and recorded in the manifest.
    async fn with_context(context: Option<&str>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = crate::utils::helpers::canonicalize_without_verbatim_prefix(tmp.path())
            .expect("canonical tempdir");
        let config_path = root.join(".config").join("xv").join("xv.conf");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            config_contents(&root.join("store"), &root.join("key.txt")),
        )
        .unwrap();

        let (context_path, context_digest) = match context {
            None => (None, None),
            Some(body) => {
                let dir = root.join(".xv");
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("context");
                std::fs::write(&path, body).unwrap();
                (
                    Some(path.to_string_lossy().into_owned()),
                    Some(crate::config::content_digest(body.as_bytes())),
                )
            }
        };

        let (config, bytes) = crate::config::settings::load_config_file_at_with_bytes(&config_path)
            .await
            .expect("fixture config loads");
        let identity = crate::schedule::target::selected_backend_identity(&config, "local")
            .expect("fixture identity");

        let manifest = ScheduleManifestV1 {
            schema_version: 1,
            schedule_id: crate::schedule::manifest::SCHEDULE_ID.to_string(),
            installed_at: "2026-09-09T15:04:05Z".to_string(),
            cadence: ManifestCadence {
                kind: "daily".to_string(),
                hour: 3,
                minute: 0,
            },
            execution: ManifestExecution {
                binary_path: Self::binary().to_string_lossy().into_owned(),
                installed_version: env!("CARGO_PKG_VERSION").to_string(),
                working_directory: root.to_string_lossy().into_owned(),
                log_path: root.join("rotate.log").to_string_lossy().into_owned(),
            },
            target: ManifestTarget {
                config_path: config_path.to_string_lossy().into_owned(),
                config_digest: crate::config::content_digest(&bytes),
                project_path: None,
                project_digest: None,
                environment: None,
                context_path,
                context_digest,
                workspace_source: if context.is_some() {
                    "context".to_string()
                } else {
                    "degenerate".to_string()
                },
                workspace_alias: None,
                backend_name: identity.name,
                backend_kind: identity.kind,
                backend_identity: identity.digest,
                vault: "default".to_string(),
            },
        };

        Self {
            _tmp: tmp,
            root,
            config_path,
            manifest,
        }
    }

    /// Add a `.xv.toml` next to the recorded working directory and record it,
    /// with `production` as the active environment.
    fn with_project(mut self, body: &str, environment: Option<&str>) -> Self {
        let path = self.root.join(".xv.toml");
        std::fs::write(&path, body).unwrap();
        self.manifest.target.project_path = Some(path.to_string_lossy().into_owned());
        self.manifest.target.project_digest = Some(crate::config::content_digest(body.as_bytes()));
        self.manifest.target.environment = environment.map(str::to_string);
        self
    }

    /// Move the recorded working directory into a fresh subdirectory, leaving
    /// the recorded project file where it is — the ancestor-project shape.
    fn in_subdirectory(mut self, name: &str) -> Self {
        let child = self.root.join(name);
        std::fs::create_dir_all(&child).unwrap();
        self.manifest.execution.working_directory = child.to_string_lossy().into_owned();
        self
    }

    async fn validate(&self) -> DriftReport {
        validate_recorded_target(&self.manifest, &Self::binary(), env!("CARGO_PKG_VERSION")).await
    }
}

/// A context file attaching two aliases on the active local backend.
const WORKSPACE_CONTEXT: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "payments-production", "backend": "local", "alias": "payments", "default": true },
      { "vault": "billing-production", "backend": "local", "alias": "billing" }
    ]
  }
}
"#;

/// The same context with `payments` detached.
const WORKSPACE_CONTEXT_WITHOUT_PAYMENTS: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "billing-production", "backend": "local", "alias": "billing", "default": true }
    ]
  }
}
"#;

/// `payments` remapped to a different real vault.
const WORKSPACE_CONTEXT_REMAPPED: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "payments-staging", "backend": "local", "alias": "payments", "default": true },
      { "vault": "billing-production", "backend": "local", "alias": "billing" }
    ]
  }
}
"#;

/// A workspace whose default alias lives on a *named* backend.
const WORKSPACE_CONTEXT_NAMED_BACKEND: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "stage-vault", "backend": "local-b", "alias": "stage", "default": true }
    ]
  }
}
"#;

const PROJECT_WITH_ENV: &str = r#"
[env.production]
vault = "project-vault"
"#;

// ---------------------------------------------------------------------------
// The undrifted baseline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unchanged_target_is_valid() {
    let fixture = Fixture::new().await;
    let report = fixture.validate().await;
    assert_eq!(report.verdict, DriftVerdict::Valid, "{:?}", report.reasons);
    assert!(report.reasons.is_empty());
    assert!(report.warnings.is_empty());
}

#[tokio::test]
async fn an_unchanged_project_target_is_valid() {
    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"));
    let report = fixture.validate().await;
    assert_eq!(report.verdict, DriftVerdict::Valid, "{:?}", report.reasons);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_config_refuses() {
    let fixture = Fixture::new().await;
    std::fs::remove_file(&fixture.config_path).unwrap();
    let report = fixture.validate().await;
    assert!(report.is_refused());
    assert_eq!(report.fields(), vec!["config_path"]);
}

#[tokio::test]
async fn changed_config_bytes_refuse() {
    let fixture = Fixture::new().await;
    let body = std::fs::read_to_string(&fixture.config_path).unwrap();
    std::fs::write(&fixture.config_path, format!("{body}\n# a comment\n")).unwrap();

    let report = fixture.validate().await;
    assert!(report.is_refused());
    assert_eq!(report.fields(), vec!["config_digest"]);
    assert_eq!(
        report.reasons[0].detail,
        format!(
            "config_digest changed; review {} and reinstall",
            fixture.manifest.target.config_path
        )
    );
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_project_file_refuses() {
    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"));
    std::fs::remove_file(fixture.root.join(".xv.toml")).unwrap();

    let report = fixture.validate().await;
    assert!(report.is_refused());
    assert_eq!(report.fields(), vec!["project_path"]);
}

#[tokio::test]
async fn changed_project_bytes_refuse() {
    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"));
    std::fs::write(
        fixture.root.join(".xv.toml"),
        format!("{PROJECT_WITH_ENV}\n# edited\n"),
    )
    .unwrap();

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["project_digest"]);
}

#[tokio::test]
async fn a_removed_environment_refuses() {
    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"));
    let replacement = "\n[env.staging]\nvault = \"other\"\n";
    std::fs::write(fixture.root.join(".xv.toml"), replacement).unwrap();

    let report = fixture.validate().await;
    // The bytes changed too — both reasons are reported, in field order.
    assert_eq!(report.fields(), vec!["project_digest", "environment"]);
    assert!(
        report.reasons[1].detail.contains("'production'"),
        "{:?}",
        report.reasons[1]
    );
}

#[tokio::test]
async fn a_project_file_appearing_after_an_absent_install_refuses() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.root.join(".xv.toml"), PROJECT_WITH_ENV).unwrap();

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["project_path"]);
}

/// Project discovery always walks up, whatever `XV_NO_PARENT_CONFIG` says.
///
/// The variable is an ambient discovery switch the scheduler's environment
/// never carries. If the replay honored it, a schedule installed under it
/// would refuse every night; if the replay refused to walk up, an ancestor
/// project file recorded at install time would read as `project_path` drift.
/// Installation refuses the one case where the two can disagree
/// (`target::suppressed_parent_project`), so here the recorded ancestor file
/// simply has to still resolve.
#[tokio::test]
async fn an_ancestor_project_file_is_still_discovered_from_a_subdirectory() {
    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"))
        .in_subdirectory("service");

    let report = fixture.validate().await;
    assert_eq!(report.verdict, DriftVerdict::Valid, "{:?}", report.reasons);
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_participating_context_refuses() {
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };
    std::fs::remove_file(fixture.root.join(".xv").join("context")).unwrap();

    let report = fixture.validate().await;
    assert!(report.is_refused());
    assert!(
        report.fields().contains(&"context_path"),
        "{:?}",
        report.fields()
    );
}

#[tokio::test]
async fn a_changed_participating_context_refuses() {
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };
    std::fs::write(
        fixture.root.join(".xv").join("context"),
        format!("{WORKSPACE_CONTEXT}\n"),
    )
    .unwrap();

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["context_digest"]);
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_detached_alias_refuses() {
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };
    std::fs::write(
        fixture.root.join(".xv").join("context"),
        WORKSPACE_CONTEXT_WITHOUT_PAYMENTS,
    )
    .unwrap();

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["context_digest", "workspace_alias"]);
}

#[tokio::test]
async fn a_remapped_alias_refuses_on_the_vault() {
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };
    std::fs::write(
        fixture.root.join(".xv").join("context"),
        WORKSPACE_CONTEXT_REMAPPED,
    )
    .unwrap();

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["context_digest", "vault"]);
}

#[tokio::test]
async fn a_changed_workspace_source_refuses() {
    // Recorded as degenerate; a context workspace now supplies the target.
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_source = "degenerate".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert!(
        report.fields().contains(&"workspace_source"),
        "{:?}",
        report.fields()
    );
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_registry_entry_refuses() {
    // The workspace names `local-b`, which the config does not configure.
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT_NAMED_BACKEND)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("stage".to_string());
    manifest.target.vault = "stage-vault".to_string();
    manifest.target.backend_name = "local-b".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["backend_name"]);
    assert!(
        report.reasons[0].detail.contains("local-b"),
        "{:?}",
        report.reasons[0]
    );
}

#[tokio::test]
async fn a_changed_backend_kind_refuses() {
    let fixture = Fixture::new().await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.backend_kind = "azure".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["backend_kind"]);
}

#[tokio::test]
async fn a_changed_backend_identity_refuses() {
    let fixture = Fixture::new().await;
    let moved = fixture.root.join("store-moved");
    std::fs::write(
        &fixture.config_path,
        config_contents(&moved, &fixture.root.join("key.txt")),
    )
    .unwrap();

    let report = fixture.validate().await;
    // The store path lives in the config file, so its digest drifted too.
    assert_eq!(report.fields(), vec!["config_digest", "backend_identity"]);
    assert_eq!(
        report.reasons[1].detail,
        "backend_identity changed for local; review the account/provider and reinstall"
    );
}

// ---------------------------------------------------------------------------
// Working directory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_working_directory_refuses() {
    let fixture = Fixture::new().await;
    let mut manifest = fixture.manifest.clone();
    manifest.execution.working_directory = fixture.root.join("gone").to_string_lossy().into_owned();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["working_directory"]);
    // Nothing downstream was recomputed against a guessed directory.
    assert_eq!(report.reasons.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_working_directory_that_canonicalizes_elsewhere_refuses() {
    let fixture = Fixture::new().await;
    let real = fixture.root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = fixture.root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut manifest = fixture.manifest.clone();
    manifest.execution.working_directory = link.to_string_lossy().into_owned();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["working_directory"]);
}

// ---------------------------------------------------------------------------
// Executable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_different_binary_path_refuses() {
    let fixture = Fixture::new().await;
    let mut manifest = fixture.manifest.clone();
    manifest.execution.binary_path = fixture.root.join("xv").to_string_lossy().into_owned();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.fields(), vec!["binary_path"]);
}

#[tokio::test]
async fn a_missing_binary_refuses() {
    let fixture = Fixture::new().await;
    let gone = fixture.root.join("xv-gone");
    let mut manifest = fixture.manifest.clone();
    manifest.execution.binary_path = gone.to_string_lossy().into_owned();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    // Same *recorded* path as the "running" binary, so the path comparison
    // passes and the file check is what refuses.
    let report =
        validate_recorded_target(&fixture.manifest, &gone, env!("CARGO_PKG_VERSION")).await;
    assert_eq!(report.fields(), vec!["binary_path"]);
    assert!(report.reasons[0].detail.contains("missing"));
}

#[tokio::test]
async fn a_binary_path_that_is_not_a_regular_file_refuses() {
    let fixture = Fixture::new().await;
    let dir = fixture.root.join("xv-dir");
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = fixture.manifest.clone();
    manifest.execution.binary_path = dir.to_string_lossy().into_owned();

    let report = validate_recorded_target(&manifest, &dir, env!("CARGO_PKG_VERSION")).await;
    assert_eq!(report.fields(), vec!["binary_path"]);
    assert!(report.reasons[0].detail.contains("regular file"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_non_executable_binary_refuses() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await;
    let file = fixture.root.join("xv-not-exec");
    std::fs::write(&file, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut manifest = fixture.manifest.clone();
    manifest.execution.binary_path = file.to_string_lossy().into_owned();

    let report = validate_recorded_target(&manifest, &file, env!("CARGO_PKG_VERSION")).await;
    assert_eq!(report.fields(), vec!["binary_path"]);
    assert!(report.reasons[0].detail.contains("not executable"));
}

#[tokio::test]
async fn the_same_binary_with_a_new_version_warns_and_allows_the_run() {
    let fixture = Fixture::new().await;
    let report = validate_recorded_target(&fixture.manifest, &Fixture::binary(), "99.99.99").await;

    assert_eq!(report.verdict, DriftVerdict::Warning);
    assert!(report.reasons.is_empty(), "{:?}", report.reasons);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].field, "installed_version");
    assert!(report.warnings[0].detail.contains("99.99.99"));
}

// ---------------------------------------------------------------------------
// Ambient inputs are ignored
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ambient_environment_selection_is_ignored() {
    // `XV_ENV` names an environment the recorded project file does not even
    // define. The replay uses the recorded name, so the target is unchanged.
    let _guard = crate::config::project::test_support::XvEnvGuard::acquire();
    std::env::set_var("XV_ENV", "no-such-environment");

    let fixture = Fixture::new()
        .await
        .with_project(PROJECT_WITH_ENV, Some("production"));
    let report = fixture.validate().await;
    assert_eq!(report.verdict, DriftVerdict::Valid, "{:?}", report.reasons);
}

#[tokio::test]
async fn the_process_working_directory_and_ambient_context_are_ignored() {
    // The process cwd is the crate root under `cargo test`, and the developer
    // may well have a real `~/.config/xv/xv.conf` and context file. Neither
    // participates: the fixture's recorded inputs are all that is read.
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    let report = fixture.validate().await;
    assert_eq!(report.verdict, DriftVerdict::Valid, "{:?}", report.reasons);
}

// ---------------------------------------------------------------------------
// Cross-cutting properties
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reasons_are_ordered_by_manifest_field() {
    // Drift several layers at once and check the output order, not the set.
    let fixture = Fixture::with_context(Some(WORKSPACE_CONTEXT)).await;
    let mut manifest = fixture.manifest.clone();
    manifest.target.workspace_alias = Some("payments".to_string());
    manifest.target.vault = "payments-production".to_string();
    let fixture = Fixture {
        manifest,
        ..fixture
    };

    // config bytes + context bytes + a project file that was never recorded.
    let body = std::fs::read_to_string(&fixture.config_path).unwrap();
    std::fs::write(&fixture.config_path, format!("{body}\n# edited\n")).unwrap();
    std::fs::write(fixture.root.join(".xv.toml"), PROJECT_WITH_ENV).unwrap();
    std::fs::write(
        fixture.root.join(".xv").join("context"),
        WORKSPACE_CONTEXT_WITHOUT_PAYMENTS,
    )
    .unwrap();

    let report = fixture.validate().await;
    let fields = report.fields();
    assert_eq!(
        fields,
        vec![
            "config_digest",
            "project_path",
            "context_digest",
            "workspace_alias"
        ],
        "{fields:?}"
    );

    let ranks: Vec<usize> = fields.iter().map(|f| field_rank(f)).collect();
    assert!(
        ranks.windows(2).all(|w| w[0] <= w[1]),
        "reasons must be sorted by manifest field: {ranks:?}"
    );
}

#[tokio::test]
async fn reasons_never_quote_file_contents_or_error_bodies() {
    let fixture = Fixture::new().await;
    // A config whose contents contain a distinctive marker, and which no
    // longer parses into the recorded target.
    let marker = "zzz-secret-marker-zzz";
    std::fs::write(
        &fixture.config_path,
        format!(
            "{}\n# {marker}\n",
            config_contents(
                &fixture.root.join("store-moved"),
                &fixture.root.join("key.txt")
            )
        ),
    )
    .unwrap();
    std::fs::write(
        fixture.root.join(".xv.toml"),
        format!("# {marker}\n[env.production]\nvault = \"{marker}\"\n"),
    )
    .unwrap();

    let report = fixture.validate().await;
    assert!(report.is_refused());
    for reason in &report.reasons {
        assert!(
            !reason.detail.contains(marker),
            "a drift reason quoted file contents: {reason:?}"
        );
        assert!(
            !reason.detail.contains("store_path"),
            "a drift reason quoted config keys: {reason:?}"
        );
    }
}
