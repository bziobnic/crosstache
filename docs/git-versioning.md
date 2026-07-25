# `xv git` — git-native versioning and the local audit trail

Two opt-in features for the local age-encrypted backend:

- **`[local].git`** — the store becomes a real git repository, committed after
  every mutation. `git log`, `git diff`, `git bisect`, and `git push` all work on
  the actual history.
- **`[local].audit`** — an append-only, hash-chained audit log under the store.

They compose: with both on, the audit log is versioned and pushed alongside the
secrets, which is what gives the chain an off-box copy.

- [Git-native versioning](#git-native-versioning)
- [Key material is never committed](#key-material-is-never-committed)
- [Syncing across machines](#syncing-across-machines)
- [The local audit trail](#the-local-audit-trail)
- [What the hash chain does and does not prove](#what-the-hash-chain-does-and-does-not-prove)
- [Why local only](#why-local-only)

---

## Git-native versioning

```toml
# ~/.config/xv/xv.conf
backend = "local"

[local]
store_path = "~/.xv/store"
key_file   = "~/.xv/key.txt"    # outside the store — see below
git        = true
```

```bash
xv git init      # create the repository (also refreshes the managed .gitignore)
```

`xv git init` works before `git = true` is set, so you are not stuck needing the
flag to make the repo and the repo to use the flag. Until the flag is on, no
commits happen and `init` says so.

From then on, every mutation commits:

```console
$ xv set DB_PASSWORD --value hunter2
[ok] Successfully set secret 'DB_PASSWORD'

$ xv rotate DB_PASSWORD --force
[ok] Successfully rotated secret 'DB_PASSWORD'

$ xv git log
 Commit   Date                       Subject
 4f2a1c9  2026-07-24T22:14:03-04:00  set DB_PASSWORD
 a91e3f0  2026-07-24T22:13:41-04:00  set DB_PASSWORD
```

| Command | Description |
|---------|-------------|
| `xv git init` | Create the repository and write the managed `.gitignore`. Idempotent. |
| `xv git log [SECRET]` | History, newest first. A secret name filters to commits touching it. `--limit N` (0 = all). Honors `--format json\|yaml\|csv`. |
| `xv git status` | Uncommitted changes in the store. |
| `xv git diff [REV]` | Which files a commit changed. **Names only, never contents.** |
| `xv git push [REMOTE] [--branch B]` | Push the store. |
| `xv git pull [REMOTE] [--branch B]` | Pull, fast-forward only. |

Commit subjects mirror the operation: `set NAME`, `update NAME`, `delete NAME`,
`restore NAME`, `purge NAME`, `rollback NAME to vN`.

`xv git diff` deliberately never prints file contents. For age ciphertext a
textual diff is noise; for plaintext metadata it could surface note and tag values
into terminal scrollback and CI logs.

This is **additive**. The backend's own `.versions/` archive still drives
`xv history` and `xv rollback` exactly as before, on every backend.

### Relationship to `xv purge`

`xv purge` removes a secret from the working tree, but earlier commits still
contain its ciphertext. `xv git log` therefore remains an honest record that the
secret existed. Purge is not a history rewrite — and once a store has been
pushed, it cannot be.

---

## Key material is never committed

The age identity is what decrypts every secret in the store. Git history is
effectively permanent, so a key committed once is very hard to expunge.

Two independent protections:

1. A **managed `.gitignore`** block listing the identity and recipients files
   plus lock files. Your own `.gitignore` lines outside that block are preserved.
2. A **pre-commit check in `xv` itself**: before each commit, the staged path list
   is inspected and the commit is **refused** if key material appears. This is the
   actual gate — `.gitignore` is a convenience that `git add -f` can defeat.

```console
$ xv set A --value v
error[xv-config-invalid]: refusing to commit the local store: age key material
('key.txt') is staged. Git history is effectively permanent, so committing a
private key would expose every secret in the store to anyone who ever sees the
repo. Move key_file outside the store (the default is ~/.xv/key.txt, with the
store at ~/.xv/store) or add it to .gitignore, then retry.
```

The shipped defaults already keep the key outside the store. You only reach this
error by pointing `key_file` inside `store_path`.

**Even so: a pushed store is ciphertext, but it is ciphertext of all your
secrets.** Its safety rests entirely on the age key and on age's cryptography. Use
a private remote.

---

## Syncing across machines

```bash
git -C ~/.xv/store remote add origin git@github.com:me/my-secret-store.git
xv git push origin --branch main

# On another machine — clone the store, then copy the key over separately.
git clone git@github.com:me/my-secret-store.git ~/.xv/store
# ... transfer ~/.xv/key.txt out of band (never through the repo) ...
xv ls
```

`xv git pull` is **fast-forward only**. Two machines that both wrote secrets have
divergent histories, and a merge conflict inside age ciphertext is not something
anyone can resolve — better to refuse than to produce a tree with conflict markers
inside encrypted files. Reconcile manually with git if it happens.

For a lock-free alternative on Azure/local, `xv file sync` and `xv migrate` move
secrets without involving git at all.

---

## The local audit trail

```toml
[local]
audit = true
```

Records land in `<store>/vaults/<vault>/.audit/log.jsonl`, one JSON object per
line, and surface through the normal command:

```console
$ xv audit --vault default
 Timestamp            Operation        Resource      Caller  Status
 2026-07-24 22:14:03  PutSecretValue   DB_PASSWORD   scott   Succeeded
 2026-07-24 22:14:07  GetSecretValue   DB_PASSWORD   scott   Succeeded
```

Recorded operations: `PutSecretValue`, `GetSecretValue`, `UpdateSecret`,
`DeleteSecret`, `RestoreSecret`, `PurgeSecret`, `RollbackSecret`, and
`ListSecrets` (as vault-wide `*`).

Only reads that actually **decrypt** a value count as `GetSecretValue`.
Metadata-only reads — the ones backing `xv ls` and existence checks — are not
logged, so real value access is not buried in noise.

**Fail-closed:** when auditing is on and an append fails, the operation that
triggered it fails too. A silently missing record would make the log's own
completeness unprovable.

### Failed attempts are recorded too

The log answers "what was attempted", not only "what succeeded":

```console
$ xv get GHOST --raw
error[xv-secret-not-found]: Secret not found: GHOST

$ xv audit --vault default
 Timestamp            Operation       Resource     Caller  Status
 2026-07-24 22:14:03  PutSecretValue  DB_PASSWORD  scott   Succeeded
 2026-07-24 22:15:40  GetSecretValue  GHOST        scott   NotFound
 2026-07-24 22:16:02  GetSecretValue  DB_PASSWORD  scott   DecryptionFailed
```

Status tokens come from a **closed set**, derived from the error's type rather
than its message, so no future error string can widen what the log records:

| Status | Meaning |
|--------|---------|
| `Succeeded` | The operation completed. |
| `NotFound` / `VaultNotFound` | The named secret or vault does not exist. |
| `DecryptionFailed` | **The one to watch.** A caller reached a secret's ciphertext and could not open it — the wrong age identity, or altered/truncated material. |
| `AccessDenied` / `AuthenticationFailed` | Permission or credential failure. |
| `InvalidArgument` / `Conflict` / `Unsupported` | Rejected request. |
| `NetworkError` / `RateLimited` | Transport-level failure. |
| `RenameIncomplete` | A rename created the new secret but left the original. |
| `InternalError` | Anything else. |

Secret **values** never appear in a record. Secret **names** do, in
`resource_name` — identically to the successful case, and the reason auditing a
failed read is worth anything at all.

Metadata-only probes are excluded from failure logging too, matching the success
path: a `NotFound` from an existence check is normal listing traffic, not an
access attempt worth recording.

Filter to just the interesting ones:

```bash
xv audit --operation DecryptionFailed
xv audit --operation NotFound
```

### Verifying the chain

Every record carries `mac = HMAC-SHA256(chain_key, prev_mac || record)`, with the
key HKDF-derived from the age identity.

```console
$ xv audit --verify
[ok] Audit chain intact: 12 record(s) verified for vault 'default'.

$ xv audit --verify      # after an edit
error: Audit chain BROKEN at record 7 for vault 'default': mac mismatch (record
contents were modified after they were written)
  6 record(s) before the break verified successfully.
```

Exits non-zero on a break, so a cron job or CI step can watch for tampering.

---

## What the hash chain does and does not prove

**Does:**

- Editing, reordering, or removing any record breaks the chain and is reported,
  with the position of the first break.
- Someone without the age identity cannot forge or repair a record — they cannot
  compute the MAC.

**Does not:**

- Someone *with* the age identity — anyone who can already decrypt every secret
  in the store — can rewrite the whole log from any point and re-chain it.
- Anyone who can write the file can truncate the tail, key or no key.

So it answers "has my history been altered since I last looked?" It does not
answer "can a compromised operator hide their tracks?" For that you need a sink
the attacker cannot reach. Pushing the store to a remote gets you most of the way,
because the remote keeps copies of the log that a local attacker cannot edit:

```bash
xv git push origin --branch main   # off-box copies of .audit/log.jsonl
```

This is a genuine difference in kind from Azure Activity Log and AWS CloudTrail,
which are produced and held by the platform rather than by the caller. That
asymmetry is also why `xv audit --verify` is local-only: there is no client-side
chain for `xv` to verify on a cloud backend, and none is needed.

The log grows by one line per audited operation. It is committed with the store
when git versioning is on, so it is covered by the same history.

---

## Why local only

`xv git` versions the **local** store. On Azure and AWS the secret values live in
Key Vault / Secrets Manager, so git-native versioning there would mean mirroring
every secret version into a git repository — a second, effectively permanent copy
of every secret, in a format designed never to forget anything. For a secrets
manager that is a poor trade, so it is deliberately not offered.

Cloud backends keep their native version history instead:

```bash
xv history DB_PASSWORD             # versions on any backend
xv rollback DB_PASSWORD --version v3
```
