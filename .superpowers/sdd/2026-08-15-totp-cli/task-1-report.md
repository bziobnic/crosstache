# Task 1 report: RFC 6238 domain engine

## Implementation

- Added `totp-rs` 6.0.0 with only `std`, `otpauth`, and `zeroize` features, plus the explicit Rust 1.88 package floor.
- Added `src/totp.rs` and exported it from the library and binary modules.
- Added `DEFAULT_TOTP_FIELD` (`one-time-code`), `GeneratedTotp`, deterministic `generate_at`, and clock-based `generate_current`.
- Bare seeds are normalized by removing ASCII visual whitespace and uppercasing, then strictly validated as unpadded RFC 4648 Base32 and built with SHA-1, six digits, and a 30-second period.
- `otpauth://totp` URIs are parsed with their algorithm, digits, and period preserved; non-TOTP schemes and malformed/invalid parameters are rejected.
- Token calculation and expiry calculation are centralized in the private `generate_for_totp` helper. Both public functions parse first; `generate_current` samples `SystemTime` once.
- Generated code is held in `zeroize::Zeroizing<String>`. `GeneratedTotp` intentionally has no `Debug`, `Display`, `Serialize`, or `Clone` implementation.
- Parser errors use generic safe messages and tests verify that neither the seed nor full URI is echoed.

## Files changed

- `Cargo.toml`
- `Cargo.lock`
- `src/lib.rs`
- `src/main.rs`
- `src/totp.rs`

## TDD evidence

### RED

After adding the seven requested tests and module declarations, before adding the production API:

```text
cargo test totp::tests --lib
error[E0425]: cannot find function `generate_at` in this scope
error: could not compile `crosstache` due to 7 previous errors
```

This was the expected missing-API compile failure.

### GREEN

After adding the minimal parser/generator implementation:

```text
cargo test totp::tests --lib
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1182 filtered out
```

The full library suite also passed:

```text
cargo test --lib
test result: ok. 1188 passed; 0 failed; 1 ignored; 0 measured
```

Other verification:

- `cargo check` passed after dependency resolution.
- `cargo fmt` and `cargo fmt --check` passed.
- `git diff --check` passed.
- `cargo tree -e features -i totp-rs` shows `alloc`, `std`, `otpauth`, and `zeroize`; it does not show `qr`, `steam`, `gen_secret`, `serde`, or `migration` as enabled `totp-rs` features.

## Self-review

- The implementation does not touch record serialization, backend traits/capabilities, cache keys, built-in record fields, or stored bytes.
- Parsing happens before token generation and before the system clock sample in `generate_current`.
- Expiry uses `step - (unix_seconds % step)`, yielding the full period exactly at a boundary and one second at the end of a period.
- URI parsing first validates scheme, host, and non-empty account label, then delegates parameter validation to `totp-rs`.
- Seed material is held in a zeroizing temporary and token output is zeroizing; generated codes and secrets are absent from error strings.

## Concerns

- Existing repository test warnings remain unrelated to this task (a duplicated test attribute and several dead-code warnings under the existing test configuration).
- No concerns specific to the RFC 6238 engine were found.

## Fix round 1: dependency error redaction

### Finding addressed

`parse_uri` previously forwarded `totp-rs`'s `TotpError::to_string()` output. Several dependency errors include the offending parameter value, so a malformed URI parameter could echo seed-like material or the full URI. The fix maps every dependency error variant to a fixed, category-safe message (`algorithm`, `digits`, `period`, `secret`, `account label`, `issuer`, URI syntax, or generic URI parameters) without formatting any dependency-provided fields.

### RED evidence

Added `totp::tests::dependency_parser_errors_never_echo_parameter_values` before changing production code. It covers malformed period, malformed digits, invalid issuer, invalid account label, issuer percent-decoding, and account-label percent-decoding cases. The focused regression command failed as expected against the old implementation:

```text
cargo test totp::tests::dependency_parser_errors_never_echo_parameter_values --lib
test totp::tests::dependency_parser_errors_never_echo_parameter_values ... FAILED
malformed period: Configuration error: invalid TOTP material: Could not parse step "MZXW6YTBOI======SENTINEL" as a number
test result: FAILED. 0 passed; 1 failed
```

### GREEN evidence

Changed `src/totp.rs` to match `totp_rs::TotpError` and return fixed safe category messages. The same focused regression command then passed:

```text
cargo test totp::tests::dependency_parser_errors_never_echo_parameter_values --lib
test totp::tests::dependency_parser_errors_never_echo_parameter_values ... ok
test result: ok. 1 passed; 0 failed
```

After `cargo fmt`, the complete domain suite passed:

```text
cargo test totp::tests --lib
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 1182 filtered out
```

The amended library suite also passed:

```text
cargo test --lib
test result: ok. 1189 passed; 0 failed; 1 ignored; 0 measured
```

### Fix-round files and self-review

- Changed `src/totp.rs` (safe `TotpError` category mapping and the six-case regression test).
- Appended this fix-round section to `task-1-report.md`.
- `cargo fmt` completed successfully; no dependency error field is interpolated by the new mapper, including the wildcard arm for future non-exhaustive variants.
- The existing unrelated duplicate-attribute and dead-code warnings remain; no new warnings or test failures were introduced.
