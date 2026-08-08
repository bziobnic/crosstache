# `xv doctor` Configuration Repair Design

## Goal

Add a recovery command for global `xv.conf` failures that can prevent `xv ui`
and every ordinary CLI command from starting. `xv doctor` diagnoses the file,
automatically applies deterministic repairs, preserves the exact original in a
timestamped backup before writing, and clearly reports problems that require a
person to resolve.

This command covers the global configuration file returned by `xv config path`.
It does not diagnose project `.xv.toml` files, credentials, network access, or
backend service availability.

## Command and Bootstrap Path

`doctor` is a top-level command:

```text
xv doctor
```

The command must be recognized and dispatched before the normal
`load_config_no_validation` path. This is essential: ordinary command dispatch
first deserializes `xv.conf`, so routing `doctor` through that path would make it
unavailable for exactly the corrupt or outdated files it must repair.

The early bootstrap should remain narrow. Normal commands keep their existing
loading and backend-resolution behavior; only `doctor` reads the configuration
file independently.

## Diagnostic and Repair Flow

1. Resolve and print the global configuration path.
2. If the file does not exist, report that built-in defaults can be loaded and
   exit successfully without creating a file.
3. Read the existing bytes without following a final-component symlink, using
   the project's existing sensitive-file safety conventions.
4. Parse the contents as an editable TOML document, then try the same JSON
   fallback accepted by normal configuration loading.
   - If neither format parses, the syntax is not automatically repairable.
     Report the TOML error location and parser diagnostic, leave the file
     unchanged, and exit with the configuration-error status.
   - Doctor diagnoses valid legacy JSON as healthy but does not rewrite it
     solely to convert formats. If repair is required, it may serialize a
     repaired TOML replacement after backing up the JSON original.
5. Apply only deterministic schema repairs:
   - Add missing required top-level scalar fields from `Config::default()`.
   - When an optional `[blob_config]` table exists, add any missing required
     fields from `BlobConfig::default()`.
   - Apply explicit legacy-key migrations from a small, reviewed migration
     table when such mappings exist. The initial implementation need not invent
     a migration where the repository has no known renamed key.
   - Preserve comments, formatting, unknown keys, and all valid user values for
     editable TOML input.
6. Deserialize the candidate into `Config`, apply the same environment
   overrides used by ordinary startup without persisting those environment
   values, then run `Config::validate()` on that effective candidate.
   Type mismatches, invalid enum values, malformed nested structures, missing
   credentials, and ambiguous values are reported but never guessed or
   replaced.
7. If deterministic repairs changed the document, create a timestamped sibling
   backup containing the exact original bytes, then atomically write the
   repaired file with private permissions. A backup or write failure is fatal;
   doctor must not claim a repair succeeded.
8. Re-read, deserialize, and validate the written file. Report both applied
   repairs and any remaining manual actions.

Deterministic repairs may still be written when a separate semantic problem
remains—for example, adding newly required defaults while reporting that an
Azure subscription ID is still missing. The original is always recoverable from
the backup.

## Backup Semantics

Backups are sibling files named with a UTC timestamp:

```text
xv.conf.backup-20260807T153012Z
```

The backup is created only when doctor is about to change the configuration.
It must use create-new semantics so an existing backup is never overwritten,
must not follow symlinks, and must receive private file permissions. Doctor
prints the backup path after successful creation.

## Output and Exit Status

Human-readable output lists checks in execution order and uses plain status
labels such as `ok`, `fixed`, and `error`. It must include:

- the inspected path;
- every applied repair;
- the backup path when a write occurred;
- every unresolved problem; and
- a concrete manual next step when one is available.

Exit status is `0` when the file was already healthy, was absent, or was fully
repaired. Doctor returns the existing configuration-error exit status (`3`)
when any syntax, schema, semantic, backup, write, or post-write verification
error remains. Diagnostics must not expose credentials or secret configuration
values.

## Internal Boundaries

The repair engine belongs in the configuration module and accepts an explicit
path so it can be tested without environment mutation. It returns a structured
report containing checks, repairs, optional backup path, and unresolved issues.
The CLI handler is responsible only for locating the global file, rendering the
report, and translating unresolved issues into the existing configuration error
type.

Filesystem mutation is isolated behind helpers for backup creation and atomic
replacement. Schema-default insertion is isolated from parsing and persistence
so new repair rules can be reviewed and tested independently.

## Testing

Unit tests exercise the repair engine with temporary paths:

- no file;
- healthy current TOML with no write or backup;
- incomplete older TOML repaired with current defaults;
- incomplete `[blob_config]` repaired from its defaults;
- comments, formatting, unknown keys, and user values preserved;
- malformed syntax reported without mutation;
- invalid type or enum reported without guessing;
- semantically invalid configuration reported clearly;
- exact backup contents and timestamped non-overwriting name;
- atomic private repaired-file write;
- backup or write failure reported without false success; and
- reparsing/post-write verification.

CLI integration tests prove that `xv doctor`:

- appears in top-level help;
- runs when ordinary config loading fails;
- exits `0` for healthy and fully repaired configurations;
- exits `3` and prints actionable diagnostics for unresolved errors; and
- reports the backup path and repaired fields.

The implementation follows test-driven development: each behavior is first
captured by a failing test, then implemented minimally, with the focused suite
and full repository checks run before completion.
