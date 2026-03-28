"""Create or verify Azure storage account for Functions runtime."""
import random
import string
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def _generate_name() -> str:
    suffix = "".join(random.choices(string.ascii_lowercase + string.digits, k=8))
    return f"xfunc{suffix}"

def _find_tagged_account(config: InstallerConfig, az: AzCli) -> dict | None:
    # Use run_or_none so AzNotFoundError (e.g. resource group already deleted) returns None
    # gracefully instead of crashing teardown before the confirmation prompt.
    accounts = az.run_or_none("storage", "account", "list", "--resource-group", config.resource_group, "--query", "[?tags.\"xfunction-installer\"=='true']")
    if isinstance(accounts, list) and accounts:
        return accounts[0]
    return None

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    return _find_tagged_account(config, az) is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
    existing = _find_tagged_account(config, az)
    if existing:
        name = existing["name"]
        warning(f"Storage account '{name}' already exists — skipping")
        return {"name": name, "status": "exists"}

    name = config.storage_account if config.storage_account else _generate_name()
    for _ in range(5):
        check = az.run("storage", "account", "check-name", "--name", name)
        if isinstance(check, dict) and check.get("nameAvailable", False):
            break
        name = _generate_name()
    else:
        raise RuntimeError("Failed to find available storage account name after 5 attempts")

    az.run("storage", "account", "create", "--name", name, "--resource-group", config.resource_group, "--sku", "Standard_LRS", "--tags", "xfunction-installer=true")
    success(f"Storage account '{name}' created")
    return {"name": name, "status": "created"}

def teardown(config: InstallerConfig, az: AzCli) -> None:
    existing = _find_tagged_account(config, az)
    if existing:
        az.run("storage", "account", "delete", "--name", existing["name"], "--resource-group", config.resource_group, "--yes")
        success(f"Storage account '{existing['name']}' deleted")
