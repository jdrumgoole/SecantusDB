"""SCRAM authentication (SHA-256 + SHA-1).

Implements MongoDB's authentication mechanisms end-to-end on the
wire: PBKDF2 password derivation per RFC 7677 (SCRAM-SHA-256) and
RFC 5802 (SCRAM-SHA-1), the challenge/response state machine, and
per-connection state pymongo / mongo-go-driver expect across
saslStart / saslContinue.

Both mechanisms share the same wire flow (n, GS2 header, auth message
construction, proof verification) — the only differences are the hash
algorithm (SHA-256 vs SHA-1), the salt length (28 vs 16 bytes), and the
default iteration count (15000 vs 10000). A user record can carry
credentials for both mechanisms simultaneously (as real mongod does)
so legacy clients pinned to SHA-1 and modern clients defaulting to
SHA-256 can authenticate the same user.

What's intentionally NOT here:

- Authorization (RBAC): see ``secantus.rbac``; integrated into the
  command dispatcher as a separate concern.
- SASLprep / RFC 4013 password normalisation: ASCII-only passwords work
  byte-identically against real ``mongod``; non-ASCII passwords may
  diverge from a SASLprepping client's expectation.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
from dataclasses import dataclass, field
from typing import Any

SCRAM_SHA_256 = "SCRAM-SHA-256"
SCRAM_SHA_1 = "SCRAM-SHA-1"
SUPPORTED_MECHS = (SCRAM_SHA_256, SCRAM_SHA_1)

DEFAULT_ITERATIONS = 15000  # MongoDB default for SCRAM-SHA-256
NONCE_BYTES = 24
SALT_BYTES = 28


# Per-mechanism parameters. ``hash_name`` is the OpenSSL hash identifier
# passed to PBKDF2 + HMAC; ``dklen`` is the digest size in bytes;
# ``default_iterations`` matches mongod's per-mechanism default; salt
# length follows RFC 5802 / 7677 (16 bytes for SHA-1, 28 for SHA-256).
_MECH_PARAMS: dict[str, dict[str, Any]] = {
    SCRAM_SHA_256: {
        "hash_name": "sha256",
        "dklen": 32,
        "default_iterations": 15000,
        "salt_bytes": 28,
    },
    SCRAM_SHA_1: {
        "hash_name": "sha1",
        "dklen": 20,
        "default_iterations": 10000,
        "salt_bytes": 16,
    },
}


def _hash_for(mechanism: str) -> Any:
    return getattr(hashlib, _MECH_PARAMS[mechanism]["hash_name"])


class AuthError(Exception):
    """Authentication failure. Surfaces as MongoDB AuthenticationFailed (code 18)."""


@dataclass
class StoredCredentials:
    """SCRAM credentials for a single mechanism, persisted per user.

    These are exactly the fields a real ``mongod`` keeps in
    ``admin.system.users[].credentials[<mechanism>]``. The plaintext
    password is never stored. ``mechanism`` is one of
    ``SCRAM_SHA_256`` / ``SCRAM_SHA_1``.
    """

    iteration_count: int
    salt: bytes
    stored_key: bytes
    server_key: bytes
    mechanism: str = SCRAM_SHA_256

    def to_doc(self) -> dict[str, object]:
        return {
            self.mechanism: {
                "iterationCount": self.iteration_count,
                "salt": base64.b64encode(self.salt).decode("ascii"),
                "storedKey": base64.b64encode(self.stored_key).decode("ascii"),
                "serverKey": base64.b64encode(self.server_key).decode("ascii"),
            }
        }

    @classmethod
    def from_doc(
        cls, doc: dict[str, object], *, mechanism: str = SCRAM_SHA_256
    ) -> StoredCredentials:
        if mechanism not in doc:
            raise KeyError(f"credentials doc has no entry for {mechanism!r}")
        sub = doc[mechanism]
        assert isinstance(sub, dict)
        return cls(
            iteration_count=int(sub["iterationCount"]),
            salt=base64.b64decode(sub["salt"]),
            stored_key=base64.b64decode(sub["storedKey"]),
            server_key=base64.b64decode(sub["serverKey"]),
            mechanism=mechanism,
        )


def _scram_password_digest(username: str, password: str, mechanism: str) -> bytes:
    """Return the byte string PBKDF2 should hash for the given mechanism.

    SCRAM-SHA-256 feeds the raw UTF-8 password directly (after SASLprep
    in a fully-conformant client; we don't normalise here).

    SCRAM-SHA-1 in MongoDB applies a legacy *password prepass*:
    ``hex(MD5(username + ":mongo:" + password))``. The prepass exists
    so MongoDB 3.x could honour passwords originally stored under the
    pre-SCRAM ``MONGODB-CR`` scheme, which used exactly this digest as
    its "password" input. Every modern driver (pymongo, mongo-go-driver,
    Java, Node) applies the same prepass in its SCRAM-SHA-1 path, so to
    accept those clients we have to apply it server-side too. Without
    this the PBKDF2 input bytes would diverge and authentication would
    fail at the proof step.
    """
    if mechanism == SCRAM_SHA_1:
        digest = hashlib.md5(  # noqa: S324 - legacy MongoDB prepass, not security-sensitive
            f"{username}:mongo:{password}".encode()
        ).hexdigest()
        return digest.encode("utf-8")
    return password.encode("utf-8")


def derive_credentials(
    password: str,
    *,
    iterations: int | None = None,
    salt: bytes | None = None,
    mechanism: str = SCRAM_SHA_256,
    username: str = "",
) -> StoredCredentials:
    """Derive SCRAM stored credentials from a plaintext password.

    Layout matches RFC 5802 §3:

        SaltedPassword = PBKDF2(HMAC-<hash>, prepassed-password, salt, iterations)
        ClientKey      = HMAC(SaltedPassword, "Client Key")
        StoredKey      = <hash>(ClientKey)
        ServerKey      = HMAC(SaltedPassword, "Server Key")

    where ``<hash>`` is SHA-256 for ``SCRAM-SHA-256`` and SHA-1 for
    ``SCRAM-SHA-1``. ``prepassed-password`` is the raw UTF-8 password
    for SCRAM-SHA-256 and ``hex(MD5(username + ":mongo:" + password))``
    for SCRAM-SHA-1 (see ``_scram_password_digest``). The plaintext is
    dropped on return.

    ``username`` is required when ``mechanism`` is SCRAM-SHA-1 because
    the legacy prepass mixes it into the digest. For SCRAM-SHA-256 it
    is ignored.
    """
    if mechanism not in _MECH_PARAMS:
        raise ValueError(f"unsupported SCRAM mechanism: {mechanism!r}")
    if mechanism == SCRAM_SHA_1 and not username:
        raise ValueError(
            "username is required to derive SCRAM-SHA-1 credentials "
            "(MongoDB's legacy prepass mixes it into the digest)"
        )
    params = _MECH_PARAMS[mechanism]
    if iterations is None:
        iterations = int(params["default_iterations"])
    if salt is None:
        salt = secrets.token_bytes(int(params["salt_bytes"]))
    hash_obj = _hash_for(mechanism)
    pbkdf2_input = _scram_password_digest(username, password, mechanism)
    salted = hashlib.pbkdf2_hmac(
        params["hash_name"], pbkdf2_input, salt, iterations, dklen=params["dklen"]
    )
    client_key = hmac.new(salted, b"Client Key", hash_obj).digest()
    stored_key = hash_obj(client_key).digest()
    server_key = hmac.new(salted, b"Server Key", hash_obj).digest()
    return StoredCredentials(
        iteration_count=iterations,
        salt=salt,
        stored_key=stored_key,
        server_key=server_key,
        mechanism=mechanism,
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
    mechanism: str = SCRAM_SHA_256,
) -> tuple[bytes, ScramState]:
    """Process a SCRAM client-first message and return (server-first payload, state).

    ``creds`` is the looked-up StoredCredentials for ``(db_name, user)``,
    or None if no such user exists. Either way we generate a server-first
    reply: when the user is missing we substitute fabricated credentials
    (under the requested ``mechanism``) so the caller can finish the
    SCRAM round-trip and surface ``AuthenticationFailed`` only after
    the client proof step. This matches real ``mongod``'s timing.
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
        # SCRAM-SHA-1 needs a username for its legacy prepass; the value
        # itself is fine since the proof will fail anyway.
        creds = derive_credentials(
            secrets.token_urlsafe(16),
            mechanism=mechanism,
            username=username,
        )
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
    hash_obj = _hash_for(state.creds.mechanism)
    client_signature = hmac.new(state.creds.stored_key, auth_message, hash_obj).digest()
    try:
        client_proof = base64.b64decode(client_proof_b64, validate=False)
    except Exception as exc:  # noqa: BLE001
        raise AuthError("invalid SCRAM client proof encoding") from exc
    if len(client_proof) != len(client_signature):
        raise AuthError("authentication failed")
    received_client_key = bytes(a ^ b for a, b in zip(client_proof, client_signature, strict=True))
    expected_stored = hash_obj(received_client_key).digest()
    # Constant-time compare guards against timing oracles even when
    # user_exists is False.
    if not hmac.compare_digest(expected_stored, state.creds.stored_key):
        raise AuthError("authentication failed")
    if not state.user_exists:
        raise AuthError("authentication failed")
    server_signature = hmac.new(state.creds.server_key, auth_message, hash_obj).digest()
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
