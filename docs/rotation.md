# `xv rotate` — rotation and rotation policies

Two distinct things share the word "rotation":

1. **Replacing a value** — `xv rotate NAME` generates a new value and writes it
   as a new version. Works on every backend.
2. **Knowing *when* to replace it** — a rotation *policy* stored with the secret,
   a command that acts on everything due, and `xv schedule` to run that command
   on a cadence via the OS scheduler. All on every backend.

Plus one backend-specific mechanism: `xv rotate --native` hands the whole job to
AWS Secrets Manager's rotation Lambda.

- [Replacing a value](#replacing-a-value)
- [Rotation policies](#rotation-policies)
- [Acting on due secrets](#acting-on-due-secrets)
- [`xv schedule` — run the sweep automatically](#xv-schedule--run-the-sweep-automatically)
- [Where the schedule actually lives](#where-the-schedule-actually-lives)
- [Native rotation (AWS)](#native-rotation-aws)
- [What rotation does not do](#what-rotation-does-not-do)

---

## Replacing a value

```bash
xv rotate API_KEY                            # new 32-char alphanumeric value
xv rotate API_KEY --length 64
xv rotate API_KEY --charset hex              # hex / base64 / numeric / alphanumeric-symbols / …
xv rotate API_KEY --generator ./mygen.sh     # custom generator (must be owned by you, mode 0700)
xv rotate API_KEY --show-value               # echo the new value (otherwise silent)
xv rotate API_KEY --force                    # skip the confirmation
```

Metadata is preserved: tags, groups, note, folder, expiry, content type. On a
typed record, the generated value becomes the new **primary field** rather than
replacing the envelope.

Every rotation stamps `xv:rotated_at`, so if the secret has a policy its clock
restarts from the rotation that actually happened.

---

## Rotation policies

A policy is two tags on the secret:

| Tag | Meaning |
|-----|---------|
| `xv:rotate_every` | Interval, e.g. `90d`. Its presence is what makes a secret policy-managed. |
| `xv:rotated_at` | RFC 3339 timestamp of the last rotation. |

Both are ordinary metadata: they travel with the secret through `xv migrate`,
are visible to anything else reading its tags, and occupy two of the backend's
tag slots (Azure allows 15 per secret).

### Setting a policy

```bash
# Policy only — does NOT change the value. The clock starts now.
xv update DB_PASSWORD --rotate-every 90d

# Rotate now and set (or change) the policy in one step.
xv rotate DB_PASSWORD --every 90d

# Remove the policy.
xv update DB_PASSWORD --clear-rotate-every
```

Interval syntax is `<number><unit>` with unit `m` (minutes), `h` (hours), `d`
(days), or `w` (weeks). The unit is **required** — a bare `90` is rejected rather
than guessed, because guessing minutes vs. days is the difference between
rotating constantly and never rotating. Maximum is 10 years.

`--rotate-every` and `--clear-rotate-every` are standalone operations, like
`--field`: combine them with other edits by running two commands.

---

## Acting on due secrets

### `--check` — report, change nothing

```bash
xv rotate --check
```

```
 Name          Status  Interval  Due
 API_KEY       ok                in 74 days (2026-10-07)
 DB_PASSWORD   due               12 days ago (2026-07-13)
 LEGACY_TOKEN  invalid  ninety   unparseable xv:rotate_every
```

Exits **51** (`xv-rotation-due`) when at least one secret is due, so a pipeline
can gate on staleness. Secrets with no policy are omitted entirely — unmanaged is
not the same as overdue. `--format json|yaml|csv` works as on any list command.

### `--due` — rotate everything that is due

```bash
xv rotate --due            # confirms once for the batch
xv rotate --due --force    # unattended (cron, CI)
```

Only policy-managed, currently-due secrets are touched. Each goes through the
same path as a manual `xv rotate`, so record handling, reserved-key guards, and
the audit/git hooks all apply identically. Generation flags (`--length`,
`--charset`, `--generator`) apply to the whole batch.

`--due` **fails** rather than proceeding if any secret's `xv:rotate_every` cannot
be parsed. An unreadable policy means its due-ness is unknown, and a run that
quietly skipped it would report success while leaving a possibly-overdue secret
in place. Fix or clear the tag, then re-run.

If some rotations fail, every failure is listed and the command exits non-zero
with a count — a partial batch never looks like success.

### `xv schedule` — run the sweep automatically

`xv` installs and manages the trigger itself, in the platform's own scheduler:

```bash
xv schedule install --vault myproj-prod-kv            # daily at 03:00
xv schedule install --vault v --interval hourly --at 00:15
xv schedule install --vault v --interval weekly --at 04:00   # Sundays
xv schedule status
xv schedule uninstall
```

| Platform | Mechanism | Unit |
|----------|-----------|------|
| macOS | launchd user agent | `~/Library/LaunchAgents/com.crosstache.xv-rotate.plist` |
| Linux | systemd **user** timer | `~/.config/systemd/user/xv-rotate.{service,timer}` |
| Windows | Task Scheduler | task `crosstache-xv-rotate` |

All three are per-user, never system-wide: the sweep runs as the user whose
credentials and config it needs, and uninstalling never requires root. There is
**no daemon** — a resident process would have to reimplement, worse, what these
schedulers already do, and would hold decryption credentials for its whole
lifetime.

#### `--print` — review the whole thing before committing to it

```bash
xv schedule install --vault v --print     # read-only preview, writes nothing
```

`--print` resolves the target and verifies the backend read-only, then prints
exactly what an install would produce:

- a header block — scheduler, cadence, the resolved target
  (`<alias-or-vault> -> <backend>/<vault>`), the backend and its identity
  digest, the config file, the `.xv.toml` and environment that participated,
  the working directory, the command, and the log path;
- the `manifest.json` that would be written, verbatim, with `installed_at`
  shown as `<set-at-install>` because the real timestamp is stamped by the
  write itself;
- the native unit file(s), and on Windows the exact `schtasks` invocation.

The pinned unit carries the manifest path, `HOME`, the working directory the
target was resolved in, and the log path — nothing else. In particular it does
**not** set `XDG_CONFIG_HOME`: the manifest already names the exact
configuration file and its digest, and an environment variable that redirects
config resolution is a target-selection input a pinned unit may not add.

It is a preview in the strict sense: it creates no directory, manifest, lock,
result, unit or log, touches no existing file, and calls no scheduler.

The `# command:` line is exactly what the installed unit runs. To drive a
scheduler `xv` does not manage — cron, a Kubernetes CronJob, a CI schedule —
paste that line, but only with the same pinned environment the preview's header
shows: `HOME`, and `XV_STATE_HOME`/`XDG_STATE_HOME` when one of them selected
the state root. The runner resolves the manifest path from its *own*
environment, so a job started with a different one refuses its own manifest
rather than guessing. Install first: the command has nothing to read until a
manifest exists.

#### What the scheduled job runs

Installed schedules are **target-pinned**. `xv schedule install` writes a
`manifest.json` and the unit runs only this:

```
xv schedule run --manifest <state dir>/schedules/rotation-default/manifest.json
```

Everything the sweep may act on comes from that file: the config file and its
digest, the `.xv.toml` path and environment that participated, the context
file, the workspace alias, the backend registry entry and its identity digest,
the real vault, the working directory the target was resolved in, the binary
path and version, the cadence, and the log path.

Before any backend is constructed the runner recomputes that target from the
recorded inputs and compares it with what the manifest says. A difference
refuses the sweep (see [Drift](#when-a-run-refuses-drift)). The sweep itself is
still `--due`-bounded — only secrets that already carry a policy and are already
past it — and the schedule never sets or changes a policy: `--every` is
deliberately not part of it, so a schedule cannot redefine what it is sweeping.

The unit contains an absolute binary path, the manifest path, `HOME`, the
recorded working directory, the log path, and — only when a variable selected
the state root — `XV_STATE_HOME` or `XDG_STATE_HOME`. **No credentials, no
secret values, no vault name, and no `XDG_CONFIG_HOME`.** The manifest already
names the exact configuration file and its digest, so a variable that redirects
config resolution would be a second, unversioned answer to "what does this job
rotate?" that can disagree with the manifest.

#### Where the schedule's files live

Under the per-user state directory, in `schedules/rotation-default/`:

| File | Written by | Lifetime |
|------|------------|----------|
| `manifest.json` | install/reinstall | removed by `uninstall` |
| `last-run.json` | the scheduled run | retained by reinstall and `uninstall` |
| `run.lock` | the scheduled run | persistent lock inode; retained |
| `install.lock` | install/reinstall/uninstall | persistent lock inode; retained |
| `recovery/` | an install rollback that could not finish | created only then; retained |

The rotation log lives outside that directory (`~/.local/state/xv/rotate.log` by
default, or wherever `--log-file` pointed) and is likewise never removed.
Everything is owner-private: `0700` directories and `0600` files on Unix, an
owner-and-SYSTEM DACL on Windows.

#### When a run refuses (drift)

The recorded target is the acknowledgement you gave at install time. If any
recorded input changed, the scheduled run refuses **before** constructing a
backend and exits with the ordinary configuration-error code, and
`xv schedule status` says the same thing without contacting the provider:

```text
[error] The installed systemd user timer rotation schedule is unsafe to run.
  Ownership: managed
  Target:    payments -> aws-prod/payments-production
  Drift:     refused
  - project_digest changed; review /home/alice/work/service/.xv.toml and reinstall
  - backend_identity changed for aws-prod; review the account/provider and reinstall
[hint] Review the changes, then run 'xv schedule install --vault payments' to accept the new target.
```

Reinstall — `xv schedule install --vault <alias-or-vault>` — is the only
operation that accepts a changed target, which is the point: an unattended job
that mutates secrets should need a fresh acknowledgement after the ground moves
under it. So **reinstall after** editing `xv.conf` or `.xv.toml`, changing the
active environment, switching account/subscription or backend identity,
re-pointing a workspace alias, or moving the directory you installed from.

Ambient variables are deliberately *ignored*: `XV_BACKEND`, `XV_ENV`, the
current directory and the ambient context file do not change what a scheduled
run does, because the run replays the recorded inputs instead.

**Upgrading `xv` in place** is the one allowed difference: the same executable
path reporting a new version is a warning, not a refusal, and the run proceeds.
Reinstalling afterwards is still worth doing — it refreshes the rendered units
and the recorded version. A binary that **moved or disappeared** is a refusal.

#### Replacing a legacy schedule

A schedule installed by an older `xv` runs `rotate --due --force` directly and
has no manifest. It is not migrated automatically, and nothing but an explicit
install ever writes a manifest. `xv schedule status` labels it and shows what it
actually runs:

```text
[warn] A legacy systemd user timer rotation schedule is installed.
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv rotate --due --force --vault payments-production
  Target:    unverified (the legacy unit does not record backend or account identity)
[hint] Replace it explicitly with 'xv schedule install --vault <alias-or-vault>'.
```

Run that install (add `--force` where there is no terminal) and the legacy unit
is replaced in the same transaction as any other reinstall.

A pinned unit whose `manifest.json` has gone — deleted by hand, or a state
directory that moved — is reported under the same `legacy-unpinned` label,
because its target can no longer be proven either. Status names the manifest it
was pinned to instead of claiming the unit recorded nothing:

```text
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv schedule run --manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
  Target:    unverified (the recorded manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json is missing)
```

That job would refuse itself at its next firing; reinstalling repairs it.

#### Install is transactional

Install and reinstall hold an exclusive `install.lock`, render everything in
memory, publish `manifest.json`, then write and register the native unit(s) and
**verify** that the scheduler really has an entry pointing at this executable,
this manifest, this cadence and this log path. If any of that fails, the prior
manifest and unit bytes are put back and the previous registration restored — a
failed reinstall leaves the schedule you had, and a failed first install leaves
nothing.

If the rollback *itself* fails, the prior bytes are written to owner-private
snapshots under `recovery/<UTC>-<artifact>` and the error reports both failures
and points at `xv schedule status`. Those snapshots are yours to inspect; `xv`
never reads them back on its own.

A file at an owned path that `xv` did not write — anything without its
`Managed by crosstache (xv schedule)` marker, or a symlink — is refused rather
than adopted or overwritten. `status` reports it as `foreign` and leaves it
alone.

#### What `uninstall` removes, and what it keeps

`xv schedule uninstall` deregisters the job and removes exactly two things: the
native unit file(s) or task entry, and `manifest.json`. It **keeps**
`last-run.json`, both lock files, `recovery/`, the rotation log, anything else
in the state directory, and any file at an owned path that `xv` did not write
(reported, not removed). Removing nothing is success — it is safe in teardown
scripts — but a scheduler that fails for a reason other than "no such job" is
reported as an error rather than as absence.

#### `XV_STATE_HOME`

The manifest lives under the per-user state directory
(`$XDG_STATE_HOME/xv/schedules/rotation-default/`, else
`~/.local/state/xv/...`; `%LOCALAPPDATA%\xv\...` on Windows).
`XV_STATE_HOME` overrides that root on every platform. It exists for tests and
embedding only — it is **not** a target-selection input, it changes nothing
about which vault or backend a schedule acts on, and an empty value is ignored.

Whichever variable selected the root at install time is pinned into the unit, so
the scheduled process computes the same manifest path the installing shell did.
Without that pin the two would disagree and the job would refuse its own
manifest at 3 a.m.

#### The limitation to plan around

A scheduled run has **no terminal**, so any credential needing interaction fails
there even though it works for you now. Azure CLI tokens work while the refresh
token is valid and the keyring is unlocked; managed identity, service principals,
and the local backend work unconditionally. Verify before trusting it:

```bash
xv rotate --due --force --vault v        # in a clean shell
xv schedule status                       # ownership, target and drift
cat ~/.local/state/xv/rotate.log         # what actually happened
```

Output is captured to that log on every platform (launchd `StandardOutPath`,
systemd `StandardOutput=append:`, a `>>` redirect for Task Scheduler), because a
3 a.m. failure with no record is indistinguishable from no failure at all.

For CI-driven rotation instead of host-driven, see
[`ci-cd.md`](ci-cd.md#rotation-gates-in-ci) — a scheduled workflow plus a
`--check` gate on pull requests.

---

## Where the schedule actually lives

AWS Secrets Manager rotates server-side: the service invokes a Lambda on its own
schedule. Azure Key Vault has no equivalent for secret *values* — it can
auto-rotate keys and emit near-expiry events, but nothing in it will regenerate a
secret. A local directory has no scheduler at all.

So the cadence has to come from somewhere outside the vault, and `xv schedule`
puts it in the host's own scheduler rather than in a daemon of its own. That
keeps reboot survival, catch-up after sleep, and log capture in the hands of
software that already does them correctly, and it means no long-lived `xv`
process holding credentials.

The division of labour: `xv` owns the policy, the due-date math, and the unit's
lifecycle; launchd/systemd/Task Scheduler owns the clock.

---

## Native rotation (AWS)

```bash
xv rotate DB_PASSWORD --native
```

Calls `RotateSecret`, which invokes the rotation Lambda configured on the secret;
rotation completes asynchronously. AWS-only, and errors with a capability hint on
other backends. `--native` cannot be combined with `--every`, `--due`, or
`--check`: the schedule and the new value are both AWS's to decide.

A secret can carry an `xv:rotate_every` policy on AWS too, but if AWS is already
rotating it on a schedule, tracking a second policy in tags is redundant.

---

## What rotation does not do

- **It does not restart anything.** An application that read the secret at
  startup keeps using the old value until it is restarted or redeployed.
  Rotation without a rollout plan is how a rotated credential becomes an
  outage — sequence them.
- **It does not update external systems.** For a database password, `xv rotate`
  changes the stored value, not the password on the database. Use
  `--generator` to hook a script that changes both, or `--native` on AWS where
  the Lambda owns that logic.
- **It does not fix a broken credential by itself.** If the scheduled sweep
  fails — expired login, locked keyring, a vault that moved — rotation silently
  stops happening until someone reads the log. Watch `rotate.log`, or run
  `xv rotate --check` from CI so a stale secret fails a pipeline rather than
  waiting to be noticed.
- **It does not purge old versions.** Previous values stay in version history
  (`xv history`) and, with `[local].git`, in commit history. That is deliberate —
  rollback needs them — but it means rotation is not a way to make an exposed
  value unrecoverable. Use `xv purge` for that, and remember it cannot rewrite
  git history that has already been pushed.
