# Cross-cloud migration with `xv migrate`

`xv migrate` copies secrets from one backend to another while preserving metadata. Phase 3 (v0.10) hardens this command for cross-cloud use as a marquee feature.

## Quick reference

```bash
# Azure -> AWS
xv migrate --from azure --to aws --vault myproj-kv

# AWS -> Azure
xv migrate --from aws --to azure --vault myproj-kv

# Different source/target vault names
xv migrate --from azure:dev-kv --to aws:prod-sm
xv migrate --from aws:prod-sm --to local:default

# Filter
xv migrate --from azure --to aws --vault myproj-kv --filter "db-*"

# Dry run
xv migrate --from azure --to aws --vault myproj-kv --dry-run

# Conflict modes
xv migrate --from azure --to aws --vault myproj-kv --on-conflict skip      # default
xv migrate --from azure --to aws --vault myproj-kv --on-conflict replace
xv migrate --from azure --to aws --vault myproj-kv --on-conflict fail

# Force replace (ignore migration tags)
xv migrate --from azure --to aws --vault myproj-kv --force-replace

# Tune concurrency
xv migrate --from azure --to aws --vault myproj-kv --concurrency 4
```

## Prerequisites

### Azure source / target

You need:
- A logged-in Azure session (`az login` or env-based credentials).
- `Key Vault Secrets User` role on the source vault (for read).
- `Key Vault Secrets Officer` role on the target vault (for write).

### AWS source / target

You need:
- AWS credentials configured (env, profile, SSO, or instance role).
- `secretsmanager:ListSecrets`, `secretsmanager:GetSecretValue`, `secretsmanager:DescribeSecret` on the source.
- `secretsmanager:CreateSecret`, `secretsmanager:PutSecretValue`, `secretsmanager:UpdateSecret`, `secretsmanager:TagResource`, `secretsmanager:UntagResource` on the target.

Minimal AWS IAM policy for the target:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "secretsmanager:CreateSecret",
        "secretsmanager:PutSecretValue",
        "secretsmanager:UpdateSecret",
        "secretsmanager:TagResource",
        "secretsmanager:UntagResource",
        "secretsmanager:DescribeSecret",
        "secretsmanager:ListSecrets"
      ],
      "Resource": "*"
    }
  ]
}
```

### Local source / target

No prerequisites beyond a configured local backend (`xv init --backend local`).

## How it works

### Addressing

`--from` and `--to` accept either a backend name (`azure`, `aws`, `local`) or a
per-side endpoint in `backend:vault` form. The endpoint form is useful when the
source and target vault/store names differ:

```bash
xv migrate --from azure:dev-kv --to aws:prod-sm
```

When the endpoint omits a vault, `xv` uses the command's `--vault` value or the
backend's configured default vault for that side. Backend aliases accepted by the
parser are `az`/`keyvault`, `age`/`file`, and `asm`/`secretsmanager`, but docs
use canonical names for clarity.

Pre-flight: `xv migrate` enumerates source and target secrets, computes a diff, and prints a summary. In dry-run mode, the run stops here.

Per-secret transfer: each secret is `get_secret`'d from source (with value) and `set_secret`'d on target. Bounded by `--concurrency` (default 8). Throttling errors trigger exponential backoff with jitter.

Idempotency: each migrated secret carries `xv:migrated_from=<source>:<vault>:<source-version-id>` and `xv:migrated_at=<timestamp>` tags on the target. Re-running `xv migrate` with `--on-conflict skip` (the default) detects these and skips entries where the source version matches.

Interruption safety: a run interrupted with Ctrl-C leaves no partial-state damage. Each transfer is atomic. Re-run to resume.

## Metadata mapping

| Source field | Azure → AWS | AWS → Azure |
|---|---|---|
| `groups` | tag `xv:groups` (comma-joined) | tag `groups` |
| `note` | AWS `Description` field | tag `note` |
| `folder` | tag `xv:folder` | tag `folder` |
| `expiry` | tag `xv:expires_at` | native attribute |
| `original_name` | tag `xv:original_name` | tag `original_name` |
| `created_by` | tag `xv:created_by` | tag `created_by` |
| `content_type` | tag `xv:content_type` | native attribute |
| version history | current value only | current value only |

## Performance

A 100-secret migration completes in <60 s on a warm credential cache and `--concurrency 8`, assuming no throttling. For larger migrations, monitor AWS CloudWatch / Azure Monitor for rate-limit events and lower `--concurrency` if needed.

## Troubleshooting

- **`Error: vault 'X' not found`**: target vault doesn't exist. Run `xv vault create X --backend <target>` first, or rely on auto-create (currently only for the source's default vault).
- **`Error: ThrottlingException`**: AWS rate-limit hit. Lower `--concurrency`. Backoff is automatic.
- **`Error: AccessDeniedException`**: missing IAM permissions on AWS, or missing role on Azure. See "Prerequisites".
- **Migrate tags on the target make rollback messy**: pass `--force-replace` to overwrite without honoring migration tags.

## Limitations (Phase 3)

- Only the current value is transferred. Full version history transfer is deferred (`--with-history` not yet implemented).
- IAM resource policies on AWS source/target secrets are not preserved.
- Cross-region AWS migrations require running `xv migrate` once per source/target region pair, using `[named_backends.*]` config.
