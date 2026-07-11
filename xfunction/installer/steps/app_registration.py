"""Create or verify the installer-owned Azure AD App Registration."""
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def _find_app_by_name(config: InstallerConfig, az: AzCli) -> dict | None:
    apps = az.run("ad", "app", "list", "--display-name", config.app_name, "--query", f"[?displayName=='{config.app_name}']")
    if isinstance(apps, list) and apps:
        return apps[0]
    return None

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    return _find_app_by_name(config, az) is not None

def run(config: InstallerConfig, az: AzCli, expected_object_id: str = "") -> dict:
    existing = _find_app_by_name(config, az)
    if existing:
        object_id = existing.get("id", "")
        if not expected_object_id or object_id.lower() != expected_object_id.lower():
            raise RuntimeError(
                f"App registration '{config.app_name}' already exists but installer ownership "
                "cannot be verified from persisted object ID; choose a new --app-name or resume "
                "with the original protected installer state"
            )
        app_id = existing["appId"]
        warning(f"Verified installer-owned app registration '{config.app_name}' (appId: {app_id})")
        sp = az.run_or_none("ad", "sp", "show", "--id", app_id)
        sp_object_id = sp["id"] if sp else None
        if not sp_object_id:
            sp_result = az.run("ad", "sp", "create", "--id", app_id)
            sp_object_id = sp_result["id"]
        return {
            "name": config.app_name,
            "app_id": app_id,
            "app_object_id": object_id,
            "sp_object_id": sp_object_id,
            "client_secret": None,
            "status": "exists",
        }

    app_result = az.run("ad", "app", "create", "--display-name", config.app_name)
    app_id = app_result["appId"]
    app_object_id = app_result["id"]
    success(f"App registration '{config.app_name}' created (appId: {app_id})")

    sp_result = az.run("ad", "sp", "create", "--id", app_id)
    sp_object_id = sp_result["id"]
    success(f"Service principal created (objectId: {sp_object_id})")

    cred_result = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
    client_secret = cred_result.get("password", "")
    success("Client secret generated (valid for 2 years)")

    return {
        "name": config.app_name,
        "app_id": app_id,
        "app_object_id": app_object_id,
        "sp_object_id": sp_object_id,
        "client_secret": client_secret,
        "status": "created",
    }

def teardown(config: InstallerConfig, az: AzCli, state_data: dict | None = None) -> None:
    state_data = state_data or {}
    if state_data.get("status") != "created":
        return
    app_id = state_data.get("app_id", "")
    object_id = state_data.get("app_object_id", "")
    existing = az.run_or_none("ad", "app", "show", "--id", object_id or app_id)
    if existing is None:
        return
    if not object_id or existing.get("id", "").lower() != object_id.lower():
        raise RuntimeError("Refusing to delete app registration: persisted object ID does not match")
    az.run_or_none("ad", "sp", "delete", "--id", app_id)
    az.run("ad", "app", "delete", "--id", app_id)
    success(f"App registration '{config.app_name}' deleted")
