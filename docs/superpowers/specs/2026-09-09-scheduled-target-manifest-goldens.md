# Scheduled target manifest golden outputs

> **Status:** Normative examples for the
> [scheduled target manifest design](2026-09-09-scheduled-target-manifest-design.md).
> Paths, hashes, timestamps and versions below are fixed fixture values. Tests
> may normalize only those values.

These examples define output structure and wording. Platform unit bodies may
gain required metadata, but their target selection, manifest/result shapes,
field labels and state names are contracts for this delivery block.

## `schedule install --print`

Command:

```text
xv --env production schedule install --vault payments --print
```

Normalized systemd output:

```text
# scheduler: systemd user timer
# schedule:  daily at 03:00
# target:    payments -> aws-prod/payments-production
# backend:   aws-prod (aws, sha256:55db8b6f5e64ef7c0af4c5b1f9b4d40d45994e8dcfb7d1f7b995f55f7cad2213)
# config:    /home/alice/.config/xv/xv.conf
# project:   /home/alice/work/service/.xv.toml (environment production)
# cwd:       /home/alice/work/service
# command:   /home/alice/bin/xv schedule run --manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
# log:       /home/alice/.local/state/xv/rotate.log

# --- manifest.json (preview; installed_at is assigned during install) ---
{
  "schema_version": 1,
  "schedule_id": "rotation-default",
  "installed_at": "<set-at-install>",
  "cadence": {
    "kind": "daily",
    "hour": 3,
    "minute": 0
  },
  "execution": {
    "binary_path": "/home/alice/bin/xv",
    "installed_version": "0.39.0",
    "working_directory": "/home/alice/work/service",
    "log_path": "/home/alice/.local/state/xv/rotate.log"
  },
  "target": {
    "config_path": "/home/alice/.config/xv/xv.conf",
    "config_digest": "sha256:9c1e9c85ec2f2701ac6f8feccdc5b9d12f9ba72f15cc32eb590cd724e34b8e92",
    "project_path": "/home/alice/work/service/.xv.toml",
    "project_digest": "sha256:83ad20d5db8cc85526a362d78c7436bb5d144733eb3a16b44132620313098c4f",
    "environment": "production",
    "context_path": null,
    "context_digest": null,
    "workspace_source": "project",
    "workspace_alias": "payments",
    "backend_name": "aws-prod",
    "backend_kind": "aws",
    "backend_identity": "sha256:55db8b6f5e64ef7c0af4c5b1f9b4d40d45994e8dcfb7d1f7b995f55f7cad2213",
    "vault": "payments-production",
    "vault_selection": "explicit"
  }
}

# --- /home/alice/.config/systemd/user/xv-rotate.service ---
[Unit]
Description=xv due-secret rotation

[Service]
Type=oneshot
ExecStart=/home/alice/bin/xv schedule run --manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
WorkingDirectory=/home/alice/work/service
Environment=HOME=/home/alice
StandardOutput=append:/home/alice/.local/state/xv/rotate.log
StandardError=append:/home/alice/.local/state/xv/rotate.log

# --- /home/alice/.config/systemd/user/xv-rotate.timer ---
[Unit]
Description=Run xv due-secret rotation daily

[Timer]
OnCalendar=*-*-* 03:00:00
Persistent=true
Unit=xv-rotate.service

[Install]
WantedBy=timers.target
```

The unit may not add target-selection environment variables or replace the
manifest runner with direct `rotate --due` arguments.

## Healthy status before the first run

```text
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  never
  Next run:  2026-09-10T03:00:00Z
  Log:       /home/alice/.local/state/xv/rotate.log (not yet written)
```

## Healthy status after a successful run

```text
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, 2 rotated, 0 failed
  Next run:  unknown (scheduler did not report a next run)
  Log:       /home/alice/.local/state/xv/rotate.log (present)
```

## Drift refusal

Status lists every difference in manifest-field order:

```text
[error] The installed systemd user timer rotation schedule is unsafe to run.
  Ownership: managed
  Target:    payments -> aws-prod/payments-production
  Drift:     refused
  - project_digest changed; review /home/alice/work/service/.xv.toml and reinstall
  - backend_identity changed for aws-prod; review the account/provider and reinstall
  Last run:  refused_drift; 2026-09-10T03:00:00Z; exit 3; target_drift
  Next run:  2026-09-11T03:00:00Z
[hint] Review the changes, then run 'xv schedule install --vault payments' to accept the new target.
```

The scheduled invocation writes this redacted result and exits with the normal
configuration-error code without constructing the backend:

```json
{
  "schema_version": 1,
  "schedule_id": "rotation-default",
  "manifest_digest": "sha256:4d38e06cbb685364a6500f808d8793840d5eb219d4e6b72f06d0f501c8eb3658",
  "started_at": "2026-09-10T03:00:00Z",
  "finished_at": "2026-09-10T03:00:00Z",
  "state": "refused_drift",
  "exit_code": 3,
  "summary": null,
  "diagnostic": {
    "code": "target_drift",
    "message": "project_digest and backend_identity changed; review the recorded target and reinstall"
  }
}
```

## Legacy and orphaned states

Recognized direct-rotate unit without a manifest:

```text
[warn] A legacy systemd user timer rotation schedule is installed.
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv rotate --due --force --vault payments-production
  Target:    unverified (the legacy unit does not record backend or account identity)
[hint] Replace it explicitly with 'xv schedule install --vault <alias-or-vault>'.
```

Manifest without a native unit:

```text
[warn] A rotation manifest exists but no systemd user timer is installed.
  Ownership: orphaned-manifest
  Target:    payments -> aws-prod/payments-production
  Drift:     valid
[hint] Run 'xv schedule install --vault payments' to repair the schedule, or 'xv schedule uninstall' to remove the manifest.
```

## Retained, running and interrupted outcomes

After reinstall, an outcome bound to the old manifest is labeled:

```text
  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, 2 rotated, 0 failed (previous install)
```

The next completed run replaces it. An active lock renders:

```text
  Last run:  running since 2026-09-10T03:00:00Z
```

A `running` record with no held lock renders:

```text
  Last run:  interrupted after 2026-09-10T03:00:00Z (no runner holds the lock)
```

A second process that cannot acquire the lock appends one line to the log,
exits zero and leaves `last-run.json` unchanged:

```text
schedule rotation skipped: another run is already_running
```

## Redaction canaries

Tests seed these strings into backend failures and nearby config fields, then
assert none appears in the manifest, outcome, status or recovery artifacts:

```text
AKIAIOSFODNN7EXAMPLE
aws-session-token-canary
azure-client-secret-canary
AGE-SECRET-KEY-1CANARY
super-secret-value-canary
```
