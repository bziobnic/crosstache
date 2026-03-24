import json
import logging
import os
import time
import traceback
import jwt
from datetime import datetime, timezone
from typing import Dict, Any, Optional, Tuple

import requests as http_requests
import azure.functions as func
from VaultRbacProcessor.vault_role_manager import VaultRoleManager
from StorageRoleManager.storage_role_manager import StorageRoleManager
from config import OWNER_ROLE_ID, KEY_VAULT_ADMINISTRATOR_ROLE_ID


# ---------------------------------------------------------------------------
# Azure AD JWT validation helpers
# ---------------------------------------------------------------------------

# Module-level JWKS cache: { "keys": [...], "fetched_at": <epoch>, "jwks_uri": <str> }
_jwks_cache: Dict[str, Any] = {}
_JWKS_CACHE_TTL_SECONDS = 3600  # 1 hour


def _get_tenant_id() -> Optional[str]:
    """Return the Azure AD tenant ID from environment variables."""
    return os.environ.get("AZURE_TENANT_ID") or os.environ.get("AZURE_AD_TENANT_ID")


def _get_expected_issuer(tenant_id: Optional[str]) -> Optional[str]:
    """Return the expected token issuer.

    Priority:
    1. AZURE_AD_ISSUER env var (explicit override)
    2. Constructed from AZURE_TENANT_ID (v2 endpoint)
    """
    explicit = os.environ.get("AZURE_AD_ISSUER")
    if explicit:
        return explicit
    if tenant_id:
        return f"https://sts.windows.net/{tenant_id}/"
    return None


def _get_openid_config_url(tenant_id: Optional[str]) -> str:
    """Return the OpenID Connect metadata URL for the tenant."""
    tid = tenant_id or "common"
    return f"https://login.microsoftonline.com/{tid}/v2.0/.well-known/openid-configuration"


def _fetch_jwks_uri(tenant_id: Optional[str]) -> str:
    """Fetch the JWKS URI from the OpenID Connect metadata endpoint."""
    url = _get_openid_config_url(tenant_id)
    resp = http_requests.get(url, timeout=10)
    resp.raise_for_status()
    return resp.json()["jwks_uri"]


def _fetch_signing_keys(jwks_uri: str) -> Dict[str, Any]:
    """Fetch the JSON Web Key Set from the JWKS URI."""
    resp = http_requests.get(jwks_uri, timeout=10)
    resp.raise_for_status()
    return resp.json()


def _get_signing_keys(force_refresh: bool = False) -> Tuple[Dict[str, Any], str]:
    """Return cached signing keys, refreshing if stale or forced.

    Returns (jwks_dict, jwks_uri).
    """
    global _jwks_cache

    now = time.time()
    cache_valid = (
        _jwks_cache
        and not force_refresh
        and (now - _jwks_cache.get("fetched_at", 0)) < _JWKS_CACHE_TTL_SECONDS
    )

    if cache_valid:
        return _jwks_cache["keys"], _jwks_cache["jwks_uri"]

    tenant_id = _get_tenant_id()
    jwks_uri = _fetch_jwks_uri(tenant_id)
    jwks_data = _fetch_signing_keys(jwks_uri)

    _jwks_cache = {
        "keys": jwks_data,
        "fetched_at": now,
        "jwks_uri": jwks_uri,
    }
    return jwks_data, jwks_uri


def _find_key_by_kid(jwks_data: Dict[str, Any], kid: str) -> Optional[Dict[str, Any]]:
    """Find a key in the JWKS by its key ID."""
    for key in jwks_data.get("keys", []):
        if key.get("kid") == kid:
            return key
    return None


def _validate_jwt(token: str) -> Dict[str, Any]:
    """Validate a JWT token against Azure AD public keys.

    Returns the decoded claims dict on success.
    Raises jwt.PyJWTError (or subclass) on any validation failure.
    """
    tenant_id = _get_tenant_id()
    expected_issuer = _get_expected_issuer(tenant_id)
    expected_audience = os.environ.get("EXPECTED_AUDIENCE")

    # Read unverified header to get kid and algorithm
    unverified_header = jwt.get_unverified_header(token)
    kid = unverified_header.get("kid")
    algorithm = unverified_header.get("alg", "RS256")

    if not kid:
        raise jwt.InvalidTokenError("Token header missing 'kid' claim")

    # Fetch signing keys (from cache if available)
    jwks_data, _jwks_uri = _get_signing_keys()

    key_data = _find_key_by_kid(jwks_data, kid)

    # If kid not found, force-refresh keys (handle key rotation)
    if key_data is None:
        logging.info(f"Key ID '{kid}' not found in cached JWKS, refreshing keys")
        jwks_data, _jwks_uri = _get_signing_keys(force_refresh=True)
        key_data = _find_key_by_kid(jwks_data, kid)

    if key_data is None:
        raise jwt.InvalidTokenError(f"Unable to find signing key with kid '{kid}'")

    # Build the public key from JWK
    public_key = jwt.algorithms.RSAAlgorithm.from_jwk(key_data)

    # Build decode options
    decode_options: Dict[str, Any] = {
        "verify_signature": True,
        "verify_exp": True,
        "verify_aud": bool(expected_audience),
        "verify_iss": bool(expected_issuer),
    }

    decode_kwargs: Dict[str, Any] = {
        "algorithms": [algorithm],
        "options": decode_options,
    }

    if expected_issuer:
        decode_kwargs["issuer"] = expected_issuer
    if expected_audience:
        decode_kwargs["audience"] = expected_audience

    decoded = jwt.decode(token, public_key, **decode_kwargs)
    return decoded


def _parse_bearer_token(auth_header: Optional[str]) -> Tuple[Optional[str], Optional[func.HttpResponse]]:
    """Parse and validate the Authorization header.

    Returns (token, None) on success or (None, error_response) on failure.
    """
    if not auth_header:
        return None, func.HttpResponse(
            json.dumps({"error": "Missing Authorization header"}),
            status_code=401,
            mimetype="application/json",
        )

    parts = auth_header.split()
    if len(parts) != 2:
        return None, func.HttpResponse(
            json.dumps({"error": "Malformed Authorization header: expected 'Bearer <token>'"}),
            status_code=401,
            mimetype="application/json",
        )

    scheme, token = parts
    if scheme.lower() != "bearer":
        return None, func.HttpResponse(
            json.dumps({"error": f"Unsupported authentication scheme '{scheme}': expected 'Bearer'"}),
            status_code=401,
            mimetype="application/json",
        )

    if not token:
        return None, func.HttpResponse(
            json.dumps({"error": "Empty bearer token"}),
            status_code=401,
            mimetype="application/json",
        )

    return token, None

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
        # Extract and validate bearer token from Authorization header
        auth_header = req.headers.get('Authorization')
        token, auth_error = _parse_bearer_token(auth_header)
        if auth_error:
            logging.error("Missing or malformed Authorization header")
            return auth_error

        logging.info("Bearer token extracted from Authorization header")

        # Validate the JWT signature and claims against Azure AD
        try:
            decoded_token = _validate_jwt(token)
            logging.info(
                "Token validated with signature verification. Claims: %s",
                json.dumps({k: v for k, v in decoded_token.items() if k not in ('aud', 'iss', 'sub')}),
            )

            # Extract user ID from token claims
            # Try different claim types that might contain the user's object ID
            user_id = decoded_token.get('oid') or decoded_token.get('sub')

            if not user_id:
                logging.error("User identity not found in token claims")
                return func.HttpResponse(
                    json.dumps({"error": "User identity not found in token"}),
                    status_code=401,
                    mimetype="application/json"
                )

            logging.info(f"User identified from token: {user_id}")

        except jwt.ExpiredSignatureError:
            logging.error("Token has expired")
            return func.HttpResponse(
                json.dumps({"error": "Token expired"}),
                status_code=401,
                mimetype="application/json"
            )
        except jwt.InvalidIssuerError as ex:
            logging.error(f"Token issuer validation failed: {ex}")
            return func.HttpResponse(
                json.dumps({"error": f"Invalid token issuer: {ex}"}),
                status_code=401,
                mimetype="application/json"
            )
        except jwt.InvalidAudienceError as ex:
            logging.error(f"Token audience validation failed: {ex}")
            return func.HttpResponse(
                json.dumps({"error": f"Invalid token audience: {ex}"}),
                status_code=401,
                mimetype="application/json"
            )
        except (jwt.InvalidTokenError, jwt.PyJWTError) as ex:
            logging.error(f"JWT validation failed: {ex}")
            return func.HttpResponse(
                json.dumps({"error": f"Invalid token: {ex}"}),
                status_code=401,
                mimetype="application/json"
            )
        except Exception as ex:
            logging.error(f"Unexpected error during token validation: {ex}")
            return func.HttpResponse(
                json.dumps({"error": f"Token validation error: {ex}"}),
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
        vault_info = await vault_manager.get_vault_info(resource_uri)
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
        
        # Build fully-qualified role definition IDs from centralized constants
        owner_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{OWNER_ROLE_ID}"
        admin_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{KEY_VAULT_ADMINISTRATOR_ROLE_ID}"
        
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
