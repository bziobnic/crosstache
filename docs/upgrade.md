# `xv upgrade` — self-update

Replace the running `xv` binary with the latest GitHub Release. The command
talks only to GitHub; it does not need a vault, credentials, or a valid
`xv.conf`.

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

Release binaries are built with `--features tui,aws`. A binary you built from
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

`--check` always exits `0`. Availability is in the message (`Already up to
date` vs `Update available: vA → vB`), so this chain works:

```bash
xv upgrade --check && xv upgrade --force
```

Do not gate CI on a non-zero `--check` exit; that contract is not implemented.
(The clap `--help` text used to claim exit 1 for “update available”; the
runtime behavior is exit 0.)

## Replace semantics

- Resolves symlinks, then writes a sibling temp file and renames over the
  current executable. Unix temp files are `0700` + `O_NOFOLLOW`; the final
  mode is `0755`.
- If the path contains `.cargo/bin`, a warning notes that a later
  `cargo install` can overwrite the upgraded binary.
- Permission denied on the write/rename asks for elevated privileges.
- On Windows the write handle is closed before the `--version` spawn, so the
  new file is executable (avoids `ERROR_SHARING_VIOLATION`).

## Pitfalls

**GitHub API rate limit.** Unauthenticated requests cap at 60/hour. Set
`GITHUB_TOKEN` (any classic or fine-grained token with public-repo read) to
raise that. A 403 is reported as a rate-limit error, not a missing release.

**`cargo install` vs release upgrades.** Upgrading a cargo-installed binary
works once; the next `cargo install --path .` puts the source build back.
Prefer the install script / release archive if you want `xv upgrade` to be
the ongoing path.

**Not a channel pin.** There is no `--version v0.37.0`. To install a specific
older release, download the archive from GitHub and verify it with minisign
as in the README “Release Verification” section.

**Network-only failure modes.** DNS, TLS, or GitHub outages fail the command;
they do not touch the existing binary. A failed verification deletes the temp
file and leaves the running binary in place.
