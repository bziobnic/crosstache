# Secret Groups

Groups let you organize related secrets and filter them when listing. A secret can belong to multiple groups.

## Assigning Groups

Groups are assigned using the `--group` flag on the `update` command:

```bash
# Add a secret to a group
xv update "my-secret" --group "authentication"

# Add to multiple groups at once
xv update "shared-secret" --group "auth" --group "payments"

# Replace all groups
xv update "my-secret" --group "only-group" --replace-groups

# Remove from all groups
xv update "my-secret" --replace-groups
```

> **Note:** `xv set` does not accept `--group`. Create the secret first, then assign groups with `xv update`.

## Listing and Filtering

```bash
# List all secrets (shows group column)
xv list

# Filter by group
xv list --group "production"
xv list --group "myapp/database"
```

## Organizing with Groups

### By application
```bash
xv update "db-host" --group "webapp/database"
xv update "db-pass" --group "webapp/database"
xv update "api-key" --group "webapp/external"
```

### By environment
```bash
xv update "db-host" --group "production"
xv update "staging-db-host" --group "staging"
```

### Cross-cutting concerns
```bash
# A secret used by multiple services
xv update "encryption-key" --group "encryption" --group "auth-service" --group "payment-service"

# Find all encryption secrets
xv list --group "encryption"
```

## Merge vs Replace

By default, adding groups **merges** with existing groups:

```bash
xv update "key" --group "groupA"    # Groups: groupA
xv update "key" --group "groupB"    # Groups: groupA, groupB (merged)
```

Use `--replace-groups` to **replace** all existing groups:

```bash
xv update "key" --group "groupC" --replace-groups   # Groups: groupC (replaced)
```

## Tips

- Use consistent naming conventions (e.g., `app/component` hierarchy)
- Design groups with team access patterns in mind
- Combine with `--folder` for hierarchical organization and `--group` for cross-cutting tags
