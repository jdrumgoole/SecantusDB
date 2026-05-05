"""SCRAM-SHA-256 authentication.

Implements MongoDB's default authentication mechanism end-to-end on the
wire: PBKDF2-HMAC-SHA-256 password derivation per RFC 7677, the SCRAM
challenge / response state machine per RFC 5802, and the per-connection
state pymongo / mongo-go-driver expect across saslStart / saslContinue.

What's intentionally NOT here:

- Authorization (RBAC): an authenticated user is treated as fully
  privileged; ``createUser`` accepts a ``roles`` array but never
  consults it on subsequent commands. Role enforcement is a separate
  slice — see ``tasks/backlog.md``.
- SASLprep / RFC 4013 password normalisation: ASCII-only passwords work
  byte-identically against real ``mongod``; non-ASCII passwords may
  diverge from a SASLprepping client's expectation.
- SCRAM-SHA-1: the legacy mechanism. Modern drivers default to
  SCRAM-SHA-256; we only advertise that one.
- Speculative authentication (auth folded into ``hello`` reply) — we
  honour the explicit ``saslStart`` round-trip instead.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
from dataclasses import dataclass, field
from typing import Any

SCRAM_SHA_256 = "SCRAM-SHA-256"
DEFAULT_ITERATIONS = 15000  # MongoDB default for SCRAM-SHA-256
NONCE_BYTES = 24
SALT_BYTES = 28


class AuthError(Exception):
    """Authentication failure. Surfaces as MongoDB AuthenticationFailed (code 18)."""


@dataclass
class StoredCredentials:
    """SCRAM-SHA-256 credentials persisted per user.

    These are exactly the fields a real ``mongod`` keeps in
    ``admin.system.users[].credentials["SCRAM-SHA-256"]``. The plaintext
    password is never stored.
    """

    iteration_count: int
    salt: bytes
    stored_key: bytes
    server_key: bytes

    def to_doc(self) -> dict[str, object]:
        return {
            SCRAM_SHA_256: {
                "iterationCount": self.iteration_count,
                "salt": base64.b64encode(self.salt).decode("ascii"),
                "storedKey": base64.b64encode(self.stored_key).decode("ascii"),
                "serverKey": base64.b64encode(self.server_key).decode("ascii"),
            }
        }

    @classmethod
    def from_doc(cls, doc: dict[str, object]) -> StoredCredentials:
        sub = doc[SCRAM_SHA_256]  # type: ignore[index]
        assert isinstance(sub, dict)
        return cls(
            iteration_count=int(sub["iterationCount"]),
            salt=base64.b64decode(sub["salt"]),
            stored_key=base64.b64decode(sub["storedKey"]),
            server_key=base64.b64decode(sub["serverKey"]),
        )


def derive_credentials(
    password: str,
    *,
    iterations: int = DEFAULT_ITERATIONS,
    salt: bytes | None = None,
) -> StoredCredentials:
    """Derive SCRAM-SHA-256 stored credentials from a plaintext password.

    Layout matches RFC 5802 §3:

        SaltedPassword = PBKDF2(HMAC-SHA-256, password, salt, iterations)
        ClientKey      = HMAC(SaltedPassword, "Client Key")
        StoredKey      = SHA-256(ClientKey)
        ServerKey      = HMAC(SaltedPassword, "Server Key")

    The plaintext password is dropped on return; only StoredKey,
    ServerKey, salt, and iteration count are kept.
    """
    if salt is None:
        salt = secrets.token_bytes(SALT_BYTES)
    salted = hashlib.pbkdf2_hmac("sha256", password.encode("utf-8"), salt, iterations, dklen=32)
    client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    server_key = hmac.new(salted, b"Server Key", hashlib.sha256).digest()
    return StoredCredentials(
        iteration_count=iterations,
        salt=salt,
        stored_key=stored_key,
        server_key=server_key,
    )


def _parse_attrs(payload: bytes) -> dict[str, str]:
    attrs: dict[str, str] = {}
    for chunk in payload.split(b","):
        if not chunk:
            continue
        key, _, val = chunk.partition(b"=")
        if key:
            attrs[key.decode("utf-8")] = val.decode("utf-8")
    return attrs


@dataclass
class ScramState:
    """Per-conversation SCRAM state across one saslStart -> saslContinue round-trip."""

    conversation_id: int
    db_name: str
    username: str
    creds: StoredCredentials
    server_nonce: str
    client_first_bare: bytes
    server_first: bytes
    user_exists: bool  # for constant-time-ish behaviour when user is missing
    step: int = 1


def begin_scram(
    *,
    conversation_id: int,
    db_name: str,
    payload: bytes,
    creds: StoredCredentials | None,
) -> tuple[bytes, ScramState]:
    """Process a SCRAM client-first message and return (server-first payload, state).

    ``creds`` is the looked-up StoredCredentials for ``(db_name, user)``,
    or None if no such user exists. Either way we generate a server-first
    reply: when the user is missing we substitute fabricated credentials
    so the caller can finish the SCRAM round-trip and surface
    ``AuthenticationFailed`` only after the client proof step. This
    matches real ``mongod``'s timing behaviour.
    """
    if not payload.startswith(b"n,"):
        raise AuthError("invalid SCRAM client-first: expected GS2 header 'n,,'")
    # GS2 header is "n,," (no channel binding, no authzid). Skip past it
    # to the bare client-first.
    gs2_end = payload.find(b",", 2)
    if gs2_end < 0:
        raise AuthError("invalid SCRAM client-first: malformed GS2 header")
    bare = payload[gs2_end + 1 :]
    attrs = _parse_attrs(bare)
    username = attrs.get("n", "")
    client_nonce = attrs.get("r", "")
    if not username or not client_nonce:
        raise AuthError("invalid SCRAM client-first: missing user or nonce")
    if creds is None:
        # Fabricate credentials with the right shape so we can run through
        # the rest of the protocol; the proof check will fail at step 2.
        creds = derive_credentials(secrets.token_urlsafe(16))
        user_exists = False
    else:
        user_exists = True
    server_nonce_b = base64.b64encode(secrets.token_bytes(NONCE_BYTES)).decode("ascii")
    combined_nonce = client_nonce + server_nonce_b
    salt_b64 = base64.b64encode(creds.salt).decode("ascii")
    server_first = f"r={combined_nonce},s={salt_b64},i={creds.iteration_count}".encode()
    state = ScramState(
        conversation_id=conversation_id,
        db_name=db_name,
        username=username,
        creds=creds,
        server_nonce=server_nonce_b,
        client_first_bare=bare,
        server_first=server_first,
        user_exists=user_exists,
        step=1,
    )
    return server_first, state


def continue_scram(state: ScramState, payload: bytes) -> bytes:
    """Process SCRAM client-final; return server-final payload.

    Verifies the client proof. Raises ``AuthError`` on any mismatch
    (wrong password, unknown user, malformed message). On success the
    state's step is bumped to 2 and the server-final ``v=...`` payload
    is returned.
    """
    if state.step != 1:
        raise AuthError("SCRAM conversation out of order")
    attrs = _parse_attrs(payload)
    client_proof_b64 = attrs.get("p", "")
    received_nonce = attrs.get("r", "")
    if not client_proof_b64:
        raise AuthError("invalid SCRAM client-final: missing proof")
    if not received_nonce.endswith(state.server_nonce):
        raise AuthError("invalid SCRAM nonce")
    # Strip the p= attribute; the rest is the client-final-without-proof
    # used in the auth-message construction.
    parts_without_proof = [p for p in payload.split(b",") if not p.startswith(b"p=")]
    client_final_without_proof = b",".join(parts_without_proof)
    auth_message = (
        state.client_first_bare + b"," + state.server_first + b"," + client_final_without_proof
    )
    client_signature = hmac.new(state.creds.stored_key, auth_message, hashlib.sha256).digest()
    try:
        client_proof = base64.b64decode(client_proof_b64, validate=False)
    except Exception as exc:  # noqa: BLE001
        raise AuthError("invalid SCRAM client proof encoding") from exc
    if len(client_proof) != len(client_signature):
        raise AuthError("authentication failed")
    received_client_key = bytes(a ^ b for a, b in zip(client_proof, client_signature, strict=True))
    expected_stored = hashlib.sha256(received_client_key).digest()
    # Constant-time compare guards against timing oracles even when
    # user_exists is False.
    if not hmac.compare_digest(expected_stored, state.creds.stored_key):
        raise AuthError("authentication failed")
    if not state.user_exists:
        raise AuthError("authentication failed")
    server_signature = hmac.new(state.creds.server_key, auth_message, hashlib.sha256).digest()
    state.step = 2
    return b"v=" + base64.b64encode(server_signature)


@dataclass
class ConnectionAuth:
    """Per-connection authentication state.

    One instance per TCP connection, threaded through every
    ``CommandContext`` for that connection. Holds the in-flight SCRAM
    conversation (if any), the set of authenticated principals, and
    the union of role bindings across those principals.

    ``authenticated_principals`` is a list of ``(db_name, username)``
    tuples. A connection can authenticate as multiple users (one per
    auth source) — pymongo does this when an ``authSource`` URI option
    is paired with admin-database commands.

    ``effective_roles`` is the union of the role bindings of every
    authenticated principal. Each entry is the
    ``{"role": <name>, "db": <db>}`` shape stored on the user record.
    The RBAC privilege check (:func:`secantus.rbac.check_privilege`)
    walks this list. Re-population is the auth-completion handler's
    responsibility (``_sasl_continue``); ``dropUser`` of the calling
    principal also rebuilds it.
    """

    authenticated_principals: list[tuple[str, str]] = field(default_factory=list)
    effective_roles: list[dict[str, Any]] = field(default_factory=list)
    scram: ScramState | None = None
    _next_conv_id: int = 1

    @property
    def is_authenticated(self) -> bool:
        return bool(self.authenticated_principals)

    def new_conversation_id(self) -> int:
        cid = self._next_conv_id
        self._next_conv_id += 1
        return cid

    def add_principal_roles(self, roles: list[dict[str, Any]]) -> None:
        """Merge a freshly-authenticated principal's role bindings.

        Bindings are deduplicated by ``(role, db)`` so a user that
        appears with the same role on multiple connections doesn't
        balloon the effective set.
        """
        seen = {(r.get("role"), r.get("db")) for r in self.effective_roles}
        for role in roles:
            if not isinstance(role, dict):
                continue
            key = (role.get("role"), role.get("db"))
            if key in seen:
                continue
            seen.add(key)
            self.effective_roles.append(dict(role))

    def remove_principal_roles(self, principal_roles: list[dict[str, Any]]) -> None:
        """Drop one principal's roles from the effective set.

        Used when a connection's user is deleted via ``dropUser`` — we
        clear that user's roles so subsequent commands on the same
        connection see the reduced privilege set.
        """
        to_remove = {(r.get("role"), r.get("db")) for r in principal_roles if isinstance(r, dict)}
        if not to_remove:
            return
        self.effective_roles = [
            r for r in self.effective_roles if (r.get("role"), r.get("db")) not in to_remove
        ]
