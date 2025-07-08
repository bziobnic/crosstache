# Secret Groups in crosstache

This document describes the behavior and implementation of secret groups in crosstache, which provide a way to organize and manage related secrets.

> **Implementation Note**: Due to limitations in Azure SDK v0.20 with tag support, the Rust implementation uses direct REST API calls for secret operations to ensure proper tag persistence. This enables full group functionality while maintaining compatibility with Azure Key Vault.

## Overview

Secret groups in crosstache are a logical organization mechanism that allows you to:
- Organize related secrets together
- Filter secrets by group when listing
- Apply bulk operations to groups of secrets
- Maintain hierarchical naming conventions

Groups are implemented using Azure Key Vault tags, ensuring compatibility with the underlying Azure infrastructure while providing enhanced organizational capabilities.

## Group Assignment Methods

### 1. Explicit Group Assignment

You can explicitly assign secrets to groups using the `--group` flag:

```bash
# Assign to a single group
xv secret set "api-key" "abc123" --group "authentication"

# Assign to multiple groups
xv secret set "shared-secret" "xyz789" --group "auth" --group "payments"
```

### 2. Group Assignment During Updates

Groups can be modified when updating secrets:

```bash
# Add to additional groups (merge mode - default)
xv secret update "my-secret" --group "new-group"

# Replace all groups (replace mode)
xv secret update "my-secret" --group "only-group" --replace-groups
```

## Group Storage Implementation

Groups are stored in Azure Key Vault secret tags using the following scheme:

### Single Group
For secrets with one group:

```json
{
  "groups": "myapp/database",
  "original_name": "myapp/database/connection",
  "created_by": "crosstache"
}
```

### Multiple Groups
For secrets with multiple groups:

```json
{
  "groups": "auth,payments,shared",
  "original_name": "shared-api-key", 
  "created_by": "crosstache"
}
```

**Tag Structure:**

- `groups`: Comma-separated list of all groups (always present when groups are assigned)
- `original_name`: User-provided secret name before sanitization
- `created_by`: Always set to "crosstache" to identify managed secrets

## Group Resolution

When determining a secret's group for display purposes, crosstache uses the following logic:

1. **`groups` tag**: If present, the first group in the comma-separated list is used for display
2. **No group**: If no groups are assigned, the secret appears without a group (shown as "(No Group)" in grouped views)

## Listing and Filtering by Groups

### List All Secrets with Groups
```bash
# Shows all secrets with their group assignments
xv secret list
```

Output format:
```
| Name                    | Group         | Enabled | Updated     |
|-------------------------|---------------|---------|-------------|
| myapp/database/host     | myapp/database| true    | 2024-01-15  |
| myapp/database/password | myapp/database| true    | 2024-01-15  |
| api-key                 | authentication| true    | 2024-01-16  |
| shared-secret           | auth          | true    | 2024-01-17  |
| ungrouped-secret        |               | true    | 2024-01-18  |
```

### Filter by Specific Group
```bash
# Show only secrets in the "myapp/database" group
xv secret list --group "myapp/database"

# Show only secrets in the "auth" group  
xv secret list --group "auth"
```

### Group-based Organization View
```bash
# Display secrets organized by groups (grouped view)
xv secret list --group-by
```

Output format:
```
Group: myapp/database (2 secrets)
| Name                    | Enabled | Updated     |
|-------------------------|---------|-------------|
| myapp/database/host     | true    | 2024-01-15  |
| myapp/database/password | true    | 2024-01-15  |

Group: auth (1 secret)
| Name          | Enabled | Updated     |
|---------------|---------|-------------|
| shared-secret | true    | 2024-01-17  |

Group: (No Group) (1 secret)
| Name             | Enabled | Updated     |
|------------------|---------|-------------|
| ungrouped-secret | true    | 2024-01-18  |
```

## Group Management Operations

### Adding Groups to Existing Secrets
```bash
# Add a secret to additional groups (merge mode)
xv secret update "existing-secret" --group "new-group"

# Add to multiple groups at once
xv secret update "existing-secret" --group "group1" --group "group2"
```

### Replacing Groups
```bash
# Replace all existing groups with new ones
xv secret update "existing-secret" --group "only-group" --replace-groups

# Remove from all groups (leaves secret ungrouped)
xv secret update "existing-secret" --replace-groups
```

### Group Assignment with Secret Renaming
```bash
# Rename secret and change its group
xv secret update "old-name" --rename "new-name" --group "new-group" --replace-groups
```

## Best Practices

### 1. Consistent Naming Conventions
Use consistent separators and naming patterns:

```bash
# Good: Consistent hierarchy
myapp/database/host
myapp/database/port  
myapp/database/user
myapp/api/key
myapp/api/secret

# Avoid: Mixed separators
myapp/database-host
myapp.api_key
```

### 2. Logical Group Hierarchies
Design group hierarchies that reflect your application structure:

```bash
# Application-based grouping
webapp/database/*
webapp/redis/*
webapp/external-apis/*

# Environment-based grouping  
prod/webapp/*
staging/webapp/*
dev/webapp/*

# Service-based grouping
auth-service/*
payment-service/*
notification-service/*
```

### 3. Use Explicit Groups for Cross-cutting Concerns
```bash
# Secrets used by multiple applications
xv secret set "shared-encryption-key" "key123" --group "shared" --group "encryption"
xv secret set "monitoring-token" "token456" --group "shared" --group "monitoring"
```

### 4. Group-based Access Control Planning
Design groups with future access control in mind:

```bash
# Groups that align with team responsibilities
database-team/*      # Database administrators
security-team/*      # Security-sensitive secrets  
frontend-team/*      # Frontend application secrets
backend-team/*       # Backend service secrets
```

## Implementation Details

### Name Sanitization and Groups
1. Original names are preserved in the `original_name` tag
2. Azure Key Vault names are sanitized according to Azure requirements
3. Groups are only assigned explicitly via `--group` flags
4. Group information is preserved in tags even when names are heavily sanitized

### Azure SDK v0.20 and REST API Implementation
The Rust implementation uses a hybrid approach:
- **Authentication**: Uses Azure SDK v0.20 for credential management
- **Secret Operations**: Uses direct REST API calls to Azure Key Vault API v7.4
- **Tag Persistence**: REST API ensures all tags (groups, original_name, created_by) are properly stored
- **Group Filtering**: Client-side filtering after retrieving secrets with full metadata

### Tag Merge Strategies
When updating groups, two strategies are available:

**Merge Mode (Default):**

- Adds new groups to existing groups
- Preserves existing group assignments
- Merges new groups with the existing comma-separated list

**Replace Mode (`--replace-groups`):**

- Removes all existing group assignments
- Replaces with only the newly specified groups
- Useful for reorganizing or cleaning up group assignments

## Limitations and Considerations

### Azure Key Vault Tag Limitations
- Maximum 15 tags per secret (Azure limit)
- Tag keys and values must be ≤ 256 characters
- Group information stored in tags may not be immediately searchable via Azure native tools

### Performance Considerations
- Listing with group information requires fetching full secret metadata
- Large vaults may experience slower list operations due to tag retrieval
- Group filtering is performed client-side after retrieval

### Compatibility
- Groups are a crosstache enhancement and may not be visible in native Azure tools
- Other tools accessing the same Key Vault will see group information as standard tags
- Secret functionality remains fully compatible with standard Azure Key Vault operations

## Migration and Legacy Support

### Migrating Existing Secrets
Existing secrets without group information will:
1. Appear without any group assignment when listed
2. Can be manually assigned to groups using the `secret update --group` command
3. Have their original names preserved in tags for future operations

### Backward Compatibility
- All group operations are backward compatible with non-grouped secrets
- Secrets without group tags will appear in listings without any group
- Legacy operations continue to work without modification

## Error Handling

### Invalid Group Names
- Group names are validated for Azure tag compliance
- Invalid characters are sanitized or operations are rejected
- Users are notified of any group name modifications

### Tag Limit Exceeded
- If adding groups would exceed Azure's 15-tag limit, operation fails gracefully
- Users are informed of the limitation and current tag usage
- Suggestions provided for tag cleanup or reorganization

## Examples and Common Patterns

### Development Workflow
```bash
# Set up application secrets with automatic grouping
xv secret set "myapp/database/host" "localhost"
xv secret set "myapp/database/user" "dbuser"
xv secret set "myapp/api/external-key" "ext_abc123"

# List all database-related secrets
xv secret list --group "myapp/database"

# Update multiple secrets in a group (hypothetical bulk operation)
xv secret list --group "myapp/database" | xargs -I {} xv secret update {} --group "production"
```

### Cross-Service Secrets
```bash
# Secret used by multiple services
xv secret set "shared-encryption-key" "key123" \
  --group "encryption" \
  --group "auth-service" \
  --group "payment-service"

# Find all encryption-related secrets
xv secret list --group "encryption"
```

### Environment Migration
```bash
# Copy development secrets to staging with group change
xv secret get "myapp/database/host" --raw | \
  xv secret set "myapp/database/host" --stdin --group "staging"
```

This group system provides powerful organization capabilities while maintaining full compatibility with Azure Key Vault's underlying infrastructure.