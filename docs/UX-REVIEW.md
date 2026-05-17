# crosstache `xv` CLI UX Review

Scope: deep-dive review of the `xv` CLI for discoverability, command naming,
error messages, configuration model, and first-run/onboarding, especially for a
user moving between Azure and AWS backends.

Verification baseline:

```text
$ cargo build --release --features aws
Finished `release` profile [optimized] target(s) in 2m 53s

$ cargo build --release
Finished `release` profile [optimized] target(s) in 4m 30s
```

The AWS-enabled binary was exercised for the AWS-specific behavior. The default
binary was exercised for default-install behavior.

## P0 — Critical

### 1. Project envs, legacy env profiles, and vault contexts present three conflicting "environment" systems

Problem: `xv` exposes three overlapping concepts with nearly identical names:

- `.xv.toml` `[env.<name>]` profiles, selected by `default_env`, `--env`, or `XV_ENV`.
- `xv env ...` global profiles stored outside `.xv.toml`.
- `xv context ...` vault contexts, where `context use <name>` means vault name, not env name.

Concrete repro/evidence:

```text
$ cat .xv.toml
default_env = "dev"

[env.dev]
backend = "azure"
vault = "real-dev-vault"
resource_group = "rg-dev"

[env.prod]
backend = "aws"
vault = "prod-prefix"

$ xv context envs
config: /tmp/tmp.eDfcRzLxz1/proj/.xv.toml
default_env: dev

envs:
  * dev  vault=real-dev-vault  rg=rg-dev
    prod  vault=prod-prefix  rg=(unset)

$ xv env list
[info] No environment profiles found.
Create one with: xv env create <name> --vault <vault> --group <group>

$ xv env use dev
error[xv-config-invalid]: Configuration error: Environment profile 'dev' not found

$ xv context use dev
[ok] Switched to vault 'dev' (global context)
   Resource Group: Vaults

$ xv context show
Current Vault Context:
  Vault: dev
  Resource Group: Vaults
  Subscription: sub-123
  Last Used: 2026-05-16 22:22:14 UTC
  Usage Count: 1
  Scope: Global

active env: dev (from /tmp/tmp.eDfcRzLxz1/proj/.xv.toml)
  vault: real-dev-vault
  resource_group: rg-dev
```

Impact: a user who reasonably tries `xv env use dev` or `xv context use dev`
after seeing `[env.dev]` either gets a false "not found" error or silently
creates an active vault context for a nonexistent vault literally named `dev`.
The resulting `context show` displays both `Vault: dev` and `active env: dev /
vault: real-dev-vault`, making the effective target ambiguous.

Proposed fix: make `.xv.toml` env profiles the single visible environment model.
Deprecate or rename legacy `xv env` to `xv profile` or `xv legacy-env`, and make
`xv env list/use/show/create` operate on `.xv.toml` by default. Change
`xv context use <name>` help and output to say "vault", and reject names that
match a `.xv.toml` env with a targeted message:

```text
"dev" is an env profile, not a vault. Use `xv --env dev ...` or `xv env use dev`.
```

### 2. AWS auth failures are reported as network timeouts

Problem: when AWS credentials cannot be resolved, `xv` returns `xv-network` and
claims `timeout or dispatch failure`. This hides the real fix: configure usable
AWS SDK credentials.

Concrete repro/evidence:

```text
$ xv --backend aws list
error[xv-network]: Network error: aws ListSecrets: timeout or dispatch failure
exit=30
```

This was reproduced with `backend = "aws"`, `[aws].region = "us-east-1"`, and no
resolvable credentials in an isolated home/config directory.

Impact: users who have used newer `aws login` flows may have cached CLI login
state but no credential source the Rust AWS SDK can resolve. The current message
sends them toward proxies, DNS, or AWS service health instead of credentials.

Proposed fix: classify AWS SDK credential-resolution failures as
`xv-auth-failed`, not `xv-network`. Add an AWS-specific hint:

```text
No AWS credentials resolved for profile "default" in us-east-1.
Try `aws configure`, set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, or run
`eval "$(aws configure export-credentials --format env)"` after `aws login`.
```

### 3. A default build silently cannot use AWS but does not clearly say so

Problem: AWS is a compile-time feature, but the default binary hides that behind
a generic backend-registry failure. The explicit "AWS backend not compiled in"
message exists in source but is swallowed before the command-level error.

Concrete repro/evidence:

```text
$ xv --backend aws list  # default build
error[xv-config-invalid]: Configuration error: No backend registry available. Run 'xv config show' to check your configuration.
exit=3

$ xv --help --show-options | rg -A3 -B1 -- '--backend'
      --backend <BACKEND>
          Secrets backend to use (overrides config file and XV_BACKEND env var). Valid values:
          azure, local
```

By contrast, other help text does mention AWS:

```text
$ xv help migrate
--from <FROM>  Source backend (azure, local, aws)
--to <TO>      Target backend (azure, local, aws)
```

Impact: an AWS user cannot tell whether they have misconfigured `xv.conf`, used
the wrong flag, or installed a binary without AWS support. The top-level help
also contradicts subcommands that advertise AWS.

Proposed fix: make feature availability explicit at startup and in help. For a
binary without AWS support, `--backend aws` should fail before command dispatch:

```text
error[xv-backend-unavailable]: AWS backend is not included in this build.
Install the AWS build or rebuild with `cargo build --release --features aws`.
```

Also update `xv version` to list compiled backends, and make `--backend` help
show either `azure, local, aws` or `azure, local (aws unavailable in this build)`.

## P1 — High

### 1. Read-only discovery commands can fail before showing `.xv.toml` envs

Problem: on a fresh machine, even commands that should help the user discover
project configuration can fail on missing Azure global config.

Concrete repro/evidence:

```text
$ xv context envs
error[xv-config-invalid]: Configuration error: Subscription ID is required

$ xv env list
error[xv-config-invalid]: Configuration error: Subscription ID is required

$ xv context show
error[xv-config-invalid]: Configuration error: Subscription ID is required
```

Impact: the user cannot inspect the local `.xv.toml` until they already know how
to satisfy unrelated global Azure settings. This is especially bad in a
multi-backend repo where the target env may be AWS or local.

Proposed fix: never require backend validation for pure discovery commands:
`context envs`, `context show`, `env list`, `env show`, `config path`, and
`config show`. Load config without validation, show available project envs, and
surface validation issues as warnings scoped to the active backend.

### 2. There is no visible command to activate a `.xv.toml` environment

Problem: `.xv.toml` env selection exists through `default_env`, `XV_ENV`, and the
global `--env` flag, but the command tree makes users look for `xv env use`.
That command operates on the legacy user-scoped profile store instead.

Concrete repro/evidence:

```text
$ xv --help --show-options
      --env <ENV>
          Active environment from the resolved .xv.toml (overrides default_env). Lower priority than
          the XV_ENV env var

$ xv help env
Commands:
  list    List available environment profiles
  use     Use an environment profile (sets vault and group context)
  create  Create a new environment profile
```

Impact: the correct path is a hidden global flag, while the obvious command is
wrong for project envs. This is the highest-friction point in the Azure-to-AWS
switching workflow.

Proposed fix: provide a first-class `xv env use <name>` that writes or updates
`default_env` in the nearest `.xv.toml`, with `--global` or a renamed command for
the legacy profile store. At minimum, make legacy `xv env use dev` detect a
matching `.xv.toml` profile and print the exact command to use.

### 3. Backend selection is split across global config, `.xv.toml`, env vars, and flags with no diagnostic

Problem: backend resolution is powerful but invisible. The effective backend
can come from `--backend`, active `.xv.toml` env, global `xv.conf`, `XV_BACKEND`,
or the Azure default. Help and error output rarely say which layer won.

Concrete repro/evidence:

```text
$ xv --help --show-options
      --backend <BACKEND>
          Secrets backend to use (overrides config file and XV_BACKEND env var). Valid values:
          azure, local

$ xv context envs
envs:
  * dev  vault=real-dev-vault  rg=rg-dev
    prod  vault=prod-prefix  rg=(unset)
```

`context envs` omits each env's backend, even though `[env.prod]` had
`backend = "aws"` in the test file.

Impact: a multi-backend user can think they switched to AWS by selecting an env,
while the command still falls back to Azure, or vice versa. Troubleshooting then
requires knowing implementation precedence.

Proposed fix: add `xv config doctor` or `xv config resolve` that prints effective
env, backend, vault, resource group/prefix, region/profile, and the source of
each value. Also include `backend=<value>` in `context envs` output.

### 4. `xv context init` cannot create an AWS or local env profile

Problem: `xv context init` is the documented project-config onboarding command,
but it only accepts env, vault, and resource group. There is no `--backend`,
`--region`, `--aws-profile`, or AWS prefix/default-vault option for the env it
creates.

Concrete repro/evidence:

```text
$ xv help context init
Usage: xv context init [OPTIONS]

Options:
      --env <ENV>
      --vault <VAULT>
      --resource-group <RESOURCE_GROUP>
      --non-interactive
      --force
      --aws-profile <AWS_PROFILE>
      --region <REGION>
```

The `--aws-profile` and `--region` flags are inherited global runtime overrides;
they do not write an AWS env profile.

Impact: first-run project setup is Azure-shaped even in a repo that needs AWS
and Azure side by side. Users must discover and hand-edit `backend = "aws"` in
`.xv.toml`.

Proposed fix: add backend-aware project init:

```text
xv env create dev --backend azure --vault real-dev-vault --resource-group rg-dev
xv env create prod --backend aws --vault prod-prefix --region us-east-1 --profile prod
xv env use prod
```

If keeping `context init`, add `--backend` and backend-specific prompts, then
write the complete `.xv.toml` profile.

### 5. Generic config hints send users to the wrong repair path

Problem: many distinct failures collapse to `xv-config-invalid` and use the same
hint: inspect config or rerun init. Examples include missing legacy env profile,
missing backend registry, and missing subscription ID.

Concrete repro/evidence:

```text
$ xv env use dev
error[xv-config-invalid]: Configuration error: Environment profile 'dev' not found

$ xv --backend aws list  # default build
error[xv-config-invalid]: Configuration error: No backend registry available. Run 'xv config show' to check your configuration.

$ xv context envs
error[xv-config-invalid]: Configuration error: Subscription ID is required
```

Impact: the same error family covers unrelated actions with unrelated fixes. New
users cannot tell whether to edit `.xv.toml`, create a legacy profile, install an
AWS build, run `xv init`, or set Azure variables.

Proposed fix: split config errors into actionable codes, for example
`xv-global-config-missing`, `xv-project-env-not-found`,
`xv-legacy-profile-not-found`, and `xv-backend-unavailable`. Hints should be
backend-aware and mention the exact key or command to change.

## P2 — Medium

### 1. Top-level product framing still says Azure-only

Problem: the top-level help describes `xv` as only an Azure Key Vault tool even
though the CLI now has local and AWS backend paths.

Concrete repro/evidence:

```text
$ xv --help
A comprehensive tool for managing Azure Key Vault
Usage: xv [OPTIONS] <COMMAND>
```

Impact: AWS and local-backend users start with the impression that their path is
bolted on or unsupported. It also makes generic commands like `migrate`,
`inject`, `run`, and `scan` look Azure-specific when they are not.

Proposed fix: change the about text to backend-neutral language, for example:

```text
Manage secrets across Azure Key Vault, AWS Secrets Manager, and local stores.
```

### 2. AWS options appear on every command, including commands where they do nothing

Problem: `--aws-profile` and `--region` are global options shown under almost
every subcommand, including `config`, `context`, `env`, `completion`, `version`,
and `init`.

Concrete repro/evidence:

```text
$ xv help version
Options:
      --aws-profile <AWS_PROFILE>  Override the AWS profile for this invocation (only honored when active backend is aws)
      --region <REGION>            Override the AWS region for this invocation (only honored when active backend is aws)

$ xv help config
Options:
      --aws-profile <AWS_PROFILE>  Override the AWS profile for this invocation (only honored when active backend is aws)
      --region <REGION>            Override the AWS region for this invocation (only honored when active backend is aws)
```

Impact: help output feels noisy and suggests AWS runtime flags might affect
local configuration commands. This makes the command surface harder to scan.

Proposed fix: hide backend runtime overrides from commands that do not touch a
backend, or group global backend overrides only in `xv --help --show-options`.

### 3. `.xv.toml` and `xv.conf` have overlapping backend fields with incomplete naming consistency

Problem: `.xv.toml` supports `backend = "aws"` under `[env.<name>]`; global
`xv.conf` supports top-level `backend = "aws"` and `[aws]`; named backends exist
in settings source but `.xv.toml` explicitly does not support them. Help does
not explain this boundary.

Concrete repro/evidence:

```text
$ xv --help --show-options
--backend <BACKEND>
    Secrets backend to use (overrides config file and XV_BACKEND env var).

$ xv help migrate
--from <FROM>
    Source backend (azure, local, aws)
```

Source/docs evidence: `docs/env-profiles.md` says named backend keys under
`[backends.*]` are not supported in `.xv.toml`, while `Config.named_backends`
exists in `src/config/settings.rs`.

Impact: users with multiple AWS accounts/regions need named backend instances,
but env profiles can only point at canonical backend kinds. This limits the
actual multi-backend story and creates hidden coupling to one global `[aws]`.

Proposed fix: let `.xv.toml` envs reference named backend keys, for example
`backend = "aws-prod"` where `[backends.aws-prod] type = "aws"` is defined in
global config, or support full inline AWS env fields in `.xv.toml`.

### 4. `context envs` does not show enough of the effective profile

Problem: env listing shows only `vault` and `rg`; it omits backend, group,
folder, AWS region/profile source, and whether values are inherited.

Concrete repro/evidence:

```text
$ xv context envs
envs:
  * dev  vault=real-dev-vault  rg=rg-dev
    prod  vault=prod-prefix  rg=(unset)
```

Impact: users cannot tell from the list that `prod` is AWS. The output invites
them to think every row is an Azure vault/resource-group pair.

Proposed fix: include backend and backend-specific labels:

```text
* dev   backend=azure  vault=real-dev-vault  rg=rg-dev
  prod  backend=aws    prefix=prod-prefix    region=us-east-1 profile=prod
```

### 5. Backend unsupported operations are framed in Azure terms

Problem: several commands are Azure concepts (`vault share`, `audit`,
blob-backed `file` commands), but they are visible regardless of active backend.
Some help text uses Azure-specific nouns without saying whether AWS/local can
support the operation.

Concrete repro/evidence:

```text
$ xv help audit
Show audit history for secrets or vaults
Options:
      --resource-group <RESOURCE_GROUP>
          Azure resource group (defaults to config value)

$ xv help file
File management commands
Commands:
  upload    Upload one or more files to blob storage
```

Impact: AWS users cannot distinguish generic secret operations from Azure-only
or blob-only operations until after they run them.

Proposed fix: annotate help with backend support, for example
`audit (azure only)` and `file (azure blob storage)`, or hide unsupported command
families when a backend-specific help mode is introduced.

## P3 — Low/Polish

### 1. Help hides global options by default, which hides the `.xv.toml` activation flag

Problem: the default help deliberately hides global options and tells users to
run `--show-options`. That keeps help short, but it hides `--env`, the only
current project-env selection affordance.

Concrete repro/evidence:

```text
$ xv --help
Options:
-h, --help       Print help (see more with '--show-options')
-V, --version    Print version

$ xv --help --show-options
      --env <ENV>
          Active environment from the resolved .xv.toml ...
```

Impact: users reading normal help will not learn how to switch project envs.

Proposed fix: keep `--env` visible in the default help, or add a command-facing
path such as `xv env use` so this does not depend on hidden global options.

### 2. `xv env create` uses `--group` where adjacent commands say `resource_group`

Problem: `xv env create` asks for `--group`, while `.xv.toml` uses
`resource_group` and `context init` uses `--resource-group`.

Concrete repro/evidence:

```text
$ xv help env create
Usage: xv env create [OPTIONS] --vault <VAULT> --group <GROUP> <NAME>

Options:
      --group <GROUP>  Resource group for the vault

$ xv help context init
      --resource-group <RESOURCE_GROUP>
          Resource group for the env
```

Impact: `group` also means secret grouping elsewhere (`xv list --group`,
`xv run --group`, `xv inject --group`). Reusing it for Azure resource group
raises the chance of misconfiguration.

Proposed fix: rename to `--resource-group` and keep `--group` as a deprecated
alias with a warning.

### 3. Generic AWS inherited flags are visually louder than command-specific flags

Problem: on many help pages, inherited `--aws-profile` and `--region` sit among
core command options and can interrupt scanning.

Concrete repro/evidence:

```text
$ xv help set
Options:
      --stdin
      --note <NOTE>
      --folder <FOLDER>
      --expires <EXPIRES>
      --not-before <NOT_BEFORE>
      --aws-profile <AWS_PROFILE>
      --region <REGION>
```

Impact: the most common secret-setting options are visually mixed with backend
override plumbing.

Proposed fix: group inherited global options under a separate "Global options"
section or hide them behind `--show-options` at subcommand level.

### 4. Build warnings add noise to first source-install experience

Problem: both required builds emit dead-code warnings before completion.

Concrete repro/evidence:

```text
$ cargo build --release --features aws
warning: function `execute_secret_set` is never used
warning: function `execute_secret_get` is never used
...
Finished `release` profile [optimized] target(s) in 2m 53s

$ cargo build --release
warning: function `execute_secret_set` is never used
...
Finished `release` profile [optimized] target(s) in 4m 30s
```

Impact: source installers may wonder whether their binary is healthy,
especially when selecting optional AWS support.

Proposed fix: clean up unused functions or gate transitional code with targeted
`#[allow(dead_code)]` plus comments. Keep release builds warning-clean.

### 5. Existing docs explain the distinction, but the CLI does not surface it at the moment of confusion

Problem: `docs/env-profiles.md` does explain that `xv env` manages global
user-scoped profiles and `.xv.toml` envs are project-scoped. The CLI output does
not point users there when they hit the mismatch.

Concrete repro/evidence:

```text
$ xv env use dev
error[xv-config-invalid]: Configuration error: Environment profile 'dev' not found
```

Impact: the answer exists, but only for users who already know which doc to
read. The error path loses the chance to repair the user's mental model.

Proposed fix: when a legacy env lookup fails and a `.xv.toml` exists, print:

```text
No legacy user profile named "dev".
Found project env "dev" in .xv.toml. Use `xv --env dev <command>` or `xv env use dev`
after migrating `xv env` to project envs. See docs/env-profiles.md.
```
