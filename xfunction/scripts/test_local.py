#!/usr/bin/env python3
"""
Local testing script for the xfunction Azure Function.

Supports three modes:
  1. Unit tests only (no Azure credentials needed)
  2. Local function runtime test (needs func start running)
  3. Remote deployed function test

Usage:
  # Run unit tests
  python scripts/test_local.py unit

  # Test against local runtime (func start must be running)
  python scripts/test_local.py local \
    --vault-name myvault \
    --subscription-id <sub-id> \
    --resource-group Vaults

  # Test against deployed function
  python scripts/test_local.py remote \
    --url https://your-func.azurewebsites.net/api/assign-roles \
    --vault-name myvault \
    --subscription-id <sub-id> \
    --resource-group Vaults

  # Get a token for manual testing
  python scripts/test_local.py token
"""

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def run_unit_tests(verbose: bool = True) -> bool:
    """Run the unit test suite."""
    print("\n=== Running Unit Tests ===\n")
    cmd = [sys.executable, "-m", "pytest", "tests/", "-v" if verbose else "-q"]
    result = subprocess.run(cmd, cwd=Path(__file__).resolve().parent.parent)
    return result.returncode == 0


def get_az_token(resource: str = "https://management.azure.com") -> str | None:
    """Get an Azure AD token using az cli."""
    print(f"Acquiring token for resource: {resource}")
    try:
        result = subprocess.run(
            ["az", "account", "get-access-token", "--resource", resource, "--query", "accessToken", "-o", "tsv"],
            capture_output=True, text=True, timeout=30
        )
        if result.returncode != 0:
            print(f"Error: {result.stderr.strip()}")
            print("Make sure you're logged in: az login")
            return None
        token = result.stdout.strip()
        if not token:
            print("Error: Empty token returned")
            return None
        print(f"Token acquired (length: {len(token)} chars)")
        return token
    except FileNotFoundError:
        print("Error: az CLI not found. Install it: https://learn.microsoft.com/en-us/cli/azure/install-azure-cli")
        return None
    except subprocess.TimeoutExpired:
        print("Error: az CLI timed out")
        return None


def get_current_user_id() -> str | None:
    """Get the current user's Azure AD object ID."""
    try:
        result = subprocess.run(
            ["az", "ad", "signed-in-user", "show", "--query", "id", "-o", "tsv"],
            capture_output=True, text=True, timeout=30
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return None


def test_endpoint(url: str, token: str, payload: dict, label: str = "Test") -> dict | None:
    """Send a test request to the function endpoint and return the response."""
    import requests

    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json"
    }

    print(f"\n--- {label} ---")
    print(f"POST {url}")
    print(f"Payload: {json.dumps(payload, indent=2)}")

    try:
        start = time.time()
        response = requests.post(url, json=payload, headers=headers, timeout=60)
        elapsed = time.time() - start

        print(f"Status: {response.status_code} ({elapsed:.2f}s)")

        try:
            body = response.json()
            print(f"Response: {json.dumps(body, indent=2)}")
        except ValueError:
            body = {"raw": response.text}
            print(f"Response (non-JSON): {response.text[:500]}")

        # Evaluate result
        if response.status_code == 200:
            success = body.get("success", False)
            print(f"\n{'PASS' if success else 'PARTIAL'}: Owner={body.get('ownerRoleAssigned')}, Admin={body.get('adminRoleAssigned')}")
            storage = body.get("storageAccounts", {})
            if storage.get("discovered", 0) > 0:
                print(f"  Storage: {storage['discovered']} accounts, success={storage.get('success')}")
        elif response.status_code == 401:
            print("\nFAIL: Authentication failed - check your token and EXPECTED_AUDIENCE setting")
        elif response.status_code == 403:
            print(f"\nFAIL: Not authorized - user is not the vault creator")
            print(f"  Your ID: {body.get('userId')}, Creator ID: {body.get('creatorId')}")
        elif response.status_code == 404:
            print(f"\nFAIL: Vault not found - check resourceUri")
        else:
            print(f"\nFAIL: {body.get('error', 'Unknown error')}")

        return body

    except requests.exceptions.ConnectionError:
        print(f"\nFAIL: Connection refused. Is the function running at {url}?")
        return None
    except requests.exceptions.Timeout:
        print(f"\nFAIL: Request timed out after 60s")
        return None
    except Exception as ex:
        print(f"\nFAIL: {type(ex).__name__}: {ex}")
        return None


def test_health(url: str) -> bool:
    """Check if the function host is reachable."""
    import requests
    base = url.rsplit("/api/", 1)[0] if "/api/" in url else url
    try:
        resp = requests.get(base, timeout=5)
        print(f"Function host at {base}: status {resp.status_code}")
        return resp.status_code < 500
    except requests.exceptions.ConnectionError:
        print(f"Function host at {base}: not reachable")
        return False


def build_resource_uri(subscription_id: str, resource_group: str, vault_name: str) -> str:
    return f"/subscriptions/{subscription_id}/resourceGroups/{resource_group}/providers/Microsoft.KeyVault/vaults/{vault_name}"


def cmd_unit(args):
    success = run_unit_tests(verbose=True)
    sys.exit(0 if success else 1)


def cmd_token(args):
    token = get_az_token()
    if token:
        print(f"\n--- Bearer Token ---\n{token}\n")
        user_id = get_current_user_id()
        if user_id:
            print(f"Your Azure AD Object ID: {user_id}")
    else:
        sys.exit(1)


def cmd_local(args):
    url = f"http://localhost:{args.port}/api/assign-roles"

    print("=== Local Function Test ===")
    if not test_health(url):
        print("\nHint: Start the function with 'func start' in the xfunction directory")
        sys.exit(1)

    token = get_az_token()
    if not token:
        sys.exit(1)

    user_id = get_current_user_id()
    if user_id:
        print(f"Your Azure AD Object ID: {user_id}")

    resource_uri = build_resource_uri(args.subscription_id, args.resource_group, args.vault_name)

    payload = {
        "resourceUri": resource_uri,
        "subscriptionId": args.subscription_id
    }

    # Test 1: Normal request
    test_endpoint(url, token, payload, label="Normal RBAC Assignment")

    if args.run_negative:
        # Test 2: Missing fields
        print("\n")
        test_endpoint(url, token, {}, label="Missing Fields (expect 400)")

        # Test 3: Bad token
        print("\n")
        test_endpoint(url, "invalid-token-value", payload, label="Invalid Token (expect 401)")


def cmd_remote(args):
    url = args.url
    print(f"=== Remote Function Test ({url}) ===")

    token = get_az_token()
    if not token:
        sys.exit(1)

    resource_uri = build_resource_uri(args.subscription_id, args.resource_group, args.vault_name)

    payload = {
        "resourceUri": resource_uri,
        "subscriptionId": args.subscription_id
    }

    test_endpoint(url, token, payload, label="Remote RBAC Assignment")


def main():
    parser = argparse.ArgumentParser(
        description="xfunction local testing tool",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    # unit
    subparsers.add_parser("unit", help="Run unit tests")

    # token
    subparsers.add_parser("token", help="Get an Azure AD token for manual testing")

    # local
    local_parser = subparsers.add_parser("local", help="Test against local function runtime")
    local_parser.add_argument("--vault-name", required=True, help="Key Vault name")
    local_parser.add_argument("--subscription-id", required=True, help="Azure subscription ID")
    local_parser.add_argument("--resource-group", default="Vaults", help="Resource group (default: Vaults)")
    local_parser.add_argument("--port", type=int, default=7071, help="Local function port (default: 7071)")
    local_parser.add_argument("--run-negative", action="store_true", help="Also run negative test cases")

    # remote
    remote_parser = subparsers.add_parser("remote", help="Test against deployed function")
    remote_parser.add_argument("--url", required=True, help="Function endpoint URL")
    remote_parser.add_argument("--vault-name", required=True, help="Key Vault name")
    remote_parser.add_argument("--subscription-id", required=True, help="Azure subscription ID")
    remote_parser.add_argument("--resource-group", default="Vaults", help="Resource group (default: Vaults)")

    args = parser.parse_args()

    commands = {
        "unit": cmd_unit,
        "token": cmd_token,
        "local": cmd_local,
        "remote": cmd_remote,
    }
    commands[args.command](args)


if __name__ == "__main__":
    main()
