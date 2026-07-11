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

# Module-level JWKS cache: { "keys": [...], "fetched_at": <epoch>, "jwks_uri": <str>, "issuer": <str> }
_jwks_cache: Dict[str, Any] = {}
_JWKS_CACHE_TTL_SECONDS = 3600  # 1 hour


def _get_tenant_id() -> Optional[str]:
    """Return the Azure AD tenant ID from environment variables."""
    return os.environ.get("AZURE_TENANT_ID") or os.environ.get("AZURE_AD_TENANT_ID")


def _get_expected_issuer(tenant_id: Optional[str]) -> Optional[str]:
    """Return the fallback expected token issuer (v1 endpoint).

    This is used only when the OIDC discovery document's own "issuer" field
    is unavailable (e.g. the metadata endpoint could not be reached). Azure
    AD v1 tokens use the `sts.windows.net` issuer form; v2 tokens use
    `login.microsoftonline.com/{tenant}/v2.0`. Since discovery normally
    supplies the concrete (and correct) issuer for whichever token version
    is actually presented, this v1 constant is only a best-effort offline
    fallback and does not by itself support v2 tokens.

    Priority:
    1. AZURE_AD_ISSUER env var (explicit override)
    2. Constructed from AZURE_TENANT_ID (v1 endpoint, offline fallback)
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


def _fetch_oidc_metadata(tenant_id: Optional[str]) -> Dict[str, Any]:
    """Fetch the OpenID Connect discovery document for the tenant."""
    url = _get_openid_config_url(tenant_id)
    resp = http_requests.get(url, timeout=10)
    resp.raise_for_status()
    return resp.json()


def _resolve_issuer(issuer: Optional[str], tenant_id: Optional[str]) -> Optional[str]:
    """Resolve a discovery-document issuer, substituting any {tenantid} template.

    The `common`/`organizations` discovery endpoints return a templated
    issuer such as `https://login.microsoftonline.com/{tenantid}/v2.0`; the
    tenant-specific endpoint returns a concrete issuer already. Substitute
    the template when present so both cases yield a usable value.
    """
    if not issuer:
        return None
    if "{tenantid}" in issuer and tenant_id:
        return issuer.replace("{tenantid}", tenant_id)
    return issuer


def _fetch_signing_keys(jwks_uri: str) -> Dict[str, Any]:
    """Fetch the JSON Web Key Set from the JWKS URI."""
    resp = http_requests.get(jwks_uri, timeout=10)
    resp.raise_for_status()
    return resp.json()


def _get_signing_keys(force_refresh: bool = False) -> Tuple[Dict[str, Any], str, Optional[str]]:
    """Return cached signing keys and discovery issuer, refreshing if stale or forced.

    The OIDC discovery document is fetched once alongside the JWKS URI (same
    cache, same TTL) so the issuer used for token validation always reflects
    metadata that was retrieved together, and discovery is not re-fetched
    per request any more than the JWKS themselves are.

    Returns (jwks_dict, jwks_uri, issuer).
    """
    global _jwks_cache

    now = time.time()
    cache_valid = (
        _jwks_cache
        and not force_refresh
        and (now - _jwks_cache.get("fetched_at", 0)) < _JWKS_CACHE_TTL_SECONDS
    )

    if cache_valid:
        return _jwks_cache["keys"], _jwks_cache["jwks_uri"], _jwks_cache.get("issuer")

    tenant_id = _get_tenant_id()
    metadata = _fetch_oidc_metadata(tenant_id)
    jwks_uri = metadata["jwks_uri"]
    issuer = _resolve_issuer(metadata.get("issuer"), tenant_id)
    jwks_data = _fetch_signing_keys(jwks_uri)

    _jwks_cache = {
        "keys": jwks_data,
        "fetched_at": now,
        "jwks_uri": jwks_uri,
        "issuer": issuer,
    }
    return jwks_data, jwks_uri, issuer


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
    explicit_issuer = os.environ.get("AZURE_AD_ISSUER")
    expected_audience = os.environ.get("EXPECTED_AUDIENCE")

    # Fail closed on missing validation configuration: a token minted for any
    # audience (or any tenant, if the issuer is also unknown) must never be
    # accepted by this privileged role-granting endpoint.
    if not expected_audience:
        raise jwt.InvalidTokenError(
            "Server misconfiguration: EXPECTED_AUDIENCE is not set; "
            "refusing to validate tokens without audience verification"
        )
    if not explicit_issuer and not tenant_id:
        raise jwt.InvalidTokenError(
            "Server misconfiguration: AZURE_TENANT_ID / AZURE_AD_ISSUER is not set; "
            "refusing to validate tokens without issuer verification"
        )

    # Read unverified header to get kid and algorithm
    unverified_header = jwt.get_unverified_header(token)
    kid = unverified_header.get("kid")
    algorithm = unverified_header.get("alg", "RS256")

    if not kid:
        raise jwt.InvalidTokenError("Token header missing 'kid' claim")

    # Fetch signing keys (from cache if available). The OIDC discovery
    # document's "issuer" field is fetched and cached alongside the JWKS URI
    # so the expected issuer tracks whichever token version (v1 or v2) the
    # tenant's discovery endpoint actually reports.
    jwks_data, _jwks_uri, discovery_issuer = _get_signing_keys()

    key_data = _find_key_by_kid(jwks_data, kid)

    # If kid not found, force-refresh keys (handle key rotation)
    if key_data is None:
        logging.info(f"Key ID '{kid}' not found in cached JWKS, refreshing keys")
        jwks_data, _jwks_uri, discovery_issuer = _get_signing_keys(force_refresh=True)
        key_data = _find_key_by_kid(jwks_data, kid)

    if key_data is None:
        raise jwt.InvalidTokenError(f"Unable to find signing key with kid '{kid}'")

    # Prefer an explicit override, then the issuer reported by discovery,
    # then fall back to the constructed v1 issuer if discovery didn't
    # provide one (e.g. metadata unavailable or missing the field).
    expected_issuer = explicit_issuer or discovery_issuer or _get_expected_issuer(tenant_id)
    if not expected_issuer:
        raise jwt.InvalidTokenError(
            "Server misconfiguration: unable to determine expected token issuer; "
            "refusing to validate tokens without issuer verification"
        )

    # Build the public key from JWK
    public_key = jwt.algorithms.RSAAlgorithm.from_jwk(key_data)

    # Build decode options
    decode_options: Dict[str, Any] = {
        "verify_signature": True,
        "verify_exp": True,
        "verify_aud": True,
        "verify_iss": True,
    }

    decode_kwargs: Dict[str, Any] = {
        "algorithms": [algorithm],
        "options": decode_options,
        "issuer": expected_issuer,
        "audience": expected_audience,
    }

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
            logging.info("Token validated with signature, issuer, and audience verification")

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

            allowed_principal_id = os.environ.get("ALLOWED_PRINCIPAL_ID", "")
            if not allowed_principal_id or user_id.lower() != allowed_principal_id.lower():
                logging.error("Authenticated caller is outside the delegated principal boundary")
                return func.HttpResponse(
                    json.dumps({"error": "Unauthorized: caller is outside the delegated boundary"}),
                    status_code=403,
                    mimetype="application/json",
                )

            logging.info("Authenticated caller identity resolved")

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
            logging.error("Missing required request parameters")
            return func.HttpResponse(
                json.dumps({"error": "Missing required parameters: resourceUri and subscriptionId are required"}),
                status_code=400,
                mimetype="application/json"
            )

        allowed_resource_group = os.environ.get("ALLOWED_RESOURCE_GROUP_ID", "").rstrip('/')
        allowed_prefix = f"{allowed_resource_group}/providers/Microsoft.KeyVault/vaults/"
        vault_segment = resource_uri[len(allowed_prefix):] if resource_uri.lower().startswith(allowed_prefix.lower()) else ""
        allowed_parts = allowed_resource_group.split('/')
        allowed_subscription = allowed_parts[2] if len(allowed_parts) > 2 else ""
        if (
            not allowed_resource_group
            or not vault_segment
            or '/' in vault_segment
            or subscription_id.lower() != allowed_subscription.lower()
        ):
            logging.error("Requested vault is outside the configured resource group boundary")
            return func.HttpResponse(
                json.dumps({"error": "Requested resource is outside the allowed resource group"}),
                status_code=403,
                mimetype="application/json"
            )
        
        logging.info("Processing role request for validated subscription resource")
        
        # Initialize the VaultRoleManager and StorageRoleManager
        vault_manager = VaultRoleManager()
        storage_manager = StorageRoleManager()
        logging.info("VaultRoleManager and StorageRoleManager initialized")
        
        # Get vault information including tags
        vault_info = await vault_manager.get_vault_info(resource_uri)
        
        if not vault_info:
            logging.error("Could not retrieve vault information for the validated resource")
            return func.HttpResponse(
                json.dumps({"error": f"Could not retrieve vault information"}),
                status_code=404,
                mimetype="application/json"
            )
        
        # Check if the caller is the creator of the vault by comparing CreatedByID tag
        try:
            vault_tags = vault_info.get('tags', {}) if isinstance(vault_info, dict) else {}
            creator_id = vault_tags.get('CreatedByID')
            
            if not creator_id:
                logging.error(
                    "Vault does not have a CreatedByID tag; refusing role assignment "
                    "because the creator cannot be verified"
                )
                return func.HttpResponse(
                    json.dumps({
                        "error": "Unauthorized: vault creator cannot be verified"
                    }),
                    status_code=403,
                    mimetype="application/json"
                )
            elif creator_id.lower() != user_id.lower():
                logging.error("Authenticated caller does not match the vault creator marker")
                return func.HttpResponse(
                    json.dumps({
                        "error": "Unauthorized: caller is not the verified vault creator"
                    }),
                    status_code=403,
                    mimetype="application/json"
                )
            else:
                logging.info("Vault creator marker matched the authenticated caller")
        
        except Exception as ex:
            logging.error(f"Error processing vault info: {str(ex)}")
            return func.HttpResponse(
                json.dumps({"error": f"Internal server error: {str(ex)}"}),
                status_code=500,
                mimetype="application/json"
            )

        if not await vault_manager.caller_has_rbac_authority(resource_uri, user_id):
            logging.error("Caller lacks pre-existing RBAC delegation authority")
            return func.HttpResponse(
                json.dumps({
                    "error": "Unauthorized: caller lacks existing RBAC delegation authority"
                }),
                status_code=403,
                mimetype="application/json"
            )
        
        # Build fully-qualified role definition IDs from centralized constants
        owner_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{OWNER_ROLE_ID}"
        admin_role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{KEY_VAULT_ADMINISTRATOR_ROLE_ID}"
        
        # Assign the Owner role to the user
        logging.info("Attempting to assign Owner role to authenticated caller")
        owner_success = await vault_manager.assign_role_to_user(
            resource_uri, owner_role_definition_id, user_id
        )
        
        # Assign the Key Vault Administrator role to the user
        logging.info("Attempting to assign Key Vault Administrator role to authenticated caller")
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
            "isCreator": creator_id and creator_id.lower() == user_id.lower(),
            "storageAccounts": {
                "discovered": len(storage_accounts),
                "assignments": storage_results,
                "success": storage_success
            }
        }
        
        if owner_success and admin_success and storage_success:
            logging.info("Successfully assigned all requested vault and storage roles")
            if storage_accounts:
                logging.info(f"✅ Storage role assignments: {len(storage_accounts)} accounts processed")
            return func.HttpResponse(
                json.dumps(response),
                status_code=200,
                mimetype="application/json"
            )
        else:
            logging.error("Failed to assign one or more requested roles")
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
