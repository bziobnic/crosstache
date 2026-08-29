# TOTP Code Generation for Secret Records

**Date:** 2026-08-15
**Status:** Approved design; implementation not started
**Initial surface:** CLI only

## Goal

Add a one-shot `xv totp <secret>` command that generates the current RFC 6238
authentication code from an encrypted seed field attached to a typed xv secret.
The command copies the code by default and reports how many seconds remain before
it expires; `--raw` / `-r` prints only the code for scripts.

This feature generates authenticator codes. It does not use TOTP as an
additional gate before xv reveals a secret.

## Existing foundation

xv record secret fields are JSON-encoded inside the backend secret value and
marked with `content_type = application/vnd.xv.record`. Metadata fields instead
live in listable `f.*` tags. Keeper import already maps
`custom_fields.$oneTimeCode` to an encrypted envelope field named
`one-time-code`, and manual record creation/update can already attach the same
field with `--field-secret`.

The initial feature therefore needs no backend schema, record envelope, tag,
cache, or migration change. It adds a read-and-transform operation over the
existing encrypted record representation.

## Settled product decisions

1. The initial release is CLI-only. TUI and web/desktop support are future
   enhancements.
2. The public command is `xv totp <secret>`, not a mode of `xv get`.
3. Generation is one-shot. There is no live countdown or `--watch` mode.
4. The canonical field is `one-time-code`. `--field <name>` replaces that exact
   field name for custom records; xv does not search aliases.
5. Seed material may be a bare Base32 value or a standard
   `otpauth://totp/...` URI.
6. URI parameters for `algorithm`, `digits`, and `period` are honored. A bare
   seed uses SHA-1, six digits, and a 30-second period.
7. Default invocation copies the code and reports seconds to expiry.
   `--raw` / `-r` prints only the zero-padded code, with no newline or metadata.
8. A seed must be an encrypted record field. A listable metadata field is
   rejected even when its contents otherwise form a valid seed.

## Architecture

### Domain module

Create `src/totp.rs` as the backend-independent domain boundary. It owns:

- `DEFAULT_TOTP_FIELD`, whose value is `"one-time-code"`;
- parsing bare Base32 and `otpauth://totp` values;
- deterministic generation for an injected Unix timestamp;
- current-time generation through a small system-clock wrapper; and
- calculation of the current code's remaining lifetime from the same clock
  sample used to generate it.

The domain result carries a zero-padded code plus `expires_in_seconds`. Tests
call the deterministic timestamp API so no test depends on wall-clock timing.
The production wrapper samples the clock only after the backend read and seed
parsing are complete.

Use `totp-rs = 6.0.0` with default features disabled and only `std`, `otpauth`,
and `zeroize` enabled. This supplies RFC 6238 SHA-1/SHA-256/SHA-512 generation,
strict Key URI parsing, and zeroization of the library's secret-bearing types
without QR, random-seed, Steam, serde, or migration features. Version 6.0.0
requires Rust 1.88; xv currently builds on the stable toolchain and declares no
older MSRV.

Normative/reference material:

- [RFC 6238](https://www.rfc-editor.org/info/rfc6238/)
- [`totp-rs` documentation](https://docs.rs/totp-rs/6.0.0/totp_rs/)

### CLI module

Create `src/cli/totp_ops.rs` rather than adding another responsibility to the
already-large `src/cli/secret_ops.rs`. The module owns:

- workspace/backend/vault resolution;
- fetching the full secret value;
- verifying record and encrypted-field status;
- calling the domain generator;
- raw versus clipboard output; and
- user-facing errors and success messages.

Wire the command through `src/cli/commands.rs`, `src/cli/mod.rs`, and
`src/lib.rs`. Reuse the existing workspace-aware read resolver and backend
secret trait path used by `xv get`, so literal and workspace-qualified secret
names retain current resolution behavior on local, Azure, and AWS backends.

### Data flow

1. Parse `xv totp <secret> [--field <name>] [--raw|-r]`.
2. Resolve the workspace/backend/vault/secret target through the existing read
   resolver.
3. Fetch the secret with its value included. This remains an ordinary audited
   secret read at the backend boundary.
4. Require the exact record content type.
5. Parse the record envelope and select `one-time-code`, or the exact field
   supplied with `--field`.
6. If the requested field is absent from the encrypted envelope but present as
   an `f.<field>` tag, reject it as insecure metadata. If absent from both,
   report the known record fields without their values.
7. Parse the selected value as a TOTP URI when it starts with the `otpauth:`
   scheme; otherwise parse it as a bare Base32 seed.
8. Sample the system clock once, generate the code, and calculate expiry from
   that same sample and the effective period.
9. Either print the raw code or copy it and schedule clipboard clearing.

No normalized seed or generated code is written back to the backend. Running
the command cannot create a secret version or mutate tags.

## CLI contract

```bash
xv totp github
xv totp github --field authenticator-seed
xv totp github --raw
xv totp github -r
```

The command has no `--watch`, `--version`, seed-value argument, algorithm
override, period override, digit override, or command-specific output format in
the initial release. Global configuration and workspace selection continue to
behave normally where they apply.

### Default output

The code is copied to the clipboard and a success message is emitted without
printing the code itself:

```text
TOTP code for 'github' copied to clipboard (expires in 17s; clipboard clears in 17s)
```

When clipboard clearing is enabled, its delay is:

```text
min(config.clipboard_timeout, expires_in_seconds)
```

The success message reports both values when they differ. When
`clipboard_timeout = 0`, xv does not schedule a clear and reports only code
expiry. Clipboard failure never falls back to stdout because that would reveal
the code unexpectedly; the error recommends rerunning with `--raw`.

### Raw output

`--raw` and `-r` bypass the clipboard and write only the zero-padded code to
stdout, with no label, expiry text, ANSI formatting, or trailing newline. This
keeps command substitution predictable:

```bash
code="$(xv totp github --raw)"
```

## Parsing and generation rules

### Bare Base32

- Trim leading/trailing whitespace.
- Remove ASCII whitespace used for visual grouping and normalize ASCII letters
  to uppercase.
- Reject an empty value or any remaining character outside unpadded RFC 4648
  Base32.
- Decode through `totp-rs` and enforce its RFC-compliant minimum secret length.
- Use SHA-1, six digits, and a 30-second period.

### Key URI

- Accept only `otpauth://totp/...`; reject HOTP, Steam, HTTP(S), and other
  schemes/hosts.
- Require a non-empty `secret` query parameter.
- Honor supported SHA-1, SHA-256, and SHA-512 algorithms.
- Honor six through eight digits and a non-zero period.
- Preserve strict URI validation, including malformed percent encoding and
  inconsistent issuer labels.
- Ignore unrelated extension query parameters only where `totp-rs` does so;
  xv does not invent interpretations for them.

### Expiry

For period `P` and sampled Unix timestamp `T`, the displayed lifetime is:

```text
P - (T mod P)
```

At an exact boundary the newly generated code reports the full period. Code and
expiry always derive from the same timestamp, so a second clock read cannot pair
an old code with a new period or vice versa.

## Security properties

- The seed must come from the encrypted record envelope, never a metadata tag.
- The backend's returned secret value remains zeroizing. Envelope extraction
  must use an RAII guard that zeroizes every temporary parsed envelope value on
  all success and error exits. The selected seed moves into a
  `Zeroizing<String>` rather than being copied into an unguarded long-lived
  string.
- Enable the TOTP library's `zeroize` feature so decoded secret bytes and
  builder/TOTP state are cleared on drop.
- Convert the generated token into a `Zeroizing<String>` while it moves through
  clipboard/raw-output code.
- Never include the seed, URI, or generated code in errors, tracing, debug
  output, cache metadata, audit payloads, or default success text.
- Error tests use a recognizable sentinel seed and assert that rendered errors
  do not contain it.
- Clipboard clearing follows xv's existing detached clear mechanism and is
  shortened to the code lifetime. Disabling clipboard clearing remains an
  explicit user configuration choice.
- `--raw` is an intentional disclosure to stdout and never also touches the
  clipboard.

## Errors

The command fails before generation or clipboard access when:

- the target is not a typed record;
- the requested field is missing;
- the field exists only as metadata (the error recommends storing it with
  `--field-secret`);
- the field is empty;
- bare Base32 is malformed or too short;
- a URI is not a TOTP Key URI, lacks a secret, or contains invalid algorithm,
  digits, period, label, issuer, or encoding data;
- the system clock is before the Unix epoch or otherwise cannot yield a valid
  timestamp; or
- normal workspace, backend, vault, authorization, or secret lookup fails.

Errors may name the secret, field, and invalid parameter category. They must not
echo the field value. Existing xv error classification and exit-code behavior
remain unchanged; invalid TOTP material is treated as invalid configuration or
input rather than an authentication failure.

## Testing

### Domain unit tests

In `src/totp.rs`:

- RFC 6238 Appendix B vectors for SHA-1, SHA-256, and SHA-512 at fixed
  timestamps;
- bare-seed defaults, lowercase/visual-whitespace normalization, and preserved
  zero padding;
- URI algorithm, digits, and period handling;
- expiry at one second before a boundary, at the boundary, and with a custom
  period;
- empty, malformed, too-short, wrong-scheme, HOTP, missing-secret,
  invalid-algorithm, invalid-digits, and zero-period inputs; and
- sentinel-secret redaction from every public error string.

### CLI unit tests

In `src/cli/commands.rs` and `src/cli/totp_ops.rs`:

- parsing of the command, canonical default, explicit `--field`, `--raw`, and
  `-r`;
- exact raw rendering with no newline;
- default success text with expiry;
- clipboard delay selection when configured timeout is shorter, longer, equal,
  or disabled; and
- clipboard failure behavior without stdout fallback.

Make output-message and delay selection a pure helper so most coverage does not
touch the process-global operating-system clipboard.

### Hermetic end-to-end tests

Create `tests/e2e_totp.rs` using the existing isolated local-backend harness:

- canonical `one-time-code` field from a bare seed;
- canonical field from a Keeper-compatible URI;
- explicit custom-field override;
- exact `--raw` stdout and empty success chatter;
- URI parameters producing the expected fixed-width code;
- workspace-qualified lookup;
- untyped secret, missing field, metadata-only field, malformed seed, and
  malformed URI failures; and
- proof that generation does not create another secret version or mutate the
  record.

Where wall-clock generation prevents a stable literal assertion, compute the
set of valid codes around the invocation's start/end timestamps or exercise a
long custom period. Fixed-vector correctness stays in deterministic domain
tests.

### Verification commands

The implementation plan will include, at minimum:

```bash
cargo fmt --check
cargo test totp
cargo test --test e2e_totp
cargo test
cargo check --all-features
```

## Documentation

- Add `xv totp` examples and its output/security contract to `README.md`.
- Add CLI TOTP generation to `docs/FEATURES.md`.
- Extend `docs/keeper.md` so the existing `$oneTimeCode` mapping shows both
  seed retrieval and current-code generation.
- Add an Unreleased entry to `CHANGELOG.md`.
- Keep TUI and web/desktop support clearly labeled as deferred rather than
  implying parity.

## Compatibility and migration

- Existing untyped and typed secrets remain byte-for-byte unchanged.
- Existing Keeper-imported records work without re-import because they already
  store `one-time-code` as encrypted material.
- Custom fields continue to work through the explicit `--field` override.
- No built-in record type gains a new declared field in this phase; doing so
  would change interactive and UI editing surfaces that are intentionally out
  of scope.
- No migration, backend capability flag, or cache invalidation is needed.

## Deferred scope

- TUI code display/copy actions.
- Web and desktop UI display, countdown, and copy actions.
- Continuously refreshing output or `--watch`.
- QR-code rendering.
- Seed generation, enrollment, or account provisioning.
- HOTP, Steam tokens, or vendor-specific OTP formats.
- Verification of a user-supplied code.
- Historical-version generation.
- Automatic alias discovery beyond `one-time-code`.
- Command-line overrides for algorithm, digits, or period.
