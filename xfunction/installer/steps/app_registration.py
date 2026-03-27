"""Create or verify Azure AD App Registration with Graph permissions."""
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

_GRAPH_API_ID = "00000003-0000-0000-c000-000000000000"
_USER_READ_ALL = "df021288-bdef-4463-88db-98f22de89214"
_APP_READ_ALL = "9a5d68dd-52b0-4cc2-bd40-abcf44ac3a30"

def _find_app_by_name(config: InstallerConfig, az: AzCli) -> dict | None:
    apps = az.run("ad", "app", "list", "--display-name", config.app_name, "--query", f"[?displayName=='{config.app_name}']")
    if isinstance(apps, list) and apps:
        return apps[0]
    return None

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    return _find_app_by_name(config, az) is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
    existing = _find_app_by_name(config, az)
    if existing:
        app_id = existing["appId"]
        warning(f"App registration '{config.app_name}' already exists (appId: {app_id})")
        sp = az.run_or_none("ad", "sp", "show", "--id", app_id)
        sp_object_id = sp["id"] if sp else None
        if not sp_object_id:
            sp_result = az.run("ad", "sp", "create", "--id", app_id)
            sp_object_id = sp_result["id"]
        return {"app_id": app_id, "sp_object_id": sp_object_id, "client_secret": None, "status": "exists"}

    app_result = az.run("ad", "app", "create", "--display-name", config.app_name)
    app_id = app_result["appId"]
    success(f"App registration '{config.app_name}' created (appId: {app_id})")

    sp_result = az.run("ad", "sp", "create", "--id", app_id)
    sp_object_id = sp_result["id"]
    success(f"Service principal created (objectId: {sp_object_id})")

    cred_result = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
    client_secret = cred_result.get("password", "")
    success("Client secret generated (valid for 2 years)")

    az.run("ad", "app", "permission", "add", "--id", app_id, "--api", _GRAPH_API_ID, "--api-permissions", f"{_USER_READ_ALL}=Role", f"{_APP_READ_ALL}=Role")
    success("Graph API permissions added (User.Read.All, Application.Read.All)")

    try:
        az.run("ad", "app", "permission", "admin-consent", "--id", app_id)
        success("Admin consent granted")
    except Exception:
        warning("Admin consent failed — you may need to grant consent manually in Azure Portal")

    return {"app_id": app_id, "sp_object_id": sp_object_id, "client_secret": client_secret, "status": "created"}

def teardown(config: InstallerConfig, az: AzCli) -> None:
    existing = _find_app_by_name(config, az)
    if existing:
        app_id = existing["appId"]
        az.run_or_none("ad", "sp", "delete", "--id", app_id)
        az.run("ad", "app", "delete", "--id", app_id)
        success(f"App registration '{config.app_name}' deleted")
