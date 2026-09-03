# `xv upgrade` — self-update

Replace the running `xv` binary with the latest GitHub Release. The command
does not contact a vault or require backend credentials. Normal startup still
loads the global configuration before dispatch, so global-config errors can
block it. A successfully selected project profile with an invalid backend name
also blocks; project discovery and environment-selection errors are otherwise
ignored for this command.

```bash
xv upgrade              # prompt, then download / verify / replace
xv upgrade --force      # skip the confirmation prompt
xv upgrade --check      # report whether an update exists; install nothing
```

## What it installs

`xv upgrade` fetches the **latest stable** release (`/releases/latest`), not
a draft, prerelease, or a pinned older tag. Asset names match the install
archives:

| Platform | Archive |
|----------|---------|
| macOS Apple Silicon | `xv-macos-apple-silicon.tar.gz` |
| macOS Intel | `xv-macos-intel.tar.gz` |
| Linux x64 | `xv-linux-x64.tar.gz` |
| Windows x64 | `xv-windows-x64.zip` |

Anything else (`linux/aarch64`, …) is refused with a pointer to the Releases
page. Archives larger than 50 MiB are refused. Download timeout is 5 minutes.

Release binaries are built with `--features tui,ui,aws`. A binary built from
source with a different feature set is replaced by that release feature set.

## Verification (fail-closed)

Order of checks, all required:

1. **Minisign signature** (`.minisig`) against the public key embedded in the
   binary (`RWRuXFh34rB613dgsXyAMmtKvYK0SFwxq4i44dhGFXVTrhAQ7hJXf6Ym`). Missing
   signature → refuse. A `.sha256` from the same release channel is **not**
   an authenticity control.
2. **Trusted comment** on the signature must be `crosstache vX.Y.Z` for the
   release tag, so a valid signature cannot be replayed onto a different tag.
3. **SHA-256** of the archive against the `.sha256` asset.
4. **`--version` of the extracted binary** must report that same version
   before the running file is replaced.

Releases since v0.11.0 are signed in CI. An unsigned latest release is treated
as tampering or a broken publish, not as a warning.

## `--check` exit status

For a successful GitHub check, `--check` exits `0` whether xv is already current
or an update is available. Availability is in the message (`Already up to date`
vs `Update available: vA → vB`), so this chain works:

```bash
xv upgrade --check && xv upgrade --force
```

An update-available result is not represented by a non-zero status. Global-config,
network, GitHub API, and release-metadata/version parsing failures remain
nonzero. `--check` does not download or authenticate release assets; signature,
checksum, and extracted-binary verification occur only during installation.

## Replace semantics

- On Unix, resolves symlinks, writes a sibling temporary executable with mode
  `0700` and `O_NOFOLLOW`, then atomically renames it over the current path; the
  installed mode is `0755`.
- On Windows, closes the temporary file before its `--version` check, then moves
  the current executable to `.old` and the verified replacement into place.
  This avoids `ERROR_SHARING_VIOLATION` but is not the Unix atomic-rename path.
- If the path contains `.cargo/bin`, a warning notes that a later
  `cargo install` can overwrite the upgraded binary.
- Permission denied on the write/rename asks for elevated privileges.

## Pitfalls

**GitHub API rate limit.** Unauthenticated requests cap at 60/hour. Set
`GITHUB_TOKEN` to raise that. A 403 is reported as a rate-limit error, not a
missing release.

**`cargo install` vs release upgrades.** Upgrading a cargo-installed binary
works once; the next `cargo install --path .` puts the source build back.
Prefer the install script or release archive if `xv upgrade` should remain the
ongoing path.

**Not a channel pin.** There is no `--version v0.37.0`. To install a specific
older release, download the archive from GitHub and verify it with minisign as
in the README “Release Verification” section.

**Network and verification failures.** DNS, TLS, GitHub, signature, checksum,
or extracted-version failures leave the running binary unchanged. The verified
temporary executable is created only after the archive checks pass.
