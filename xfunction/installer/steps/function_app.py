"""Create or verify Azure Function App with app settings."""
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    result = az.run_or_none("functionapp", "show", "--name", config.function_app_name, "--resource-group", config.resource_group)
    return result is not None

def run(config: InstallerConfig, az: AzCli, app_registration_data: dict | None = None) -> dict:
    created = False
    if check_exists(config, az):
        warning(f"Function app '{config.function_app_name}' already exists — updating settings")
    else:
        az.run(
            "functionapp", "create",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--storage-account", config.storage_account,
            "--consumption-plan-location", config.location,
            "--runtime", "python",
            "--runtime-version", "3.11",
            "--functions-version", "4",
            "--os-type", "Linux",
            "--assign-identity", "[system]",
        )
        success(f"Function app '{config.function_app_name}' created")
        created = True

    if app_registration_data:
        tenant_id = app_registration_data.get("tenant_id", "")
        client_id = app_registration_data.get("app_id", "")
        client_secret = app_registration_data.get("client_secret", "")
        settings = [
            f"AZURE_TENANT_ID={tenant_id}",
            f"AZURE_CLIENT_ID={client_id}",
            "FUNCTIONS_WORKER_RUNTIME=python",
            f"EXPECTED_AUDIENCE={client_id}",
        ]
        if client_secret:
            settings.append(f"AZURE_CLIENT_SECRET={client_secret}")
        az.run(
            "functionapp", "config", "appsettings", "set",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--settings", *settings,
        )
        success("App settings configured")
    else:
        warning("No app registration data — skipping app settings configuration")

    return {
        "name": config.function_app_name,
        "url": f"https://{config.function_app_name}.azurewebsites.net",
        "status": "created" if created else "updated",
    }

def teardown(config: InstallerConfig, az: AzCli) -> None:
    if check_exists(config, az):
        az.run("functionapp", "delete", "--name", config.function_app_name, "--resource-group", config.resource_group, "--yes")
        success(f"Function app '{config.function_app_name}' deleted")
