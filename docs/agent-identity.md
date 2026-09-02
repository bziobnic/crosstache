# Agent identity and policy

Agent policy is opt-in. It identifies an automated caller from existing IAM
context, evaluates each secret operation, and records the decision. `xv` does
not issue credentials or create agent identities.

## Enforcement modes

| Configuration and identity | Behavior |
|---|---|
| No `[agent]` block, or `enforce = false` | Unenforced. Identity is not resolved, no policy wrapper or decision log is created, and existing audit records remain v1 and byte-compatible. |
| `[agent]` with `enforce = true`, identity resolves | Enforced. Every `SecretBackend` operation is checked and every allow or deny is recorded. |
| `[agent]` with `enforce = true`, identity does not resolve | Fail closed. The backend is not returned; the error lists each resolver and the environment it requires. |

Only secret operations are enforced in this release. Vault, file, audit-query,
and backend health-check operations delegate unchanged.

## Identity resolvers

Resolvers run once per process, in this order; the first successful resolver
wins.

| Source | Detection and identity | Verified | Status |
|---|---|---:|---|
| `github-oidc` | `ACTIONS_ID_TOKEN_REQUEST_URL` plus `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. The existing Actions OIDC transport requests a token, then `repository`, `job_workflow_ref` (or `workflow`), and `ref` claims form `github:<repository>:<workflow>@<ref>`. | yes | supported |
| `entra-workload-identity` | `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, and `AZURE_FEDERATED_TOKEN_FILE`. `xv` uses an explicit `WorkloadIdentityCredential` exchange and requires the returned access token's tenant/client claims to match before forming `entra:<tenant>:<client>`. | yes | supported |
| `aws-role` | Would require STS `GetCallerIdentity`. | n/a | unsupported: no STS client is compiled in |
| `spiffe` | Would require a SPIFFE Workload API client. | n/a | unsupported: no Workload API client is compiled in |
| `env-assertion` | `XV_AGENT_ID` supplies the id verbatim. | **no** | supported as an explicit assertion |

The GitHub runtime URL must use HTTPS, contain no user information, and name
GitHub's documented `pipelines.actions.githubusercontent.com` token host before
the runtime bearer is sent. The returned token must carry GitHub's issuer, the
requested Azure token-exchange audience, a future expiry, and non-empty
repository/workflow/ref claims. `xv` does not separately verify that token's
JWT signature because it was obtained directly from the authenticated,
allowlisted runtime endpoint.

Entra environment variables alone are not identity. `xv` exchanges the
projected assertion with a documented Azure public or sovereign authority,
then validates expiry and matching tenant/client claims in the returned token.
Custom authority hosts are rejected. This exchange is authentication, but it
is not workload attestation.

All successful resolvers also read optional context:

- `XV_AGENT_SESSION`: opaque task/session correlation id.
- `XV_AGENT_PRINCIPAL`: the human or system principal the agent acts for.
- `XV_AGENT_DELEGATION`: comma-separated outermost-to-innermost delegation chain.
- `XV_AGENT_PURPOSE`: free text recorded only for audit. It is never consulted
  by authorization because prompt-injected tool or web content can poison an
  agent's stated justification.

Identity and optional context fields have byte/count limits and reject control
characters rather than truncating. Purpose is durable audit context: it must
not contain passwords, tokens, secret values, or other confidential data.

## Policy reference

```toml
[agent]
enforce = true
allow_unverified_identities = false
default_decision = "deny"

[[agent.policy]]
name = "ci-deploy-reads"
identity = "github:bziobnic/crosstache:*"
identity_source = "github-oidc"
workspace = "prod"
secrets = ["deploy/*", "registry/*"]
operations = ["get", "list"]
max_duration = "10m"
raw_disclosure = true
approval_tier = "none"
```

`default_decision` is `deny` and no other value is accepted. Rules are ordered;
the first rule whose identity glob, identity source, workspace, secret glob,
and operation all match decides the request. An empty identity, source, or
workspace is a wildcard. Empty `secrets` or `operations` matches nothing.

Operations are `get`, `list`, `set`, `update`, `delete`, `rename`, `rollback`,
`restore`, `purge`, and `rotate`. A list first requires an applicable
identity/source/workspace rule with the `list` operation. The backend result is
then filtered by applying that rule set to every actual secret name; a scoped
rule such as `deploy/*` therefore returns only matching names. The decision log
always records the vault-level scope decision and records up to 256 item-level
filter decisions per request. This applies equally to live-secret and
soft-deleted-secret listings; deleted names outside the authorized scope are
not returned.

Plaintext-bearing reads (`get`/version/snapshot with values and backup) require
`raw_disclosure = true` on the matching rule. Metadata-only reads do not. An
unverified `XV_AGENT_ID` additionally cannot receive plaintext unless
`allow_unverified_identities = true`. This is an explicit acceptance of an
environment assertion, not a promotion to verified identity.

Secret globs treat `/` as a folder separator: `deploy/*` matches
`deploy/key`, not `deploy/nested/admin`. Identity globs intentionally retain
cross-separator matching so `github:owner/repo:*` covers workflow paths.

Configuration loading rejects malformed identity or secret globs, unknown
identity sources or operations, unsupported default decisions, invalid
approval tiers, and invalid durations. A policy version is a truncated SHA-256
of the fully defaulted, canonically serialized `[agent]` block.

## Decision and audit records

Every allow and deny is appended to the HMAC-chained decision log at
`$XDG_STATE_HOME/xv/agent-decisions.jsonl` (falling back to
`~/.local/state/xv/agent-decisions.jsonl`). Its private chain key is stored next
to it as `agent-decisions.age-key`. Records include identity, source,
verification status, principal, session, operation, resource, decision,
matched rule or denial reason, policy version, and audit-only purpose.
The ordered delegation chain is recorded and MAC-bound element by element.

When an allowed operation reaches an audit-enabled local backend, its normal
audit row is v2 and MAC-binds the agent fields. Legacy and unenforced rows stay
v1; one chain may contain v1 rows followed by v2 rows and still verifies. The
v2 domain/version tag is itself MAC-bound; unknown versions and v1 records that
carry unbound v2 attribution fields are rejected.

## Limitations

- There is no workload attestation and no mTLS channel in this CLI.
- `XV_AGENT_ID` is an unverified assertion any process can set.
- AWS role and SPIFFE resolution are unavailable in this build; environment
  strings are not treated as verified substitutes.
- `max_duration` is parsed, validated, and recorded in the policy version, but
  is not enforced until a broker supplies a session lifetime.
- `approval_tier` (`none`, `notify`, or `required`) is parsed, validated, and
  recorded in the policy version, but is inert until a human-in-the-loop
  channel exists. `required` does not currently pause or block a request.
- The local decision and audit chains are tamper-evident, not tamper-proof.
  Anyone holding the chain key can rewrite them, and a writer can truncate the
  tail. Use an off-host append-only sink for stronger evidence.
- Client-side caches are disabled while enforcement is active, so cached
  secret names can never bypass policy or decision logging.
- `restore_from_backup` is refused in enforced mode because the backend trait
  does not expose the destination secret name before restoration. It will stay
  fail-closed until that destination can be bound to policy.
- `xv local encrypt-metadata` and `xv local migrate` are refused before opening
  or scanning the store whenever enforcement is active, including with
  `--dry-run`. These maintenance paths operate directly on `LocalBackend` and
  cannot currently bind each discovered name or filesystem action to policy.
- All `xv git` maintenance subcommands are likewise refused before opening the
  repository while enforcement is active. Even read-only log/status/diff can
  enumerate secret names or metadata, while push/pull move whole-store history;
  none can yet be bound to per-secret policy.
- `xv init`, `xv backend add`, and `xv backend rm` are refused before
  inspecting or changing backend state while enforcement is active. Setup,
  configuration replacement, and whole-store purge cannot be represented as
  per-secret policy decisions. Read-only `xv backend ls` remains available.
