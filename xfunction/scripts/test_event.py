#!/usr/bin/env python

import argparse
import json
import uuid
from datetime import datetime

import requests


def create_sample_event(vault_name, resource_group="Vaults", principal_id=None, subscription_id=None):
    """
    Create a sample Event Grid event for testing.
    
    Args:
        vault_name: Name of the vault
        resource_group: Resource group name (default: Vaults)
        principal_id: Principal ID of the creator (if None, this must be set via arg)
        subscription_id: Subscription ID (if None, this must be set via arg)
    
    Returns:
        A dictionary representing an Event Grid event
    """
    if not subscription_id:
        print("Error: subscription_id must be provided")
        return None
        
    if not principal_id:
        print("Error: principal_id must be provided")
        return None
    
    # Create resource URI for the Key Vault
    resource_uri = f"/subscriptions/{subscription_id}/resourceGroups/{resource_group}/providers/Microsoft.KeyVault/vaults/{vault_name}"
    
    # Create event ID
    event_id = str(uuid.uuid4())
    
    # Current timestamp
    timestamp = datetime.utcnow().isoformat() + "Z"
    
    # Create the event
    event = {
        "id": event_id,
        "subject": resource_uri,
        "data": {
            "authorization": {
                "evidence": {
                    "principalId": principal_id
                }
            },
            "claims": {
                "http://schemas.microsoft.com/identity/claims/objectidentifier": principal_id
            },
            "operationName": "Microsoft.KeyVault/vaults/write",
            "resourceUri": resource_uri,
            "resourceProvider": "Microsoft.KeyVault",
            "resourceType": "Microsoft.KeyVault/vaults",
            "resourceId": resource_uri
        },
        "eventType": "Microsoft.Resources.ResourceWriteSuccess",
        "eventTime": timestamp,
        "dataVersion": "1.0"
    }
    
    return event

def send_event_to_local_function(event, port=7071):
    """
    Send event to local Azure Function for testing.
    
    Args:
        event: Event Grid event
        port: Port of local function app
    
    Returns:
        Response from the Function App
    """
    url = f"http://localhost:{port}/runtime/webhooks/eventgrid?functionName=VaultRbacProcessor"
    
    # Create a fake event grid notification (which is a list of events)
    event_grid_notification = [event]
    
    headers = {
        "Content-Type": "application/json",
        "aeg-event-type": "Notification"
    }
    
    print(f"Sending test event to: {url}")
    print(f"Event data: {json.dumps(event, indent=2)}")
    
    try:
        response = requests.post(url, json=event_grid_notification, headers=headers)
        return response
    except Exception as e:
        print(f"Error sending event: {str(e)}")
        return None

def main():
    parser = argparse.ArgumentParser(description="Test Event Grid trigger for Vault RBAC Processor")
    parser.add_argument("--vault-name", required=True, help="Name of the vault")
    parser.add_argument("--resource-group", default="Vaults", help="Resource group name")
    parser.add_argument("--principal-id", required=True, help="Principal ID of the creator")
    parser.add_argument("--subscription-id", required=True, help="Subscription ID")
    parser.add_argument("--port", type=int, default=7071, help="Port of local function app")
    parser.add_argument("--save", action="store_true", help="Save event to file instead of sending")
    
    args = parser.parse_args()
    
    # Create sample event
    event = create_sample_event(
        vault_name=args.vault_name,
        resource_group=args.resource_group,
        principal_id=args.principal_id,
        subscription_id=args.subscription_id
    )
    
    if not event:
        return
    
    if args.save:
        # Save to file
        filename = f"event_{args.vault_name}_{datetime.now().strftime('%Y%m%d%H%M%S')}.json"
        with open(filename, "w") as f:
            json.dump(event, f, indent=2)
        print(f"Saved event to {filename}")
    else:
        # Send to local function
        response = send_event_to_local_function(event, args.port)
        if response:
            print(f"Response status: {response.status_code}")
            print(f"Response: {response.text}")

if __name__ == "__main__":
    main() if __name__ == "__main__":
    main() 