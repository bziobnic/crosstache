# Cross-cloud migration with `xv migrate`

`xv migrate` copies secrets from one backend to another while preserving metadata. Phase 3 (v0.10) hardens this command for cross-cloud use as a marquee feature.

## Quick reference

```bash
# Azure -> AWS
xv migrate --from azure --to aws --vault myproj-kv

# AWS -> Azure
xv migrate --from aws --to azure --vault myproj-kv

# Filter
xv migrate --from azure --to aws --vault myproj-kv --filter "db-*"

# Dry run
xv migrate --from azure --to aws --vault myproj-kv --dry-run

# Include attachments (destination vault and V2 key ring must already exist)
xv migrate --from local:work --to local:stage --with-attachments \
  --to-key-id DESTINATION_ACTIVE_ID --dry-run
xv migrate --from local:work --to local:stage --with-attachments \
  --to-key-id DESTINATION_ACTIVE_ID --offline

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

Both endpoints must expose a readable attachment inventory. Configure the blob
container or S3 bucket even for a secret-only migration; missing storage
configuration or listing permission cannot establish that attachments are absent.
Local storage can inspect its persisted attachment metadata without `file-ops`.

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

Pre-flight: `xv migrate` enumerates source and target secrets, computes a diff, and checks attachment prefixes before transferring secrets. Without `--with-attachments`, attached sources or destinations are refused, including with `--force-replace`; an unavailable attachment inventory is an error. In dry-run mode, no secrets are written and `--offline` is not required.

Per-secret transfer: each unattached secret is `get_secret`'d from source (with value) and `set_secret`'d on target. Bounded by `--concurrency` (default 8). Throttling errors trigger exponential backoff with jitter. Attached entries are strictly preflighted, then executed sequentially through the recoverable transfer engine; they do not share that concurrency pool.

Idempotency: each migrated secret carries `xv:migrated_from=<source>:<vault>:<source-version-id>` and `xv:migrated_at=<timestamp>` tags on the target. Re-running `xv migrate` with `--on-conflict skip` (the default) detects these and skips entries where the source version matches. Attached entries preserve source metadata rather than adding those bookkeeping tags.

Interruption safety: an interrupted run may leave completed secret copies. Re-run to reconcile unattached entries using the migration tags. Attached entries are not an atomic batch — a later failure can leave earlier attached transfers complete. Resume a failed attached operation with its reported `xv transfer` intent and `--resume ID --offline`. This command does not provide a transaction across the whole batch.

To inspect or apply a single attachment transfer:

```bash
xv transfer NAME --from SOURCE --to DESTINATION --to-key-id ACTIVE_ID
xv transfer NAME --from SOURCE --to DESTINATION --to-key-id ACTIVE_ID --apply --offline
```

`SOURCE` and `DESTINATION` are vault names or workspace aliases; use `--new-name NEW`
for a rename and `--move` for source removal after verification. Preview reads
and authenticates attachment data but does not apply transfers or create a
recovery journal. See [`attachments.md`](attachments.md#rename-and-move).

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

- **`Error: vault 'X' not found`**: target vault doesn't exist. Run `xv vault create X --backend <target>` first, or rely on auto-create (currently only for the source's default vault). Attached migrations never auto-create the destination: create the vault, initialize its key ring, then pass `--to-key-id`.
- **`Error: ThrottlingException`**: AWS rate-limit hit. Lower `--concurrency`. Backoff is automatic.
- **`Error: AccessDeniedException`**: missing IAM permissions on AWS, or missing role on Azure. See "Prerequisites".
- **Attached source refused**: pass `--with-attachments` (and `--offline` unless `--dry-run`). Cross-vault attachments also need `--to-key-id` for a healthy destination V2 ring.
- **Azure destination / AWS or Azure source move refused**: those routes cannot provide atomic create or conditional delete. Copy from Azure/AWS to Local or AWS when snapshots and exact ownership verify. See [`attachments.md`](attachments.md#rename-and-move).
- **Migrate tags on the target make rollback messy**: pass `--force-replace` to overwrite without honoring migration tags.

## Limitations (Phase 3)

- Only the current value is transferred. Full version history transfer is deferred (`--with-history` not yet implemented).
- IAM resource policies on AWS source/target secrets are not preserved.
- Cross-region AWS migrations require running `xv migrate` once per source/target region pair, using `[named_backends.*]` config.
- **Attachments migrate only with `--with-attachments`.** Destination vaults must already exist with a healthy initialized V2 key ring (`xv attachment-key initialize --apply --offline`, then `--to-key-id`). Ordinary `xv migrate` still auto-creates a missing target vault for unattached secrets, but attached destinations are never created or keyed implicitly. Custody records remain excluded and an existing destination attachment key is preserved (even under `--force-replace`). Azure destinations and AWS/Azure source moves are refused. Source ciphertext larger than 256 MiB is rejected at preflight. See [`attachments.md`](attachments.md#migration).
