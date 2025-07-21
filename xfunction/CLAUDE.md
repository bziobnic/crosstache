# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is an Azure Function application that provides automated RBAC (Role-Based Access Control) management for Azure Key Vaults and associated Azure Storage Accounts. The function automatically assigns appropriate permissions to vault creators for both Key Vaults and related storage accounts through HTTP-triggered endpoints with JWT authentication.

## Key Architecture Details

### Hybrid Authentication Pattern
- **Service Authentication**: Uses ClientSecretCredential with App Registration for Azure SDK operations
- **User Authentication**: Validates JWT tokens from Authorization header to verify user identity
- **Creator Verification**: Validates requesting user created the vault via `CreatedByID` tag before role assignment

### Core Processing Flow
1. HTTP request with JWT token and vault information
2. Token validation and user identity extraction
3. Vault creator verification via Azure tags
4. Dual vault role assignment (Owner + Key Vault Administrator)
5. Storage account discovery using multiple strategies
6. Storage role assignment based on vault role success
7. Redundant role cleanup when Owner role is assigned

### Module Structure
- `function_app.py`: Main Azure Function entry point with `DirectVaultRbacProcessor` HTTP trigger
- `VaultRbacProcessor/vault_role_manager.py`: Core RBAC logic, principal resolution, and vault role management
- `StorageRoleManager/storage_role_manager.py`: Storage account discovery and storage role management
- **Vault Role IDs**: Owner (`8e3af657-a8ff-443c-a75c-2fe8c4bcb635`) and Key Vault Administrator (`00482a5a-887f-4fb3-b363-3b7fe8e74483`)
- **Storage Role IDs**: Storage Account Contributor (`17d1049b-9a84-46fb-8f53-869881c3d3ab`), Storage Blob Data Owner (`b7e6dc6d-f1e8-4753-8033-0f276bb0955c`), Storage Blob Data Contributor (`ba92f5b4-2d11-453d-a403-e96b0029c9fe`)

## Development Commands

### Local Development
```bash
# Install dependencies
pip install -r requirements.txt

# Run tests
python tests/run_tests.py

# Run specific test file
python -m pytest tests/test_direct_rbac_processor.py -v
python -m pytest tests/test_integration.py -v

# Start local Azure Functions runtime
func start
```

### Azure Functions Development
```bash
# Create local.settings.json for local development
func settings add AZURE_TENANT_ID "your-tenant-id"
func settings add AZURE_CLIENT_ID "your-client-id"
func settings add AZURE_CLIENT_SECRET "your-client-secret"

# Deploy to Azure
func azure functionapp publish <function-app-name>
```

### PowerShell Deployment (see scripts/README-POWERSHELL.md)
```powershell
# Full deployment pipeline
.\scripts\Deploy-AzureFunction.ps1 -ResourceGroupName "rg-name" -FunctionAppName "func-name"

# Test deployment
.\scripts\Test-VaultRbacFunction.ps1 -FunctionAppUrl "https://your-function.azurewebsites.net"
```

## Storage Integration Details

### Storage Account Discovery Strategy
The function automatically discovers storage accounts associated with a Key Vault using three strategies:

1. **Resource Group Strategy (Primary)**: Finds all storage accounts in the same resource group as the vault
2. **Tag-Based Association (Secondary)**: Looks for storage accounts with `AssociatedVault` tag matching the vault name
3. **Naming Convention (Fallback)**: Identifies storage accounts with names containing the vault name using patterns:
   - `{vault-name}storage`
   - `{vault-name}stor`
   - `stor{vault-name}`
   - `{vault-name}st`

### Storage Role Mapping
Storage roles are automatically assigned based on the vault role being granted:

| Vault Role | Storage Roles Assigned |
|------------|----------------------|
| **Owner** | Storage Account Contributor + Storage Blob Data Owner |
| **Key Vault Administrator** | Storage Blob Data Contributor |

### Storage Assignment Logic
- Storage role assignment only occurs after successful vault role assignment
- Same creator verification applies to storage operations
- Storage operations are logged separately with detailed results
- Function response includes storage account discovery and assignment results

## Critical Implementation Details

### Error Handling Patterns
- **Replication Delays**: Azure AD principal replication can take time; functions handle `PrincipalNotFound` errors gracefully
- **Network Errors**: Comprehensive error classification for Azure API failures with user-friendly messages
- **JWT Validation**: Proper token expiration and claims validation with security logging

### Security Considerations
- JWT tokens must include valid `sub` claim for user identification
- Creator verification prevents unauthorized role assignments
- Sensitive information filtered from logs (tokens, secrets)
- App Registration requires minimal permissions: `Application.Read.All`, `User.Read.All` for Graph API

### Azure-Specific Limitations
- **Principal Types**: Azure RBAC requires proper principal type identification (User/ServicePrincipal/Group)
- **Role Assignment Redundancy**: Owner role includes Key Vault Administrator permissions, requiring cleanup
- **Graph API Pagination**: Large user queries may require pagination handling

## Configuration Requirements

### Required Environment Variables (Azure App Settings)
```
AZURE_TENANT_ID=your-tenant-id
AZURE_CLIENT_ID=your-app-registration-client-id
AZURE_CLIENT_SECRET=your-app-registration-secret
```

### Host Configuration
- `host.json` configures Azure Functions runtime
- Event Grid integration configured but currently uses HTTP trigger pattern
- Application Insights integration for monitoring

## Testing Strategy

### Test Structure
- `test_direct_rbac_processor.py`: Unit tests for core RBAC functionality
- `test_integration.py`: End-to-end tests requiring live Azure credentials
- Tests validate JWT processing, role assignment logic, and Azure API integration

### Integration Testing Requirements
- Requires valid Azure credentials with permissions to create/modify RBAC assignments
- Tests create temporary resources for validation
- Use `python tests/run_tests.py` for proper test execution with exit codes

## Dependencies and Versions

### Core Azure SDKs
- `azure-identity>=1.12.0`: Authentication and credential management
- `azure-mgmt-authorization>=4.0.0`: RBAC management for both vaults and storage
- `azure-mgmt-keyvault>=10.1.0`: Key Vault operations
- `azure-mgmt-storage>=21.0.0`: Storage account discovery and management
- `msgraph-sdk>=1.0.0`: Microsoft Graph API for principal resolution

### Function Runtime
- Python 3.9+ with Azure Functions v4 runtime
- `azure-functions>=1.14.0` for HTTP triggers and bindings
- `PyJWT>=2.6.0` for token validation

## Deployment Pipeline

The project includes comprehensive PowerShell scripts for automated deployment:
- Resource group and Function App creation
- App Registration setup with proper permissions
- Automated testing and validation
- See `scripts/README-POWERSHELL.md` for detailed deployment instructions