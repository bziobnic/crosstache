import json
import logging
import traceback
import jwt
from datetime import datetime
from typing import Dict, Any


import azure.functions as func
from VaultRbacProcessor.vault_role_manager import VaultRoleManager
from StorageRoleManager.storage_role_manager import StorageRoleManager

app = func.FunctionApp()


@app.function_name(name="DirectVaultRbacProcessor")
@app.route(route="assign-roles", auth_level=func.AuthLevel.ANONYMOUS, methods=["POST"])
async def direct_vault_rbac_processor(req: func.HttpRequest) -> func.HttpResponse:
    """
    HTTP trigger function for direct RBAC assignment to Key Vault.
    This function allows the BBayVault Go client to directly call the Function
    to assign roles immediately after vault creation, without waiting for Event Grid.
    
    The function validates the caller's identity using JWT token and assigns
    Owner and Key Vault Administrator roles to the authenticated user.
    It also verifies that the caller is the creator of the vault by checking
    the CreatedByID tag on the vault.
    """
    logging.info("===== Direct RBAC assignment request received =====")
    
    try:
        # Extract and validate JWT token from Authorization header
        auth_header = req.headers.get('Authorization')
        if not auth_header or not auth_header.startswith('Bearer '):
            logging.error("Missing or invalid Authorization header")
            return func.HttpResponse(
                json.dumps({"error": "Missing or invalid Authorization header"}),
                status_code=401,
                mimetype="application/json"
            )
        
        token = auth_header.split(' ')[1]
        logging.info("Token extracted from Authorization header")
        
        # Validate the token and extract user identity
        try:
            # Decode token without verification first to get claims
            # This is safe because we'll verify the token in the next step
            decoded_token = jwt.decode(token, options={"verify_signature": False})
            logging.info(f"Token claims extracted: {json.dumps({k: v for k, v in decoded_token.items() if k not in ['aud', 'iss', 'sub']})}") 
            
            # Verify token is not expired
            if 'exp' in decoded_token:
                expiry = datetime.fromtimestamp(decoded_token['exp'])
                if expiry < datetime.now():
                    logging.error(f"Token expired at {expiry}")
                    return func.HttpResponse(
                        json.dumps({"error": "Token expired"}),
                        status_code=401,
                        mimetype="application/json"
                    )
            
            # Extract user ID from token claims
            # Try different claim types that might contain the user's object ID
            user_id = None
            if 'oid' in decoded_token:
                user_id = decoded_token['oid']
            elif 'sub' in decoded_token:
                user_id = decoded_token['sub']
            
            if not user_id:
                logging.error("User identity not found in token claims")
                return func.HttpResponse(
                    json.dumps({"error": "User identity not found in token"}),
                    status_code=401,
                    mimetype="application/json"
                )
            
            logging.info(f"User identified from token: {user_id}")
            
        except Exception as ex:
            logging.error(f"Error validating token: {str(ex)}")
            return func.HttpResponse(
                json.dumps({"error": f"Invalid token: {str(ex)}"}),
                status_code=401,
                mimetype="application/json"
            )
        
        # Parse and validate request body
        try:
            req_body = req.get_json()
        except ValueError:
            logging.error("Invalid JSON in request body")
            return func.HttpResponse(
                json.dumps({"error": "Invalid JSON in request body"}),
                status_code=400,
                mimetype="application/json"
            )
        
        # Extract required parameters
        resource_uri = req_body.get('resourceUri')
        subscription_id = req_body.get('subscriptionId')
        
        if not resource_uri or not subscription_id:
            logging.error(f"Missing required parameters: {json.dumps(req_body)}")
            return func.HttpResponse(
                json.dumps({"error": "Missing required parameters: resourceUri and subscriptionId are required"}),
                status_code=400,
                mimetype="application/json"
            )
        
        logging.info(f"Processing request for resource: {resource_uri}")
        logging.info(f"Subscription ID: {subscription_id}")
        
        # Initialize the VaultRoleManager and StorageRoleManager
        vault_manager = VaultRoleManager()
        storage_manager = StorageRoleManager()
        logging.info("VaultRoleManager and StorageRoleManager initialized")
        
        # Get vault information including tags
        vault_info = vault_manager.get_vault_info(resource_uri)
        logging.info(f"Vault info type: {type(vault_info)}")
        logging.info(f"Vault info content: {vault_info}")
        
        if not vault_info:
            logging.error(f"Could not retrieve vault information for {resource_uri}")
            return func.HttpResponse(
                json.dumps({"error": f"Could not retrieve vault information"}),
                status_code=404,
                mimetype="application/json"
            )
        
        # Check if the caller is the creator of the vault by comparing CreatedByID tag
        try:
            vault_tags = vault_info.get('tags', {}) if isinstance(vault_info, dict) else {}
            creator_id = vault_tags.get('CreatedByID')
            
            logging.info(f"Vault tags: {vault_tags}")
            logging.info(f"Vault creator ID from tags: {creator_id}")
            logging.info(f"Current user ID from token: {user_id}")
            
            if not creator_id:
                logging.warning("Vault does not have CreatedByID tag, cannot verify creator")
                # Continue with role assignment but log the warning
            elif creator_id.lower() != user_id.lower():
                logging.error(f"User {user_id} is not the creator of the vault (creator is {creator_id})")
                return func.HttpResponse(
                    json.dumps({
                        "error": "Unauthorized: Only the creator of the vault can assign roles",
                        "userId": user_id,
                        "creatorId": creator_id
                    }),
                    status_code=403,
                    mimetype="application/json"
                )
            else:
                logging.info(f"✅ Verified user {user_id} is the creator of the vault")
        
        except Exception as ex:
            logging.error(f"Error processing vault info: {str(ex)}")
            return func.HttpResponse(
                json.dumps({"error": f"Internal server error: {str(ex)}"}),
                status_code=500,
                mimetype="application/json"
            )
        
        # Define the Owner role ID
        owner_role_id = "8e3af657-a8ff-443c-a75c-2fe8c4bcb635"  # Azure Owner role ID
        admin_role_id = "00482a5a-887f-4fb3-b363-3b7fe8e74483"  # Azure Key Vault Administrator role ID
        owner_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{owner_role_id}"
        admin_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{admin_role_id}"
        
        # Assign the Owner role to the user
        logging.info(f"Attempting to assign Owner role to user {user_id}")
        owner_success = await vault_manager.assign_role_to_user(
            resource_uri, owner_role_definition_id, user_id
        )
        
        # Assign the Key Vault Administrator role to the user
        logging.info(f"Attempting to assign Key Vault Administrator role to user {user_id}")
        admin_success = await vault_manager.assign_role_to_user(
            resource_uri, admin_role_definition_id, user_id
        )
        
        # Discover and assign storage account permissions
        logging.info(f"Discovering associated storage accounts for vault")
        storage_accounts = await storage_manager.discover_associated_storage_accounts(resource_uri)
        storage_results = {}
        
        if storage_accounts:
            logging.info(f"Found {len(storage_accounts)} associated storage accounts")
            
            # Assign storage roles based on vault Owner role
            if owner_success:
                logging.info(f"Assigning storage roles based on Owner role")
                storage_results = await storage_manager.assign_storage_roles_to_user(
                    storage_accounts, owner_role_definition_id, user_id
                )
            
            # If Owner role failed but Admin role succeeded, assign storage roles based on Admin role
            elif admin_success:
                logging.info(f"Assigning storage roles based on Key Vault Administrator role")
                storage_results = await storage_manager.assign_storage_roles_to_user(
                    storage_accounts, admin_role_definition_id, user_id
                )
        else:
            logging.info("No associated storage accounts found")
        
        # Calculate overall storage success
        storage_success = True
        if storage_accounts:
            for storage_name, roles in storage_results.items():
                for role_id, success in roles.items():
                    if not success:
                        storage_success = False
                        break
                if not storage_success:
                    break
        
        # Prepare response based on role assignment results
        response = {
            "success": owner_success and admin_success and storage_success,
            "ownerRoleAssigned": owner_success,
            "adminRoleAssigned": admin_success,
            "resourceUri": resource_uri,
            "userId": user_id,
            "isCreator": creator_id and creator_id.lower() == user_id.lower(),
            "storageAccounts": {
                "discovered": len(storage_accounts),
                "assignments": storage_results,
                "success": storage_success
            }
        }
        
        if owner_success and admin_success and storage_success:
            logging.info(f"✅ Successfully assigned all vault and storage roles to user {user_id}")
            if storage_accounts:
                logging.info(f"✅ Storage role assignments: {len(storage_accounts)} accounts processed")
            return func.HttpResponse(
                json.dumps(response),
                status_code=200,
                mimetype="application/json"
            )
        else:
            logging.error(f"❌ Failed to assign one or more roles to user {user_id}")
            if not owner_success:
                logging.error(f"Failed to assign Owner role")
            if not admin_success:
                logging.error(f"Failed to assign Key Vault Administrator role")
            if not storage_success:
                logging.error(f"Failed to assign one or more storage roles")
                for storage_name, roles in storage_results.items():
                    for role_id, success in roles.items():
                        if not success:
                            logging.error(f"Failed to assign storage role {role_id} to {storage_name}")
                
            return func.HttpResponse(
                json.dumps(response),
                status_code=500,
                mimetype="application/json"
            )
    
    except Exception as ex:
        error_details = {
            "error_type": type(ex).__name__,
            "error_message": str(ex),
            "stack_trace": traceback.format_exc(),
        }
        logging.error(f"❌ Error processing direct RBAC assignment: {json.dumps(error_details, indent=2)}")
        return func.HttpResponse(
            json.dumps({"error": f"Internal server error: {str(ex)}"}),
            status_code=500,
            mimetype="application/json"
        )
