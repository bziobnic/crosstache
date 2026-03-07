# UX Audit Findings - 2026-03-06

Full codebase review identifying UX improvement opportunities across 7 themes.

## Theme A: Output Consistency & Confirmation Standardization
- Mixed emoji styles: `checkmark` vs `check` vs plain text for success messages
- Mixed confirmation mechanisms: `InteractivePrompt` vs raw `rpassword` parsing (8 instances)
- No standard prefix pattern across commands
- Inconsistent emoji between `format.rs` (`check`, `warning`, `x`, `info`) and `interactive.rs` (different emoji variants)

## Theme B: Progress Indicators & Operation Feedback
- No spinners for long-running ops (vault creation, bulk uploads, RBAC resolution)
- Bulk operations lack per-item progress ("Setting 3/10...")
- No result counts after list/filter operations
- Container size calculation has no feedback for large containers
- Batch group delete has good feedback pattern but others don't follow it

## Theme C: Error Messages & Actionable Guidance
- Auth errors pass through raw Azure SDK text
- Generic "No data to display" doesn't distinguish empty vs permission denied vs filter miss
- HTTP 403 doesn't explain why or suggest RBAC role
- SSL/TLS error detection uses fragile string matching
- DNS extraction fallback returns "unknown-vault"
- Missing 429 (rate limiting) handling
- Typo: "Unimnplemented" at commands.rs:5943

## Theme D: Post-Operation Hints & Discoverability
- Delete doesn't suggest `xv restore` for undo
- Soft-delete doesn't explain recovery process or timeline
- Secret creation doesn't suggest `xv get` to verify
- Name sanitization warning doesn't explain how to work with hashed names
- No mapping lookup command for sanitized names
- `rotate` command (line 4819) is the one good example to follow

## Theme E: Scripting/Automation Support
- JSON/YAML/CSV output formats return placeholder strings (completely broken)
- Emoji in stderr breaks piped output
- No TTY detection before using emoji/color
- Template output not implemented despite being exposed as option

## Theme F: Silent Failure Fixes
- Blob metadata/tags silently dropped (SDK limitation logged but not shown)
- Tag limit (15) not validated, operations may silently lose tags
- `file sync` returns Ok(()) but prints "not implemented" to stderr
- Empty file uploads accepted without warning
- `NameMappingStats` struct exists but is never used

## Theme G: Configuration & Setup UX
- Setup step numbering breaks when optional steps skipped
- Config validation errors don't say where to set missing values
- Running `xv init` twice gives silent success with no changes
- Blob storage setup interrupts Key Vault setup flow
- Config file parse errors show raw serde errors instead of guidance
- Hardcoded 80-char box drawing truncates long values (GUIDs)
- `max_length(20)` on subscription select silently hides items beyond 20
