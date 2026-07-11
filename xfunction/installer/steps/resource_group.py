"""Create or verify Azure resource group."""
from installer.az import AzCli, AzNotFoundError
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    result = az.run_or_none("group", "show", "--name", config.resource_group)
    return result is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
    existing = az.run_or_none("group", "show", "--name", config.resource_group)
    if existing is not None:
        warning(f"Resource group '{config.resource_group}' already exists — skipping")
        return {
            "name": config.resource_group,
            "resource_id": existing.get("id", "") if isinstance(existing, dict) else "",
            "status": "exists",
        }
    result = az.run("group", "create", "--name", config.resource_group, "--location", config.location)
    success(f"Resource group '{config.resource_group}' created in {config.location}")
    return {
        "name": config.resource_group,
        "resource_id": result.get("id", "") if isinstance(result, dict) else "",
        "location": config.location,
        "status": "created",
    }

def teardown(config: InstallerConfig, az: AzCli, state_data: dict | None = None) -> None:
    state_data = state_data or {}
    if state_data.get("status") != "created":
        return
    expected_id = state_data.get("resource_id", "")
    parts = expected_id.strip("/").split("/")
    if (
        len(parts) != 4
        or parts[0].lower() != "subscriptions"
        or parts[2].lower() != "resourcegroups"
        or not parts[1]
        or not parts[3]
    ):
        raise RuntimeError("Refusing to delete resource group: persisted resource ID is invalid")
    subscription_id, resource_group_name = parts[1], parts[3]
    existing = az.run_or_none(
        "group", "show", "--name", resource_group_name,
        "--subscription", subscription_id,
    )
    if existing is None:
        return
    if not expected_id or existing.get("id", "").lower() != expected_id.lower():
        raise RuntimeError("Refusing to delete resource group: persisted resource ID does not match")
    az.run(
        "group", "delete", "--name", resource_group_name,
        "--subscription", subscription_id, "--yes", "--no-wait",
    )
    success(f"Resource group '{resource_group_name}' deletion initiated")
