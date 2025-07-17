# Azure Integration Guidelines

## Authentication Strategy

### DefaultAzureCredential Chain
crosstache uses Azure's `DefaultAzureCredential` which attempts authentication in this order:
1. Environment variables (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`)
2. Managed Identity (when running on Azure resources)
3. Azure CLI (`az login`)
4. Visual Studio Code Azure Account extension
5. Azure PowerShell

### Implementation Pattern
```rust
use azure_identity::DefaultAzureCredential;
use std::sync::Arc;

let credential = Arc::new(DefaultAzureCredential::default());
```

## API Integration Approach

### Hybrid Strategy
- **Authentication**: Azure SDK v0.20 for credential management
- **Operations**: Direct REST API calls to Azure Key Vault API v7.4
- **Rationale**: SDK limitations with tag support require REST API for full functionality

### REST API Patterns
- Use `reqwest` with `rustls-tls` for HTTP client
- Implement proper retry logic with exponential backoff
- Handle rate limiting and throttling gracefully
- Parse Azure error responses for user-friendly messages

### Key Vault API Endpoints
- Secrets: `https://{vault-name}.vault.azure.net/secrets/`
- Vaults: `https://management.azure.com/subscriptions/{subscription-id}/resourceGroups/{resource-group}/providers/Microsoft.KeyVault/vaults/`

## Error Handling

### Azure-Specific Errors
- Authentication failures (401, 403)
- Resource not found (404)
- Rate limiting (429)
- Network connectivity issues
- DNS resolution failures for vault URLs

### Error Mapping
Map Azure HTTP status codes to appropriate `crosstacheError` variants:
- 401/403 → `AuthenticationError` or `PermissionDenied`
- 404 → `VaultNotFound` or `SecretNotFound`
- 429 → Retry with backoff
- Network errors → `NetworkError` with specific details

## Resource Management

### Subscription and Resource Groups
- Support multiple subscriptions via configuration
- Default resource group from config or environment
- Validate resource existence before operations

### Vault Naming and URLs
- Validate vault names against Azure requirements
- Construct vault URLs: `https://{name}.vault.azure.net`
- Handle DNS resolution failures gracefully

### Tag Management
- Use tags for metadata storage (groups, original names, created_by)
- Preserve user-defined tags alongside system tags
- Handle tag limitations (15 tags max, 256 char values)