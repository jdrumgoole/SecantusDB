"""PostgreSQL SCRAM-SHA-256 authentication (server side).

Postgres SCRAM is the same RFC 5802 / RFC 7677 exchange MongoDB uses, so the
credential derivation is shared with ``secantus.auth`` — a ``StoredCredentials``
(salt, iteration count, StoredKey, ServerKey) verifies a client proof without
ever holding the plaintext password. Only the message *framing* differs: this
module speaks the Postgres SASL envelope (AuthenticationSASL / SASLContinue /
SASLFinal) rather than Mongo's ``saslStart`` / ``saslContinue`` commands.

Channel binding is not offered (``SCRAM-SHA-256`` only, not the ``-PLUS``
variant), matching a server that doesn't require TLS for auth.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import os
from dataclasses import dataclass, field

from secantus.auth import SCRAM_SHA_256, StoredCredentials, derive_credentials

GS2_HEADER = b"n,,"
# base64("n,,") — the channel-binding value the client must echo in c=.
_GS2_B64 = base64.b64encode(GS2_HEADER).decode("ascii")


class PGAuthError(Exception):
    """SCRAM verification failed (bad password / malformed message)."""


@dataclass
class UserStore:
    """In-memory username -> SCRAM credentials, derived from plaintext at init.

    ``roles`` optionally binds each username to RBAC role bindings
    (``[{"role": ..., "db": ...}]``) so the wire server can authorize statements
    via :func:`secantus.rbac.check_privilege` — the same role model as the Mongo
    server. A user with credentials but no role entry authenticates but is
    granted nothing (real RBAC: LOGIN without grants). (#193)
    """

    creds: dict[str, StoredCredentials]
    roles: dict[str, list[dict[str, str]]] = field(default_factory=dict)

    @classmethod
    def from_passwords(
        cls,
        users: dict[str, str],
        roles: dict[str, list[dict[str, str]]] | None = None,
    ) -> UserStore:
        return cls(
            {u: derive_credentials(p, mechanism=SCRAM_SHA_256) for u, p in users.items()},
            {u: list(r) for u, r in (roles or {}).items()},
        )

    def get(self, username: str) -> StoredCredentials | None:
        return self.creds.get(username)

    def roles_for(self, username: str) -> list[dict[str, str]]:
        return self.roles.get(username, [])


def mock_credentials() -> StoredCredentials:
    """Random verifier for an unknown user.

    Running the full SCRAM exchange against a throwaway verifier (rather than
    bailing early) makes an unknown user fail at the *same* step as a wrong
    password, so the handshake can't be used to enumerate valid usernames.
    """
    return StoredCredentials(
        iteration_count=4096,
        salt=os.urandom(16),
        stored_key=os.urandom(32),
        server_key=os.urandom(32),
        mechanism=SCRAM_SHA_256,
    )


def _attrs(message: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for part in message.split(","):
        if "=" in part:
            key, _, value = part.partition("=")
            out[key] = value
    return out


def _server_nonce() -> str:
    return base64.b64encode(os.urandom(18)).decode("ascii")


class ScramExchange:
    """One SCRAM-SHA-256 conversation. Feed it the client messages in order."""

    def __init__(self, creds: StoredCredentials) -> None:
        self._creds = creds
        self._client_first_bare = ""
        self._server_first = ""

    def server_first(self, client_first: bytes) -> bytes:
        text = client_first.decode("utf-8", "replace")
        # Strip the gs2 header ("n,," / "y,," / "n,a=..,") to get client-first-bare.
        if text.startswith("n,") or text.startswith("y,"):
            parts = text.split(",", 2)
            # A well-formed header has the gs2 flag, an (optional) authzid, then
            # the bare message — three comma-separated parts. A truncated
            # header like "n," yields fewer; surface it as the typed
            # PGAuthError rather than an unpack ValueError. (§I21)
            if len(parts) < 3:
                raise PGAuthError("malformed SCRAM client-first message")
            bare = parts[2]
        else:
            bare = text
        self._client_first_bare = bare
        attrs = _attrs(bare)
        client_nonce = attrs.get("r", "")
        if not client_nonce:
            raise PGAuthError("missing client nonce")
        combined = client_nonce + _server_nonce()
        salt_b64 = base64.b64encode(self._creds.salt).decode("ascii")
        self._server_first = f"r={combined},s={salt_b64},i={self._creds.iteration_count}"
        return self._server_first.encode("utf-8")

    def server_final(self, client_final: bytes) -> bytes:
        text = client_final.decode("utf-8", "replace")
        attrs = _attrs(text)
        if attrs.get("c") != _GS2_B64:
            raise PGAuthError("channel binding mismatch")
        proof_b64 = attrs.get("p")
        if proof_b64 is None:
            raise PGAuthError("missing client proof")
        without_proof = f"c={attrs['c']},r={attrs.get('r', '')}"
        auth_message = f"{self._client_first_bare},{self._server_first},{without_proof}"
        auth_bytes = auth_message.encode("utf-8")

        client_signature = hmac.new(self._creds.stored_key, auth_bytes, hashlib.sha256).digest()
        client_proof = base64.b64decode(proof_b64)
        client_key = bytes(a ^ b for a, b in zip(client_proof, client_signature, strict=True))
        # Constant-time compare — a plain ``!=`` on the digests would leak
        # timing that narrows a brute force. Mirrors the Mongo-side SCRAM check
        # in ``secantus.auth`` (which uses ``hmac.compare_digest``). (#195)
        if not hmac.compare_digest(hashlib.sha256(client_key).digest(), self._creds.stored_key):
            raise PGAuthError("password authentication failed")

        server_signature = hmac.new(self._creds.server_key, auth_bytes, hashlib.sha256).digest()
        return b"v=" + base64.b64encode(server_signature)
