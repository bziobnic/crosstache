"""Create or verify Azure resource group."""
from installer.az import AzCli, AzNotFoundError
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    result = az.run_or_none("group", "show", "--name", config.resource_group)
    return result is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
    if check_exists(config, az):
        warning(f"Resource group '{config.resource_group}' already exists — skipping")
        return {"name": config.resource_group, "status": "exists"}
    result = az.run("group", "create", "--name", config.resource_group, "--location", config.location)
    success(f"Resource group '{config.resource_group}' created in {config.location}")
    return {"name": config.resource_group, "location": config.location, "status": "created"}

def teardown(config: InstallerConfig, az: AzCli) -> None:
    if check_exists(config, az):
        az.run("group", "delete", "--name", config.resource_group, "--yes", "--no-wait")
        success(f"Resource group '{config.resource_group}' deletion initiated")
