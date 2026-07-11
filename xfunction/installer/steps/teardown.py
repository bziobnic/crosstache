"""Orchestrate teardown of all installer-created resources."""
import shutil
from installer.az import AzCli
from installer.config import InstallerConfig, InstallerState
from installer.utils.output import success, warning, step_header, summary_table
from installer.utils.prompts import confirm
from installer.steps import resource_group, storage_account, app_registration, function_app, rbac

def run(config: InstallerConfig, az: AzCli, state: InstallerState) -> None:
    resources_to_delete = []
    rbac_data = state.get_step_data("rbac")
    app_data = state.get_step_data("app_registration")
    function_data = state.get_step_data("function_app")
    storage_data = state.get_step_data("storage_account")
    resource_group_data = state.get_step_data("resource_group")

    if rbac_data.get("assignment_ids"):
        resources_to_delete.append(("RBAC Assignments", str(len(rbac_data["assignment_ids"])), ""))
    if app_data.get("status") == "created":
        resources_to_delete.append(("App Registration", app_data.get("name", app_data.get("app_id", "")), ""))
    if function_data.get("status") == "created":
        resources_to_delete.append(("Function App", function_data.get("name", ""), ""))
    if storage_data.get("status") == "created":
        resources_to_delete.append(("Storage Account", storage_data.get("name", ""), ""))
    if not config.keep_resource_group and resource_group_data.get("status") == "created":
        resources_to_delete.append(("Resource Group", resource_group_data.get("name", ""), ""))

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

    if rbac_data.get("assignment_ids"):
        step_num += 1
        step_header(step_num, total, "Removing role assignments...")
        try:
            rbac.teardown(config, az, assignment_ids=rbac_data["assignment_ids"])
            results.append(("RBAC Assignments", str(len(rbac_data["assignment_ids"])), "Removed"))
        except Exception as ex:
            results.append(("RBAC Assignments", str(len(rbac_data["assignment_ids"])), f"Failed: {ex}"))

    if app_data.get("status") == "created":
        step_num += 1
        step_header(step_num, total, "Deleting app registration...")
        try:
            app_registration.teardown(config, az, app_data)
            results.append(("App Registration", app_data.get("name", app_data.get("app_id", "")), "Deleted"))
        except Exception as ex:
            results.append(("App Registration", app_data.get("name", app_data.get("app_id", "")), f"Failed: {ex}"))

    if function_data.get("status") == "created":
        step_num += 1
        step_header(step_num, total, "Deleting function app...")
        try:
            function_app.teardown(config, az, function_data)
            results.append(("Function App", function_data.get("name", ""), "Deleted"))
        except Exception as ex:
            results.append(("Function App", function_data.get("name", ""), f"Failed: {ex}"))

    if storage_data.get("status") == "created":
        step_num += 1
        step_header(step_num, total, "Deleting storage account...")
        try:
            storage_account.teardown(config, az, storage_data)
            results.append(("Storage Account", storage_data.get("name", ""), "Deleted"))
        except Exception as ex:
            results.append(("Storage Account", "", f"Failed: {ex}"))

    if not config.keep_resource_group and resource_group_data.get("status") == "created":
        step_num += 1
        step_header(step_num, total, "Deleting resource group...")
        try:
            resource_group.teardown(config, az, resource_group_data)
            results.append(("Resource Group", resource_group_data.get("name", ""), "Deleted"))
        except Exception as ex:
            results.append(("Resource Group", resource_group_data.get("name", ""), f"Failed: {ex}"))

    failures = [result for result in results if result[2].startswith("Failed:")]
    if failures:
        state.save()
        summary_table(results)
        raise RuntimeError(
            f"Teardown incomplete: {len(failures)} privileged cleanup operation(s) failed; "
            "installer state was retained for retry"
        )

    # Clean up xv credentials only after privileged resource cleanup succeeds.
    if shutil.which("xv"):
        should_clean_xv = config.non_interactive
        if not config.non_interactive:
            should_clean_xv = confirm("Remove xv-stored credentials (group: xfunction)?", default=True)
        if should_clean_xv:
            try:
                import subprocess
                for secret in ["azure-tenant-id", "azure-client-id", "azure-client-secret", "function-app-url"]:
                    result = subprocess.run(
                        ["xv", "delete", secret, "--group", "xfunction"],
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    if result.returncode != 0:
                        raise RuntimeError(
                            f"xv credential cleanup failed for {secret}: {result.stderr.strip()}"
                        )
                success("Removed xv-stored credentials")
            except Exception as ex:
                state.save()
                raise RuntimeError(
                    f"Azure cleanup succeeded, but credential cleanup failed; state retained: {ex}"
                ) from ex

    state.clear()
    success("State file removed")
    summary_table(results)
