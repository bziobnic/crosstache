"""Create or verify Azure storage account for Functions runtime."""
import random
import string
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def _generate_name() -> str:
    suffix = "".join(random.choices(string.ascii_lowercase + string.digits, k=8))
    return f"xfunc{suffix}"

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    if not config.storage_account:
        return False
    return az.run_or_none(
        "storage", "account", "show", "--name", config.storage_account,
        "--resource-group", config.resource_group,
    ) is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
    name = config.storage_account if config.storage_account else _generate_name()
    if config.storage_account and check_exists(config, az):
        raise RuntimeError(
            f"Storage account '{name}' already exists but installer ownership cannot be verified; "
            "choose a new --storage-account name"
        )
    for _ in range(5):
        check = az.run("storage", "account", "check-name", "--name", name)
        if isinstance(check, dict) and check.get("nameAvailable", False):
            break
        name = _generate_name()
    else:
        raise RuntimeError("Failed to find available storage account name after 5 attempts")

    result = az.run("storage", "account", "create", "--name", name, "--resource-group", config.resource_group, "--sku", "Standard_LRS", "--tags", "xfunction-installer=true")
    success(f"Storage account '{name}' created")
    return {
        "name": name,
        "resource_id": result.get("id", "") if isinstance(result, dict) else "",
        "status": "created",
    }

def teardown(config: InstallerConfig, az: AzCli, state_data: dict | None = None) -> None:
    state_data = state_data or {}
    if state_data.get("status") != "created":
        return
    expected_id = state_data.get("resource_id", "")
    parts = expected_id.strip("/").split("/")
    if (
        len(parts) != 8
        or parts[0].lower() != "subscriptions"
        or parts[2].lower() != "resourcegroups"
        or parts[4].lower() != "providers"
        or parts[5].lower() != "microsoft.storage"
        or parts[6].lower() != "storageaccounts"
        or not all((parts[1], parts[3], parts[7]))
    ):
        raise RuntimeError("Refusing to delete storage account: persisted resource ID is invalid")
    subscription_id, resource_group_name, name = parts[1], parts[3], parts[7]
    existing = az.run_or_none(
        "storage", "account", "show", "--name", name,
        "--resource-group", resource_group_name, "--subscription", subscription_id,
    )
    if existing is None:
        return
    if not expected_id or existing.get("id", "").lower() != expected_id.lower():
        raise RuntimeError("Refusing to delete storage account: persisted resource ID does not match")
    az.run(
        "storage", "account", "delete", "--name", name,
        "--resource-group", resource_group_name, "--subscription", subscription_id, "--yes",
    )
    success(f"Storage account '{name}' deleted")
