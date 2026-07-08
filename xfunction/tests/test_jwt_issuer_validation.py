"""Tests exercising the REAL `_validate_jwt` issuer verification path.

Unlike test_direct_rbac_processor.py (which mocks `_validate_jwt` entirely),
these tests only mock the HTTP calls used to fetch the OIDC discovery
document and JWKS, sign real JWTs with a generated RSA key, and let the
actual signature/issuer/audience validation in `_validate_jwt` run.
"""
import copy
import os
import sys
import time
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import patch, MagicMock

import jwt
from cryptography.hazmat.primitives.asymmetric import rsa

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

import function_app


TENANT_ID = "test-tenant-id"
AUDIENCE = "test-client-id"
KID = "test-key-id"


def _generate_rsa_key():
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def _jwk_for(private_key, kid):
    alg = jwt.algorithms.RSAAlgorithm(jwt.algorithms.RSAAlgorithm.SHA256)
    jwk = alg.to_jwk(private_key.public_key(), as_dict=True)
    jwk["kid"] = kid
    jwk["kty"] = "RSA"
    jwk["use"] = "sig"
    return jwk


def _make_token(private_key, issuer, kid=KID, audience=AUDIENCE, expired=False):
    now = datetime.now(timezone.utc)
    exp = now - timedelta(minutes=5) if expired else now + timedelta(hours=1)
    payload = {
        "oid": "test-user-id",
        "iss": issuer,
        "aud": audience,
        "exp": int(exp.timestamp()),
        "iat": int(now.timestamp()),
    }
    return jwt.encode(payload, private_key, algorithm="RS256", headers={"kid": kid})


def _mock_response(json_data):
    resp = MagicMock()
    resp.json.return_value = json_data
    resp.raise_for_status.return_value = None
    return resp


class JwtIssuerValidationTestBase(unittest.TestCase):
    def setUp(self):
        self.private_key = _generate_rsa_key()
        self.jwks = {"keys": [_jwk_for(self.private_key, KID)]}
        self.env_patcher = patch.dict(
            os.environ,
            {
                "AZURE_TENANT_ID": TENANT_ID,
                "EXPECTED_AUDIENCE": AUDIENCE,
            },
            clear=False,
        )
        self.env_patcher.start()
        os.environ.pop("AZURE_AD_ISSUER", None)
        # Ensure each test starts with a clean, unpopulated JWKS cache.
        function_app._jwks_cache = {}

    def tearDown(self):
        self.env_patcher.stop()
        function_app._jwks_cache = {}

    def _mock_http_get(self, discovery_doc):
        """Return a side_effect for http_requests.get: discovery doc, then JWKS."""
        def _get(url, timeout=10):
            if url.endswith("/.well-known/openid-configuration"):
                return _mock_response(discovery_doc)
            if url == discovery_doc.get("jwks_uri"):
                return _mock_response(self.jwks)
            raise AssertionError(f"Unexpected URL fetched: {url}")
        return _get


class TestV2IssuerAccepted(JwtIssuerValidationTestBase):
    def test_v2_token_accepted_when_discovery_issuer_is_v2(self):
        v2_issuer = f"https://login.microsoftonline.com/{TENANT_ID}/v2.0"
        discovery_doc = {
            "issuer": v2_issuer,
            "jwks_uri": "https://login.microsoftonline.com/test-tenant-id/discovery/v2.0/keys",
        }
        token = _make_token(self.private_key, issuer=v2_issuer)

        with patch.object(
            function_app.http_requests, "get", side_effect=self._mock_http_get(discovery_doc)
        ):
            claims = function_app._validate_jwt(token)

        self.assertEqual(claims["oid"], "test-user-id")
        self.assertEqual(claims["iss"], v2_issuer)


class TestWrongIssuerRejected(JwtIssuerValidationTestBase):
    def test_token_with_wrong_issuer_rejected(self):
        v2_issuer = f"https://login.microsoftonline.com/{TENANT_ID}/v2.0"
        discovery_doc = {
            "issuer": v2_issuer,
            "jwks_uri": "https://login.microsoftonline.com/test-tenant-id/discovery/v2.0/keys",
        }
        # Token signed with an issuer that does NOT match discovery's issuer.
        token = _make_token(self.private_key, issuer="https://evil.example.com/not-azure/")

        with patch.object(
            function_app.http_requests, "get", side_effect=self._mock_http_get(discovery_doc)
        ):
            with self.assertRaises(jwt.InvalidIssuerError):
                function_app._validate_jwt(token)


class TestDiscoveryUnavailableFallback(JwtIssuerValidationTestBase):
    def test_fallback_still_validates_v1_tokens_when_discovery_issuer_missing(self):
        """If the discovery document has no "issuer" field (unavailable /
        unparseable), _validate_jwt must fall back to the constructed v1
        issuer and still accept a genuine v1 token."""
        v1_issuer = f"https://sts.windows.net/{TENANT_ID}/"
        discovery_doc = {
            # No "issuer" field present.
            "jwks_uri": "https://login.microsoftonline.com/test-tenant-id/discovery/v2.0/keys",
        }
        token = _make_token(self.private_key, issuer=v1_issuer)

        with patch.object(
            function_app.http_requests, "get", side_effect=self._mock_http_get(discovery_doc)
        ):
            claims = function_app._validate_jwt(token)

        self.assertEqual(claims["oid"], "test-user-id")
        self.assertEqual(claims["iss"], v1_issuer)

    def test_fallback_rejects_v2_token_when_discovery_issuer_missing(self):
        """Sanity check: the v1 fallback issuer does NOT accept v2 tokens,
        confirming the fallback is genuinely v1-only (matches Finding 9's
        documented nuance)."""
        v2_issuer = f"https://login.microsoftonline.com/{TENANT_ID}/v2.0"
        discovery_doc = {
            "jwks_uri": "https://login.microsoftonline.com/test-tenant-id/discovery/v2.0/keys",
        }
        token = _make_token(self.private_key, issuer=v2_issuer)

        with patch.object(
            function_app.http_requests, "get", side_effect=self._mock_http_get(discovery_doc)
        ):
            with self.assertRaises(jwt.InvalidIssuerError):
                function_app._validate_jwt(token)


class TestExplicitIssuerOverride(JwtIssuerValidationTestBase):
    def test_explicit_issuer_env_var_overrides_discovery(self):
        override_issuer = "https://custom-issuer.example.com/"
        discovery_doc = {
            "issuer": f"https://login.microsoftonline.com/{TENANT_ID}/v2.0",
            "jwks_uri": "https://login.microsoftonline.com/test-tenant-id/discovery/v2.0/keys",
        }
        token = _make_token(self.private_key, issuer=override_issuer)

        with patch.dict(os.environ, {"AZURE_AD_ISSUER": override_issuer}):
            with patch.object(
                function_app.http_requests, "get", side_effect=self._mock_http_get(discovery_doc)
            ):
                claims = function_app._validate_jwt(token)

        self.assertEqual(claims["iss"], override_issuer)


if __name__ == "__main__":
    unittest.main()
