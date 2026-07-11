"""Assign narrowly conditioned RBAC roles to the Function service principal."""
import uuid

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning


_ROLES = [
    ("Role Based Access Control Administrator", "Create/manage constrained role assignments"),
    ("Reader", "Read vault and assignment metadata"),
]

_ASSIGNABLE_ROLE_IDS = [
    "8e3af657-a8ff-443c-a75c-2fe8c4bcb635",  # Owner
    "00482a5a-887f-4fb3-b363-3b7fe8e74483",  # Key Vault Administrator
    "17d1049b-9a84-46fb-8f53-869881c3d3ab",  # Storage Account Contributor
    "b7e6dc6d-f1e8-4753-8033-0f276bb0955b",  # Storage Blob Data Owner
    "ba92f5b4-2d11-453d-a403-e96b0029c9fe",  # Storage Blob Data Contributor
]


def _scope(config: InstallerConfig) -> str:
    return f"/subscriptions/{config.subscription_id}/resourceGroups/{config.resource_group}"


def _rbac_condition(delegated_principal_id: str) -> str:
    """Limit assignment writes/deletes to approved roles and one exact user."""
    role_ids = ", ".join(_ASSIGNABLE_ROLE_IDS)
    return (
        "((!(ActionMatches{'Microsoft.Authorization/roleAssignments/write'})) OR "
        "(@Request[Microsoft.Authorization/roleAssignments:RoleDefinitionId] "
        f"ForAnyOfAnyValues:GuidEquals {{{role_ids}}} AND "
        "@Request[Microsoft.Authorization/roleAssignments:PrincipalId] "
        f"ForAnyOfAnyValues:GuidEquals {{{delegated_principal_id}}})) AND "
        "((!(ActionMatches{'Microsoft.Authorization/roleAssignments/delete'})) OR "
        "(@Resource[Microsoft.Authorization/roleAssignments:RoleDefinitionId] "
        f"ForAnyOfAnyValues:GuidEquals {{{role_ids}}} AND "
        "@Resource[Microsoft.Authorization/roleAssignments:PrincipalId] "
        f"ForAnyOfAnyValues:GuidEquals {{{delegated_principal_id}}}))"
    )


def _normalized_condition(value: str | None) -> str:
    return "".join((value or "").split()).lower()


def _has_exact_delegation_condition(
    assignment: dict,
    delegated_principal_id: str,
) -> bool:
    return (
        assignment.get("conditionVersion") == "2.0"
        and _normalized_condition(assignment.get("condition"))
        == _normalized_condition(_rbac_condition(delegated_principal_id))
    )


def check_exists(
    config: InstallerConfig,
    az: AzCli,
    sp_object_id: str = "",
    delegated_principal_id: str = "",
) -> bool:
    if not sp_object_id or not delegated_principal_id:
        return False
    scope = _scope(config)
    assignments = az.run(
        "role", "assignment", "list", "--assignee", sp_object_id, "--scope", scope
    )
    if not isinstance(assignments, list):
        return False
    exact = [
        assignment
        for assignment in assignments
        if isinstance(assignment, dict)
        and assignment.get("scope", "").lower() == scope.lower()
    ]
    for role_name, _ in _ROLES:
        matches = [
            assignment
            for assignment in exact
            if assignment.get("roleDefinitionName", "") == role_name
        ]
        if not matches:
            return False
        if role_name == "Role Based Access Control Administrator" and not any(
            _has_exact_delegation_condition(assignment, delegated_principal_id)
            for assignment in matches
        ):
            return False
    return True


def run(
    config: InstallerConfig,
    az: AzCli,
    sp_object_id: str = "",
    delegated_principal_id: str = "",
) -> dict:
    if not sp_object_id:
        raise ValueError("sp_object_id is required for RBAC step")
    try:
        delegated_principal_id = str(uuid.UUID(delegated_principal_id))
    except (ValueError, TypeError, AttributeError) as ex:
        raise ValueError("delegated_principal_id must be an exact user object GUID") from ex

    scope = _scope(config)
    results = {}
    existing = az.run(
        "role", "assignment", "list", "--assignee", sp_object_id, "--scope", scope
    )
    exact_existing = [
        assignment
        for assignment in existing
        if isinstance(assignment, dict)
        and assignment.get("scope", "").lower() == scope.lower()
    ] if isinstance(existing, list) else []

    for role_name, purpose in _ROLES:
        matches = [
            assignment
            for assignment in exact_existing
            if assignment.get("roleDefinitionName", "") == role_name
        ]
        if matches:
            if role_name == "Role Based Access Control Administrator" and not any(
                _has_exact_delegation_condition(assignment, delegated_principal_id)
                for assignment in matches
            ):
                raise RuntimeError(
                    "Existing RBAC Administrator assignment lacks the exact required condition; "
                    "refusing to accept unconstrained delegation"
                )
            warning(f"Role '{role_name}' already assigned — skipping")
            results[role_name] = "exists"
            continue

        args = [
            "role", "assignment", "create",
            "--assignee-object-id", sp_object_id,
            "--assignee-principal-type", "ServicePrincipal",
            "--role", role_name,
            "--scope", scope,
        ]
        if role_name == "Role Based Access Control Administrator":
            args.extend([
                "--condition", _rbac_condition(delegated_principal_id),
                "--condition-version", "2.0",
            ])
        assignment = az.run(*args)
        success(f"Role '{role_name}' assigned ({purpose})")
        results[role_name] = {
            "status": "assigned",
            "id": assignment.get("id", "") if isinstance(assignment, dict) else "",
        }

    assignment_ids = [
        value["id"]
        for value in results.values()
        if isinstance(value, dict)
        and value.get("status") == "assigned"
        and value.get("id")
    ]
    return {"roles": results, "assignment_ids": assignment_ids, "status": "configured"}


def teardown(
    config: InstallerConfig,
    az: AzCli,
    assignment_ids: list[str] | None = None,
) -> None:
    for assignment_id in assignment_ids or []:
        try:
            az.run("role", "assignment", "delete", "--ids", assignment_id)
            success(f"Role assignment '{assignment_id}' removed")
        except Exception:
            warning(f"Could not remove role assignment '{assignment_id}'")
            raise
