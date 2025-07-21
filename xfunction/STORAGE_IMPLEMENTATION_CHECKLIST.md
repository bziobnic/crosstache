# Azure Storage Permissions Implementation Checklist

## Phase 1: Architecture Analysis & Design
- [x] **1.1** Review current Key Vault role mapping:
  - Owner role (`8e3af657-a8ff-443c-a75c-2fe8c4bcb635`)
  - Key Vault Administrator role (`00482a5a-887f-4fb3-b363-3b7fe8e74483`)
- [x] **1.2** Research Azure Storage built-in roles for equivalent permissions:
  - Storage Account Contributor (`17d1049b-9a84-46fb-8f53-869881c3d3ab`) - "Permits management of storage accounts. Provides access to the account key, which can be used to access data via Shared Key authorization."
  - Storage Blob Data Owner (`b7e6dc6d-f1e8-4753-8033-0f276bb0955c`) - "Provides full access to Azure Storage blob containers and data, including assigning POSIX access control."
  - Storage Blob Data Contributor (`ba92f5b4-2d11-453d-a403-e96b0029c9fe`) - "Read, write, and delete Azure Storage containers and blobs."
- [x] **1.3** Define storage permission mapping strategy:
  - **Key Vault Owner** → **Storage Account Contributor** + **Storage Blob Data Owner**
    - Rationale: Owner needs both management access (Storage Account Contributor) and full data access (Storage Blob Data Owner)
    - Storage Account Contributor provides management capabilities similar to vault management
    - Storage Blob Data Owner provides comprehensive data access including POSIX control
  - **Key Vault Administrator** → **Storage Blob Data Contributor**
    - Rationale: Administrator role maps to data-level operations without management overhead
    - Storage Blob Data Contributor provides read/write/delete access to blob data
- [x] **1.4** Identify storage account discovery mechanism:
  - **Primary Strategy**: Same resource group as vault
    - Query all storage accounts in the same resource group as the vault
    - Most common deployment pattern for related resources
  - **Secondary Strategy**: Tag-based association
    - Look for storage accounts with tag `AssociatedVault` = vault name/ID
    - Provides explicit linking mechanism
  - **Fallback Strategy**: Naming convention
    - Look for storage accounts with names containing vault name
    - Pattern: `{vault-name}storage`, `{vault-name}stor`, `stor{vault-name}`
  - **Implementation Order**: Resource group → Tags → Naming convention

## Phase 2: Core Infrastructure Changes
- [x] **2.1** Create new module `StorageRoleManager/storage_role_manager.py`:
  - ✅ Created directory structure with `__init__.py`
  - ✅ Implemented storage account discovery with multiple strategies
  - ✅ Added Storage Management Client integration
  - ✅ Defined storage role mappings and assignment logic
  - ✅ Included comprehensive error handling and logging
- [x] **2.2** Add Azure Storage SDK dependencies to `requirements.txt`:
  - ✅ `azure-mgmt-storage>=21.0.0` - Added for Storage Management operations
  - ✅ Verified compatibility with existing dependencies
- [ ] **2.3** Extend `VaultRoleManager` class or create `StorageRoleManager` class:
  - Add storage account discovery methods
  - Add storage role assignment methods
  - Implement storage account validation
- [ ] **2.4** Update authentication scope to include Storage management:
  - Verify current App Registration has Storage permissions
  - Add required permissions if missing

## Phase 3: Storage Discovery Implementation
- [ ] **3.1** Implement `discover_associated_storage_accounts()` method:
  - Parse vault resource ID to extract subscription/resource group
  - Query storage accounts in same resource group
  - Apply filtering logic (naming convention or tags)
- [ ] **3.2** Add storage account validation:
  - Verify storage account exists and is accessible
  - Check if RBAC is enabled on storage account
  - Validate storage account type compatibility
- [ ] **3.3** Implement storage account metadata extraction:
  - Get storage account properties and tags
  - Check for creator/association tags
  - Extract relevant configuration details

## Phase 4: Role Assignment Logic
- [ ] **4.1** Create `assign_storage_roles_to_user()` method:
  - Mirror signature of `assign_role_to_user()` from vault manager
  - Handle multiple storage accounts per vault
  - Implement batch role assignment
- [ ] **4.2** Implement storage role mapping logic:
  - Map vault Owner to Storage Account Contributor + Blob Data Owner
  - Map vault Administrator to Storage Blob Data Contributor
  - Handle edge cases and role conflicts
- [ ] **4.3** Add storage role cleanup logic:
  - Remove redundant storage role assignments
  - Handle role inheritance and overlap
  - Log all role changes for audit

## Phase 5: Function App Integration
- [x] **5.1** Update `function_app.py` main handler:
  - ✅ Added StorageRoleManager import and initialization
  - ✅ Integrated storage account discovery after vault operations
  - ✅ Added storage role assignment based on vault role success
  - ✅ Enhanced response structure to include storage assignment results
  - ✅ Added comprehensive error handling and logging for storage operations
  - ✅ Maintained backward compatibility - vault-only operations continue to work
- [x] **5.2** Modify request/response structure:
  - ✅ Enhanced response to include `storageAccounts` section with:
    - `discovered`: Number of storage accounts found
    - `assignments`: Detailed role assignment results per storage account
    - `success`: Overall storage assignment success status
  - ✅ Maintained existing API contract - no breaking changes to request structure
  - ✅ Backward compatible - existing clients will receive additional storage info
- [x] **5.3** Update JWT validation and creator verification:
  - ✅ Reused existing vault creator verification for storage operations
  - ✅ Storage role assignment only occurs after successful vault creator verification
  - ✅ No additional authorization needed - storage follows vault permissions
  - ✅ Same JWT validation logic applies to storage operations

## Phase 6: Error Handling & Logging
- [x] **6.1** Implement storage-specific error handling:
  - ✅ Storage account not found errors handled with graceful degradation
  - ✅ Permission denied scenarios managed with proper error logging
  - ✅ Replication delay handling via PrincipalNotFound error management
  - ✅ HttpResponseError and generic exception handling implemented
- [x] **6.2** Add comprehensive logging for storage operations:
  - ✅ Storage account discovery process logged with detailed results
  - ✅ Role assignment attempts and results tracked per storage account
  - ✅ Error contexts logged with specific storage account information
  - ✅ Success/failure status logged for audit purposes
- [x] **6.3** Update error response format:
  - ✅ Enhanced response includes detailed storage operation results
  - ✅ Storage-specific error messages included in logs
  - ✅ Consistent error format maintained with existing vault error handling
  - ✅ Response includes storage account count and assignment details

## Phase 7: Testing Implementation
- [x] **7.1** Create storage role manager unit tests:
  - ✅ Created `test_storage_role_manager.py` with comprehensive unit tests
  - ✅ Tests storage account discovery logic with multiple strategies
  - ✅ Verifies role mapping functionality for Owner and Administrator roles
  - ✅ Mocks Azure Storage Management Client to avoid live API calls
  - ✅ Tests GUID validation and normalization
  - ✅ Tests error scenarios with invalid input
- [ ] **7.2** Update integration tests:
  - Extend `test_integration.py` to include storage operations
  - Test end-to-end vault + storage role assignment
  - Verify creator verification works for both resources
- [ ] **7.3** Add storage-specific test scenarios:
  - Test multiple storage accounts per vault
  - Verify role cleanup and redundancy handling
  - Test error scenarios (missing storage, permissions)

## Phase 8: Configuration & Documentation
- [x] **8.1** Update environment variable documentation:
  - ✅ No new environment variables required - reuses existing Azure credentials
  - ✅ Updated `CLAUDE.md` with comprehensive storage integration details
  - ✅ Added storage role mapping documentation and discovery strategies
  - ✅ Updated dependencies section with azure-mgmt-storage requirement
- [ ] **8.2** Update PowerShell deployment scripts:
  - Modify app registration setup for storage permissions
  - Update deployment verification to test storage operations
  - Add storage-specific health checks
- [ ] **8.3** Update function app configuration:
  - Add any required storage-specific app settings
  - Update `host.json` if needed for storage operations
  - Verify Application Insights captures storage metrics

## Phase 9: Security & Validation
- [ ] **9.1** Security review of storage permissions:
  - Verify principle of least privilege for storage roles
  - Ensure storage operations don't expose sensitive data
  - Review audit logging for storage operations
- [ ] **9.2** Add input validation for storage parameters:
  - Validate storage account naming conventions
  - Sanitize storage-related inputs
  - Add boundary checks for storage operations
- [ ] **9.3** Test with various storage account types:
  - Standard vs Premium storage accounts
  - Different storage access tiers
  - Various storage account configurations

## Phase 10: Deployment & Monitoring
- [ ] **10.1** Create deployment plan:
  - Stage storage integration in development environment
  - Plan production rollout strategy
  - Prepare rollback procedures
- [ ] **10.2** Update monitoring and alerting:
  - Add storage operation metrics to Application Insights
  - Create alerts for storage assignment failures
  - Monitor storage discovery performance
- [ ] **10.3** Performance optimization:
  - Optimize storage account discovery queries
  - Implement caching for repeated storage operations
  - Monitor function execution time with storage operations

## Implementation Notes

### Key Design Decisions
- **Hybrid approach**: Maintain existing vault functionality while adding storage operations
- **Role mapping**: Mirror vault permissions to equivalent storage roles  
- **Discovery mechanism**: Flexible storage account association (same resource group, naming conventions, or tags)
- **Security**: Preserve creator verification and JWT validation patterns
- **Backward compatibility**: Ensure existing vault-only operations continue to work

### Azure Storage Role Mappings
| Vault Role | Storage Roles |
|------------|---------------|
| Owner (`8e3af657-a8ff-443c-a75c-2fe8c4bcb635`) | Storage Account Contributor + Storage Blob Data Owner |
| Key Vault Administrator (`00482a5a-887f-4fb3-b363-3b7fe8e74483`) | Storage Blob Data Contributor |

### Storage Account Discovery Strategy
1. **Same Resource Group**: Find storage accounts in the same resource group as the vault
2. **Naming Convention**: Look for storage accounts with names related to the vault (e.g., `{vault-name}storage`)
3. **Tag Association**: Use tags to explicitly link storage accounts to vaults