# Desktop Onboarding and Product Polish Design

**Date:** 2026-07-22 · **Status:** Approved design

**Backlog coverage:** items 5 and 14

## Goal

Let a user reach a working vault from an unconfigured or broken desktop launch,
and finish the product hierarchy, preferences, help, and Crosstache-specific
visual identity without weakening the CLI-first architecture.

## Desktop startup states

The packaged desktop app starts in one of four explicit states:

1. loading configuration;
2. connecting to the effective backend and vault;
3. setup required;
4. recoverable startup failure.

Loading identifies the current phase without showing credentials or tokens. A
successful connection navigates to the shared embedded UI. Failure remains in
the local bundled frontend and never exposes the tokenized server URL.

## First-run setup

Setup offers:

- **Create local vault:** choose store location and vault name, generate the age
  identity through shared local setup services, write configuration safely, and
  verify a list operation.
- **Connect Azure:** collect the same non-secret values supported by `xv init`,
  use the shared credential chain, and verify tenant/subscription/vault access.
- **Connect AWS:** collect region, profile, and vault prefix, use the shared AWS
  credential chain, and verify access.
- **Advanced configuration:** show the configuration path and exact equivalent
  CLI commands without requiring the user to leave setup.

The shared Rust setup service performs parse, validation, permission-safe
write, and verification. It writes a temporary file and atomically replaces the
configuration only after validation. Failures leave prior configuration
unchanged. Secret credentials are never accepted by the desktop form when the
provider’s standard credential chain can supply them.

## Recovery screen

Recoverable startup failures show stable error code, operation, effective
backend/vault, safe message, hint, and expandable diagnostics. Actions are
Retry, Choose backend, Open configuration, Copy diagnostics, and Show CLI
command. Authentication failures name the supported provider login action.
Configuration failures identify the exact file and field when safe.

`xv ui` does not gain these write paths. Its token/configuration recovery screen
shows exact CLI remediation and remains persistent.

## Visual hierarchy and preferences

The approved vault-workspace direction uses existing green tokens refined into
a disciplined context-led system:

- dark forest context rail;
- mint connection and primary-action accent;
- quiet neutral canvas and high-contrast surfaces;
- red reserved for destructive or failed states;
- system body typography with a distinct utility treatment for context and
  identifiers.

The context rail is the signature element. Repeated “Your secrets/files” and
generic safety prose are reduced after first use so content and scope stay above
the fold. Motion is limited to one orchestrated sheet transition, progress,
and state changes; reduced-motion preferences remove them.

Settings include System/Light/Dark theme, protected-value timeout bounded by
security policy, density, and reset-layout actions. Help includes keyboard
shortcuts, backend capability explanations, local-session security model,
configuration path, app/CLI version, and copyable diagnostics.

## Platform behavior

Desktop setup is macOS-only while the current Tauri package is macOS-only. The
shared setup services and CLI remain cross-platform. Presentation preferences
are portable and versioned. Unknown future preference fields are ignored; old
versions migrate without discarding recognized values.

## Acceptance evidence

- Rust tests cover local/Azure/AWS setup validation, atomic writes, preservation
  on failure, verification, and safe diagnostics.
- Desktop frontend tests cover every startup state, Retry, backend choice,
  configuration path, copied diagnostics, and prevention of secret logging.
- Isolated smoke tests cover fresh local setup and representative invalid Azure
  and AWS configurations.
- Browser and visual tests cover theme choices, reduced motion, context
  hierarchy, Help, Settings, minimum window size, and phone rendering.
- Final package verification builds and launches the signed-independent release
  bundle against an isolated local vault.
