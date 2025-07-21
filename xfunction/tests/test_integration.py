import requests
import json
import os
import sys
import argparse
import msal
from datetime import datetime


def get_azure_token(tenant_id, client_id, client_secret, scope):
    """Get an Azure AD token using MSAL."""
    app = msal.ConfidentialClientApplication(
        client_id=client_id,
        client_credential=client_secret,
        authority=f"https://login.microsoftonline.com/{tenant_id}"
    )
    
    result = app.acquire_token_for_client(scopes=[scope])
    
    if "access_token" in result:
        return result["access_token"]
    else:
        print(f"Error getting token: {result.get('error')}, {result.get('error_description')}")
        return None


def test_direct_rbac_assignment(function_url, token, subscription_id, vault_name):
    """Test the direct RBAC assignment endpoint with a real request."""
    # Construct the resource URI for the vault
    resource_uri = f"/subscriptions/{subscription_id}/resourceGroups/Vaults/providers/Microsoft.KeyVault/vaults/{vault_name}"
    
    # Prepare request payload
    payload = {
        "resourceUri": resource_uri,
        "subscriptionId": subscription_id
    }
    
    # Set headers with token
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json"
    }
    
    # Make the request
    print(f"\nSending request to {function_url}")
    print(f"Resource URI: {resource_uri}")
    print(f"Subscription ID: {subscription_id}")
    
    try:
        response = requests.post(function_url, json=payload, headers=headers)
        
        # Print response details
        print(f"\nResponse Status Code: {response.status_code}")
        print(f"Response Headers: {response.headers}")
        
        try:
            response_json = response.json()
            print(f"Response Body: {json.dumps(response_json, indent=2)}")
            
            # Check if the request was successful
            if response.status_code == 200 and response_json.get("success"):
                print("\n✅ Test PASSED: Role assignment successful")
                return True
            else:
                print("\n❌ Test FAILED: Role assignment failed")
                return False
        except ValueError:
            print(f"Response Body (not JSON): {response.text}")
            print("\n❌ Test FAILED: Response is not valid JSON")
            return False
            
    except Exception as e:
        print(f"\n❌ Test FAILED: Exception occurred: {str(e)}")
        return False


def main():
    """Main function to run the integration test."""
    parser = argparse.ArgumentParser(description="Test the direct RBAC assignment function")
    parser.add_argument("--url", default="https://fa-user-keyvault.azurewebsites.net/api/api/assign-roles", 
                        help="URL of the function app endpoint")
    parser.add_argument("--tenant-id", required=True, help="Azure AD tenant ID")
    parser.add_argument("--client-id", required=True, help="Azure AD client ID")
    parser.add_argument("--client-secret", required=True, help="Azure AD client secret")
    parser.add_argument("--subscription-id", required=True, help="Azure subscription ID")
    parser.add_argument("--vault-name", required=True, help="Name of the Key Vault to assign roles to")
    parser.add_argument("--scope", default="https://management.azure.com/.default", 
                        help="Token scope")
    
    args = parser.parse_args()
    
    # Get token
    print(f"Getting token for tenant {args.tenant_id} and client {args.client_id}")
    token = get_azure_token(args.tenant_id, args.client_id, args.client_secret, args.scope)
    
    if not token:
        print("Failed to get token. Exiting.")
        sys.exit(1)
    
    print("Token acquired successfully")
    
    # Run the test
    success = test_direct_rbac_assignment(
        args.url, token, args.subscription_id, args.vault_name
    )
    
    # Exit with appropriate code
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
