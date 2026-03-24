# xfunction Azure Function — Code Review

> Reviewed: 2026-03-24 | Deep-dive analysis of VaultRbacProcessor + StorageRoleManager

---

## Executive Summary

The Azure Function handles RBAC role assignments for Key Vaults and associated Storage Accounts. The core logic works but has **5 critical issues** (led by missing JWT signature verification), significant code duplication between the two role managers, and gaps in test coverage. The PowerShell scripts have hardcoded subscription IDs and one script (`configure-graph-permissions.ps1`) fundamentally misunderstands Azure managed identity permissions.

**Total issues found: 35** — 5 critical, 7 high, 8 medium, 10 low, 5 informational.

---

## Critical Issues

### C1. JWT Token Signature Verification Not Implemented
**File:** `function_app.py:49`
```python
decoded_token = jwt.decode(token, options={"verify_signature": False})
```
Comment claims "This is safe because we'll verify the token in the next step" but there is **no subsequent signature verification**. Only token expiration is checked.

**Risk:** An attacker can craft a JWT with any user ID and the function will accept it. Token expiration alone is insufficient — the token's signature must be validated against Azure AD public keys, and the `iss` and `aud` claims must be verified.

### C2. Hardcoded Role IDs Duplicated Across Three Files
**Files:**
- `function_app.py:168–169`
- `VaultRbacProcessor/vault_role_manager.py:63–64, 106–107`
- `StorageRoleManager/storage_role_manager.py:19–21, 134–136`

Role IDs (Owner, Key Vault Admin, Storage roles) are hardcoded in multiple places. Changing a role ID requires updating 3+ files. Should be centralized in a config module.

### C3. Misleading "Success" Returns on Azure API Failures
**Files:** `vault_role_manager.py:207–213`, `storage_role_manager.py:258–261`
```python
elif "PrincipalNotFound" in str(ex):
    logging.warning("This might be due to replication delays. Try again later.")
    return True  # Still return success since we'll retry on next event
```
The function returns HTTP 200 even when Azure RBAC assignments fail. This masks failures and makes them impossible to diagnose from the caller's perspective.

### C4. Bearer Token Parsing Has No Bounds Checking
**File:** `function_app.py:42`
```python
token = auth_header.split(' ')[1]
```
If the Authorization header is `"Bearer"` with no token value, this raises an unhandled `IndexError` → 500 instead of a proper 401.

### C5. Missing Environment Variable Validation
**Files:** `vault_role_manager.py:20–22`, `storage_role_manager.py:25–27`
```python
tenant_id = os.environ["AZURE_TENANT_ID"]
client_id = os.environ["AZURE_CLIENT_ID"]
client_secret = os.environ["AZURE_CLIENT_SECRET"]
```
Missing env vars raise `KeyError` during module initialization, crashing the entire function app with a cryptic error.

---

## High Severity Issues

### H1. Duplicate Code Across VaultRoleManager and StorageRoleManager
Identical methods in both files (~70+ lines duplicated):
- `_detect_principal_type()` (~24 lines)
- `get_principal_id_for_user()` (~30 lines)
- `_is_guid()` (~7 lines)
- `_normalize_guid()` (~12 lines)

Bug fixes must be applied twice. Should be extracted to a shared `utils/azure_helpers.py`.

### H2. String-Based Error Checking Instead of Exception Types
**Files:** `vault_role_manager.py:205–213`, `storage_role_manager.py` (similar)
```python
if "already exists" in str(ex):
    ...
elif "PrincipalNotFound" in str(ex):
    ...
```
Fragile — Azure error messages could change across SDK versions. Should check `isinstance(ex, ResourceExistsError)` or `ex.error.code`.

### H3. No Rate Limiting or Throttling
Each request makes multiple Azure API calls (Graph API for principal detection, RBAC assignments, storage account discovery) with no rate limiting or exponential backoff. Could trigger Azure 429 throttling on bulk operations.

### H4. No Timeout Configuration for Azure Clients
Azure SDK clients use default timeouts. Azure Functions have a 10-minute limit — no configured request timeout could cause functions to hang.

### H5. Incomplete Test Coverage for Async Functions
`test_storage_role_manager.py` uses `asyncio.run()` but doesn't test concurrent scenarios, async cleanup, or timeout behavior. Should use `unittest.IsolatedAsyncioTestCase` consistently.

### H6. Principal Type Detection Makes 3 Sequential Graph API Calls
**File:** `vault_role_manager.py:220–254`
`_detect_principal_type()` tries User, then ServicePrincipal, then Group lookups sequentially. No caching, no fallback if Graph API is down. Could be 6+ HTTP calls per request.

### H7. test_event.py Has Syntax Error
**File:** `scripts/test_event.py:134–135`
```python
if __name__ == "__main__":
    main() if __name__ == "__main__":
    main()
```
Duplicate `main()` call — invalid Python syntax. Script will fail to run.

---

## Medium Severity Issues

### M1. Storage Discovery Falls Back to ALL Accounts in Resource Group
**File:** `storage_role_manager.py:87`
```python
if not storage_accounts and accounts_in_rg:
    storage_accounts = [account.id for account in accounts_in_rg]
```
If no naming-convention matches are found, roles get assigned to **every** storage account in the resource group. Should be configurable or conservative (return empty list).

### M2. Vault Creator Verification Doesn't Normalize UUIDs
**File:** `function_app.py:145`
Comparison uses `.lower()` but doesn't normalize UUID format (with/without hyphens). Edge case where different UUID representations fail the creator check.

### M3. Inconsistent HTTP Status Codes for Partial Failures
If vault role assignments succeed but storage fails → HTTP 500. If 1 of 2 vault roles fails → HTTP 500. No use of 207 Multi-Status or similar for partial success.

### M4. Resource ID Validation Happens After Operations Start
Vault resource ID format validation occurs after managers are initialized and operations attempted. Should validate immediately after extraction.

### M5. Storage Account Discovery Doesn't Filter Deleted Accounts
API listing includes soft-deleted storage accounts. Attempting role assignment on deleted accounts fails silently.

### M6. No Handling for Service Principal as Vault Creator
Creator verification logic only works for user principals. Service principals creating vaults can't verify themselves.

### M7. CSV Injection Risk in Logging
**File:** `function_app.py:50`
Token claims logged without sanitization. If logs are exported to CSV, malicious claims could perform CSV injection.

### M8. Missing Integration Tests
`tests/test_integration.py` exists but is minimal (~112 lines) with no actual Azure API integration tests.

---

## Low Severity Issues

### L1. Magic Numbers in host.json
`maxEventBatchSize: 1` and `maxBatchSize: 1` without explanation.

### L2. STORAGE_IMPLEMENTATION_CHECKLIST.md Drift
Phases 3, 4, 7.2–7.3, 8.2–8.3, 9, 10 shown as incomplete — needs update.

### L3. No Vault Name Format Validation
Vault names assumed valid but never checked against Azure naming rules.

### L4. Generic Exception Handlers
Broad `except Exception` catches mask specific error types.

### L5. No Audit Trail Beyond Azure Native Logs
Role assignments don't produce structured audit entries.

### L6. Inconsistent Response Field Naming
Response uses camelCase (`ownerRoleAssigned`) while request uses mixed conventions.

### L7. Missing Type Hints
No type annotations on function signatures across all Python files.

### L8. Incomplete Docstrings
HTTP trigger function lacks request/response format documentation.

### L9. No Token Expiration Clock Skew Buffer
Token expiration check at line 55 doesn't account for clock skew. Should add a 5-minute buffer.

### L10. Redundant Exception Handling in Role Assignment
Multiple layers of try/except that catch and re-log the same error.

---

## Script Issues

### Hardcoded Subscription ID (3 scripts)
**Files:** `setup-managed-identity.ps1:2`, `test-function.ps1:2`, `setup-app-registration.ps1`
Subscription ID `250d9a86-64a4-457e-a34e-fb2898eda332` hardcoded in source control. Should be parameterized.

### Plaintext Secret Output
**File:** `setup-app-registration.ps1:42`
Client secret printed to console in plaintext.

### configure-graph-permissions.ps1 — Fundamentally Broken
Attempts to assign Graph API delegated permissions to a managed identity. Managed identities only support app roles, not delegated permissions. The script acknowledges this by falling back to manual Portal instructions at lines 91–103.

### test-function.ps1 — No Resource Cleanup
Creates a Key Vault for testing but never deletes it. Hardcoded 15-second wait is brittle.

### get-function-logs.ps1 — Deprecated CLI Commands
Uses `az functionapp log` (deprecated). Should use `az functionapp logs tail` or `az monitor log-analytics query`.

### update-event-grid-filter.ps1 — No Rollback
Sequential filter updates with no rollback if one fails. Could leave Event Grid subscription in a broken state.

---

## Test Coverage Assessment

| Test File | Coverage | Key Gaps |
|-----------|----------|----------|
| `test_direct_rbac_processor.py` | ~70% | No JWT signature testing, no Azure exception type testing, no concurrent scenarios |
| `test_storage_role_manager.py` | ~60% | No network failure testing, no real Azure format testing, weak async coverage |
| `test_integration.py` | ~10% | Minimal — no actual Azure API integration tests |

### Tests That Would Pass Even With Broken Code
- JWT signature validation disabled → tests still pass (mocks bypass verification)
- Azure API calls fail in production → mocked tests pass
- Storage naming convention returns false positives → tests don't check word boundaries

---

## Recommended Priority Actions

| # | Action | Severity | Effort |
|---|--------|----------|--------|
| 1 | Implement JWT signature verification with Azure AD public keys | Critical | Medium |
| 2 | Centralize role IDs into a config module | Critical | Low |
| 3 | Fix misleading success returns — report actual failures | Critical | Low |
| 4 | Add bounds checking to bearer token parsing | Critical | Low |
| 5 | Validate env vars at startup with clear errors | Critical | Low |
| 6 | Extract duplicate code to shared utils module | High | Medium |
| 7 | Fix test_event.py syntax error | High | Low |
| 8 | Replace string-based error checking with exception types | High | Medium |
| 9 | Add rate limiting / exponential backoff | High | Medium |
| 10 | Parameterize hardcoded subscription IDs in scripts | Medium | Low |
