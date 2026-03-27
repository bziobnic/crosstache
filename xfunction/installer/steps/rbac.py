"""Assign RBAC roles to the App Registration's service principal."""
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

_ROLES = [
    ("Role Based Access Control Administrator", "Create/manage role assignments"),
    ("Key Vault Administrator", "Read vault tags for creator verification"),
    ("Reader", "List storage accounts for discovery"),
]

def check_exists(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> bool:
    if not sp_object_id:
        return False
    scope = f"/subscriptions/{config.subscription_id}"
    assignments = az.run("role", "assignment", "list", "--assignee", sp_object_id, "--scope", scope)
    if not isinstance(assignments, list):
        return False
    assigned_roles = {a.get("roleDefinitionName", "") for a in assignments}
    return all(role_name in assigned_roles for role_name, _ in _ROLES)

def run(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> dict:
    if not sp_object_id:
        raise ValueError("sp_object_id is required for RBAC step")
    scope = f"/subscriptions/{config.subscription_id}"
    results = {}
    existing = az.run("role", "assignment", "list", "--assignee", sp_object_id, "--scope", scope)
    assigned_roles = {a.get("roleDefinitionName", "") for a in existing} if isinstance(existing, list) else set()

    for role_name, purpose in _ROLES:
        if role_name in assigned_roles:
            warning(f"Role '{role_name}' already assigned — skipping")
            results[role_name] = "exists"
            continue
        az.run(
            "role", "assignment", "create",
            "--assignee-object-id", sp_object_id,
            "--assignee-principal-type", "ServicePrincipal",
            "--role", role_name,
            "--scope", scope,
        )
        success(f"Role '{role_name}' assigned ({purpose})")
        results[role_name] = "assigned"

    return {"roles": results, "status": "configured"}

def teardown(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> None:
    if not sp_object_id:
        return
    scope = f"/subscriptions/{config.subscription_id}"
    for role_name, _ in _ROLES:
        try:
            az.run("role", "assignment", "delete", "--assignee", sp_object_id, "--role", role_name, "--scope", scope, "--yes")
            success(f"Role '{role_name}' removed")
        except Exception:
            warning(f"Could not remove role '{role_name}' — may not exist")
