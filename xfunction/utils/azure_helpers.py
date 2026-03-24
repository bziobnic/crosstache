"""Shared Azure helper utilities for RBAC role management.

Provides principal resolution, GUID handling, retry logic, and Graph API
integration used by both VaultRoleManager and StorageRoleManager.
"""

import asyncio
import functools
import logging
import uuid
from typing import Optional

import requests
from azure.identity import ClientSecretCredential
from azure.core.exceptions import HttpResponseError

# Default timeout for HTTP requests to Azure/Graph APIs (seconds)
HTTP_TIMEOUT = 30

# Retry configuration
MAX_RETRIES = 3
RETRY_BASE_DELAY = 1.0  # seconds
RETRY_MAX_DELAY = 30.0  # seconds


def _is_retryable(ex: Exception) -> bool:
    """Determine if an exception is retryable (transient)."""
    if isinstance(ex, HttpResponseError):
        status = getattr(ex, 'status_code', None)
        # 429 = throttled, 5xx = server errors (excluding 501 Not Implemented)
        if status == 429 or (status is not None and 500 <= status <= 599 and status != 501):
            return True
    if isinstance(ex, (requests.exceptions.ConnectionError, requests.exceptions.Timeout)):
        return True
    return False


def _get_retry_after(ex: Exception) -> Optional[float]:
    """Extract Retry-After header value from an Azure error if present."""
    if isinstance(ex, HttpResponseError):
        response = getattr(ex, 'response', None)
        if response is not None:
            retry_after = getattr(response, 'headers', {}).get('Retry-After')
            if retry_after:
                try:
                    return float(retry_after)
                except (ValueError, TypeError):
                    pass
    return None


def retry_async(func):
    """Decorator that adds exponential backoff retry to async functions.

    Retries on Azure 429 (throttled) and transient 5xx errors.
    Respects Retry-After headers from Azure API responses.
    """
    @functools.wraps(func)
    async def wrapper(*args, **kwargs):
        last_exception = None
        for attempt in range(MAX_RETRIES + 1):
            try:
                return await func(*args, **kwargs)
            except Exception as ex:
                last_exception = ex
                if attempt == MAX_RETRIES or not _is_retryable(ex):
                    raise
                # Calculate delay: use Retry-After header if available, else exponential backoff
                retry_after = _get_retry_after(ex)
                delay = retry_after if retry_after else min(RETRY_BASE_DELAY * (2 ** attempt), RETRY_MAX_DELAY)
                logging.warning(f"Retryable error on attempt {attempt + 1}/{MAX_RETRIES + 1}, retrying in {delay:.1f}s: {ex}")
                await asyncio.sleep(delay)
        raise last_exception
    return wrapper


def is_guid(value: str) -> bool:
    """Check if a string is a valid GUID."""
    try:
        uuid.UUID(value)
        return True
    except (ValueError, AttributeError, TypeError):
        return False


def normalize_guid(guid_str: str) -> str:
    """Ensure GUID is properly formatted with hyphens."""
    if not guid_str:
        return guid_str

    # Remove all hyphens first
    clean = guid_str.replace('-', '')

    # If it's a valid GUID length (32 chars without hyphens), format it properly
    if len(clean) == 32:
        return f"{clean[0:8]}-{clean[8:12]}-{clean[12:16]}-{clean[16:20]}-{clean[20:32]}"

    return guid_str


def _get_graph_headers(credential: ClientSecretCredential) -> dict:
    """Get authorization headers for Microsoft Graph API calls."""
    token = credential.get_token("https://graph.microsoft.com/.default")
    return {
        'Authorization': f'Bearer {token.token}',
        'Content-Type': 'application/json'
    }


async def detect_principal_type(credential: ClientSecretCredential, principal_id: str) -> str:
    """Detect the principal type (User, ServicePrincipal, Group) using Graph API.

    Uses the directoryObjects endpoint for a single API call instead of
    three sequential lookups.

    :param credential: Azure credential for Graph API access
    :param principal_id: The principal's object ID
    :return: One of "User", "ServicePrincipal", "Group"
    """
    try:
        headers = _get_graph_headers(credential)

        # Use directoryObjects endpoint — single call instead of 3
        url = f"https://graph.microsoft.com/v1.0/directoryObjects/{principal_id}"
        response = requests.get(url, headers=headers, timeout=HTTP_TIMEOUT)

        if response.status_code == 200:
            data = response.json()
            odata_type = data.get("@odata.type", "")
            if "user" in odata_type.lower():
                return "User"
            elif "servicePrincipal" in odata_type:
                return "ServicePrincipal"
            elif "group" in odata_type.lower():
                return "Group"

        # Fallback: try individual endpoints if directoryObjects didn't work
        for entity_type, path in [("User", "users"), ("ServicePrincipal", "servicePrincipals"), ("Group", "groups")]:
            url = f"https://graph.microsoft.com/v1.0/{path}/{principal_id}"
            response = requests.get(url, headers=headers, timeout=HTTP_TIMEOUT)
            if response.status_code == 200:
                return entity_type

        return "ServicePrincipal"  # Default fallback
    except Exception as ex:
        logging.warning(f"Error detecting principal type: {str(ex)}")
        return "ServicePrincipal"


async def get_principal_id_for_user(credential: ClientSecretCredential, user_upn: str) -> Optional[str]:
    """Get the object ID for a user using Microsoft Graph API.

    :param credential: Azure credential for Graph API access
    :param user_upn: The user principal name (email address)
    :return: The object ID if found, None otherwise
    """
    try:
        headers = _get_graph_headers(credential)
        url = f"https://graph.microsoft.com/v1.0/users?$filter=userPrincipalName eq '{user_upn}'"

        logging.info(f"Calling Microsoft Graph API to find user: {user_upn}")
        response = requests.get(url, headers=headers, timeout=HTTP_TIMEOUT)

        if response.status_code == 200:
            user_data = response.json()
            if 'value' in user_data and len(user_data['value']) > 0:
                user_id = user_data['value'][0]['id']
                logging.info(f"Found user object ID: {user_id}")
                return user_id
            else:
                logging.warning(f"No users found with UPN: {user_upn}")
        else:
            logging.warning(f"Graph API returned status code: {response.status_code}")

        return None

    except Exception as ex:
        logging.error(f"Error getting principal ID: {str(ex)}")
        return None
