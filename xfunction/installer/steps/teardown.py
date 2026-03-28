"""Orchestrate teardown of all installer-created resources."""
import shutil
from installer.az import AzCli
from installer.config import InstallerConfig, InstallerState
from installer.utils.output import success, warning, step_header, summary_table
from installer.utils.prompts import confirm
from installer.steps import resource_group, storage_account, app_registration, function_app, rbac

def run(config: InstallerConfig, az: AzCli, state: InstallerState) -> None:
    resources_to_delete = []
    sp_object_id = state.get_step_data("app_registration").get("sp_object_id", "")

    if sp_object_id:
        resources_to_delete.append(("RBAC Assignments", "3 roles", ""))
    if app_registration.check_exists(config, az):
        resources_to_delete.append(("App Registration", config.app_name, ""))
    if function_app.check_exists(config, az):
        resources_to_delete.append(("Function App", config.function_app_name, ""))
    if storage_account.check_exists(config, az):
        sa_data = state.get_step_data("storage_account")
        resources_to_delete.append(("Storage Account", sa_data.get("name", "tagged account"), ""))
    if not config.keep_resource_group and resource_group.check_exists(config, az):
        resources_to_delete.append(("Resource Group", config.resource_group, ""))

    if not resources_to_delete:
        warning("No resources found to delete")
        return

    print("\nThe following resources will be deleted:")
    for resource, name, _ in resources_to_delete:
        print(f"  - {resource}: {name}")

    if not config.non_interactive:
        if not confirm("\nProceed with deletion?", default=False):
            warning("Teardown cancelled")
            return

    total = len(resources_to_delete)
    step_num = 0
    results = []

    if sp_object_id:
        step_num += 1
        step_header(step_num, total, "Removing role assignments...")
        try:
            rbac.teardown(config, az, sp_object_id=sp_object_id)
            results.append(("RBAC Assignments", "3 roles", "Removed"))
        except Exception as ex:
            results.append(("RBAC Assignments", "3 roles", f"Failed: {ex}"))

    if app_registration.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting app registration...")
        try:
            app_registration.teardown(config, az)
            results.append(("App Registration", config.app_name, "Deleted"))
        except Exception as ex:
            results.append(("App Registration", config.app_name, f"Failed: {ex}"))

    if function_app.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting function app...")
        try:
            function_app.teardown(config, az)
            results.append(("Function App", config.function_app_name, "Deleted"))
        except Exception as ex:
            results.append(("Function App", config.function_app_name, f"Failed: {ex}"))

    if storage_account.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting storage account...")
        try:
            storage_account.teardown(config, az)
            sa_name = state.get_step_data("storage_account").get("name", "")
            results.append(("Storage Account", sa_name, "Deleted"))
        except Exception as ex:
            results.append(("Storage Account", "", f"Failed: {ex}"))

    if not config.keep_resource_group and resource_group.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting resource group...")
        try:
            resource_group.teardown(config, az)
            results.append(("Resource Group", config.resource_group, "Deleted"))
        except Exception as ex:
            results.append(("Resource Group", config.resource_group, f"Failed: {ex}"))

    # Clean up xv credentials (prompt in interactive mode)
    if shutil.which("xv"):
        should_clean_xv = config.non_interactive
        if not config.non_interactive:
            should_clean_xv = confirm("Remove xv-stored credentials (group: xfunction)?", default=True)
        if should_clean_xv:
            try:
                import subprocess
                for secret in ["azure-tenant-id", "azure-client-id", "azure-client-secret", "function-app-url"]:
                    subprocess.run(["xv", "delete", secret, "--group", "xfunction"], capture_output=True, timeout=10)
                success("Removed xv-stored credentials")
            except Exception:
                warning("Could not clean up xv credentials")

    state.clear()
    success("State file removed")
    summary_table(results)
