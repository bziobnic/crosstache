"""Create or verify Azure Function App with app settings."""
import json
import os
import tempfile
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    result = az.run_or_none("functionapp", "show", "--name", config.function_app_name, "--resource-group", config.resource_group)
    return result is not None

def run(
    config: InstallerConfig,
    az: AzCli,
    app_registration_data: dict | None = None,
    expected_resource_id: str = "",
    expected_status: str = "",
) -> dict:
    created = False
    existing = az.run_or_none(
        "functionapp", "show", "--name", config.function_app_name,
        "--resource-group", config.resource_group,
    )
    resource_id = ""
    if existing is not None:
        resource_id = existing.get("id", "") if isinstance(existing, dict) else ""
        if not expected_resource_id or resource_id.lower() != expected_resource_id.lower():
            raise RuntimeError(
                f"Function app '{config.function_app_name}' already exists but installer ownership "
                "cannot be verified from persisted resource ID; choose a new name or resume with "
                "the original protected installer state"
            )
        warning(f"Verified installer-owned Function app '{config.function_app_name}'")
    else:
        created_result = az.run(
            "functionapp", "create",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--storage-account", config.storage_account,
            "--consumption-plan-location", config.location,
            "--runtime", "python",
            "--runtime-version", "3.11",
            "--functions-version", "4",
            "--os-type", "Linux",
        )
        success(f"Function app '{config.function_app_name}' created")
        created = True
        if isinstance(created_result, dict):
            resource_id = created_result.get("id", "")
        if not resource_id:
            resource_id = (
                f"/subscriptions/{config.subscription_id}/resourceGroups/{config.resource_group}"
                f"/providers/Microsoft.Web/sites/{config.function_app_name}"
            )

    if app_registration_data:
        tenant_id = app_registration_data.get("tenant_id", "")
        client_id = app_registration_data.get("app_id", "")
        client_secret = app_registration_data.get("client_secret", "")
        settings = {
            "AZURE_TENANT_ID": tenant_id,
            "AZURE_CLIENT_ID": client_id,
            "FUNCTIONS_WORKER_RUNTIME": "python",
            "EXPECTED_AUDIENCE": client_id,
            "ALLOWED_RESOURCE_GROUP_ID": (
                f"/subscriptions/{config.subscription_id}/resourceGroups/{config.resource_group}"
            ),
            "ALLOWED_PRINCIPAL_ID": app_registration_data.get("delegated_principal_id", ""),
        }
        if client_secret:
            settings["AZURE_CLIENT_SECRET"] = client_secret
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            settings_path = handle.name
            json.dump(settings, handle)
        try:
            os.chmod(settings_path, 0o600)
            az.run(
                "functionapp", "config", "appsettings", "set",
                "--name", config.function_app_name,
                "--resource-group", config.resource_group,
                "--settings", f"@{settings_path}",
            )
        finally:
            try:
                os.unlink(settings_path)
            except FileNotFoundError:
                pass
        success("App settings configured")
    else:
        warning("No app registration data — skipping app settings configuration")

    return {
        "name": config.function_app_name,
        "resource_id": resource_id,
        "url": f"https://{config.function_app_name}.azurewebsites.net",
        "status": "created" if created or expected_status == "created" else "updated",
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
        or parts[5].lower() != "microsoft.web"
        or parts[6].lower() != "sites"
        or not all((parts[1], parts[3], parts[7]))
    ):
        raise RuntimeError("Refusing to delete Function App: persisted resource ID is invalid")
    subscription_id, resource_group_name, function_app_name = parts[1], parts[3], parts[7]
    existing = az.run_or_none(
        "functionapp", "show", "--name", function_app_name,
        "--resource-group", resource_group_name, "--subscription", subscription_id,
    )
    if existing is None:
        return
    if not expected_id or existing.get("id", "").lower() != expected_id.lower():
        raise RuntimeError("Refusing to delete Function App: persisted resource ID does not match")
    az.run(
        "functionapp", "delete", "--name", function_app_name,
        "--resource-group", resource_group_name, "--subscription", subscription_id, "--yes",
    )
    success(f"Function app '{function_app_name}' deleted")
