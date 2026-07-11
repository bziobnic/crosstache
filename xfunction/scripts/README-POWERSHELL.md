# Key Vault RBAC Processor PowerShell Scripts

The supported provisioning workflow is `python -m installer install` from the `xfunction` directory. It persists exact resource ownership IDs and can safely resume or uninstall.

The remaining PowerShell scripts are narrow maintenance helpers:

- `setup-app-registration.ps1` creates a fresh app registration without Graph permissions and applies credentials through a private temporary file.
- `deploy-function.ps1` publishes from the actual `xfunction` project root and fails on every native-command error.
- `verify-deployment.ps1` verifies the deployed HTTP Function inventory, exact constrained resource-group RBAC assignment, required settings, and absence of the obsolete Event Grid trigger.
- `update-event-grid-filter.ps1` and `fix-event-grid-endpoint.ps1` are intentionally disabled because no Event Grid-triggered Function is deployed.
- `configure-graph-permissions.ps1` is a no-op because Graph permissions are not required.
- `setup-managed-identity.ps1` is intentionally disabled because its old workflow granted an unused identity subscription-wide administration.

Example deployment and verification:

```powershell
python -m installer install
./deploy-function.ps1 -FunctionAppName <name> -ResourceGroup <group>
./verify-deployment.ps1 -FunctionAppName <name> -ResourceGroup <group> -SubscriptionId <subscription>
```
