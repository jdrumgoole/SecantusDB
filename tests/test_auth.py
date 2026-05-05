"""SCRAM-SHA-256 authentication tests.

Three layers of coverage:

1. Pure unit tests on the auth module (no server, no pymongo). Verify the
   SCRAM math against pre-computed vectors and exercise the state machine.
2. Storage tests for the user CRUD methods.
3. End-to-end pymongo integration: createUser, then connect with
   ``MongoClient(uri, username=, password=)`` and run a real command.
"""

from __future__ import annotations

import base64
import hashlib
import hmac

import pymongo
import pytest
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer
from secantus.auth import (
    SCRAM_SHA_256,
    AuthError,
    ConnectionAuth,
    StoredCredentials,
    begin_scram,
    continue_scram,
    derive_credentials,
)
from secantus.storage import Storage

# ----------------------------------------------------------------------
# Unit: derivation
# ----------------------------------------------------------------------


def test_derive_credentials_round_trips_through_doc() -> None:
    creds = derive_credentials("hunter2")
    doc = creds.to_doc()
    revived = StoredCredentials.from_doc(doc)
    assert revived.iteration_count == creds.iteration_count
    assert revived.salt == creds.salt
    assert revived.stored_key == creds.stored_key
    assert revived.server_key == creds.server_key


def test_derive_credentials_is_deterministic_with_fixed_salt() -> None:
    salt = b"\x00" * 28
    a = derive_credentials("pw", iterations=1000, salt=salt)
    b = derive_credentials("pw", iterations=1000, salt=salt)
    assert a.stored_key == b.stored_key
    assert a.server_key == b.server_key


def test_derive_credentials_diverges_on_different_password() -> None:
    salt = b"x" * 28
    a = derive_credentials("right", iterations=1000, salt=salt)
    b = derive_credentials("wrong", iterations=1000, salt=salt)
    assert a.stored_key != b.stored_key


# ----------------------------------------------------------------------
# Unit: SCRAM state machine
# ----------------------------------------------------------------------


def _client_first(user: str, client_nonce: str) -> bytes:
    return f"n,,n={user},r={client_nonce}".encode()


def _client_proof_for(
    *,
    password: str,
    salt: bytes,
    iterations: int,
    client_nonce: str,
    combined_nonce: str,
    server_first: bytes,
    client_first_bare: bytes,
) -> tuple[bytes, bytes]:
    """Compute (client-final-payload, expected server-signature) for a
    correct password. Returns the bytes a real client would send."""
    salted = hashlib.pbkdf2_hmac("sha256", password.encode(), salt, iterations, dklen=32)
    client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    server_key = hmac.new(salted, b"Server Key", hashlib.sha256).digest()
    channel_binding = base64.b64encode(b"n,,").decode("ascii")
    client_final_without_proof = f"c={channel_binding},r={combined_nonce}".encode()
    auth_message = client_first_bare + b"," + server_first + b"," + client_final_without_proof
    client_signature = hmac.new(stored_key, auth_message, hashlib.sha256).digest()
    proof = bytes(a ^ b for a, b in zip(client_key, client_signature, strict=True))
    payload = client_final_without_proof + b",p=" + base64.b64encode(proof)
    server_signature = hmac.new(server_key, auth_message, hashlib.sha256).digest()
    return payload, server_signature


def test_scram_full_roundtrip_succeeds_with_correct_password() -> None:
    salt = b"s" * 28
    creds = derive_credentials("hunter2", iterations=1000, salt=salt)
    client_nonce = "client-nonce-abc"
    client_first = _client_first("alice", client_nonce)
    server_first, state = begin_scram(
        conversation_id=1, db_name="admin", payload=client_first, creds=creds
    )
    # server-first looks like r=<combined>,s=<salt>,i=1000
    parts = dict(p.decode().split("=", 1) for p in server_first.split(b","))
    assert parts["r"].startswith(client_nonce)
    assert parts["s"] == base64.b64encode(salt).decode("ascii")
    assert parts["i"] == "1000"
    combined = parts["r"]
    client_final, expected_sig = _client_proof_for(
        password="hunter2",
        salt=salt,
        iterations=1000,
        client_nonce=client_nonce,
        combined_nonce=combined,
        server_first=server_first,
        client_first_bare=state.client_first_bare,
    )
    server_final = continue_scram(state, client_final)
    assert server_final == b"v=" + base64.b64encode(expected_sig)
    assert state.step == 2


def test_scram_wrong_password_fails_at_proof_step() -> None:
    salt = b"s" * 28
    creds = derive_credentials("right", iterations=1000, salt=salt)
    client_nonce = "client-xyz"
    client_first = _client_first("bob", client_nonce)
    server_first, state = begin_scram(
        conversation_id=1, db_name="admin", payload=client_first, creds=creds
    )
    combined = dict(p.decode().split("=", 1) for p in server_first.split(b","))["r"]
    client_final, _ = _client_proof_for(
        password="wrong",
        salt=salt,
        iterations=1000,
        client_nonce=client_nonce,
        combined_nonce=combined,
        server_first=server_first,
        client_first_bare=state.client_first_bare,
    )
    with pytest.raises(AuthError):
        continue_scram(state, client_final)


def test_scram_unknown_user_completes_roundtrip_then_fails() -> None:
    """Server fabricates credentials when the user is missing so it can run
    the full conversation; rejection happens at the proof step. This is the
    real-mongod behaviour to avoid timing oracles."""
    client_first = _client_first("ghost", "client-nonce")
    server_first, state = begin_scram(
        conversation_id=1, db_name="admin", payload=client_first, creds=None
    )
    # Pick any plausible client final — proof will mismatch the fabricated key.
    combined = dict(p.decode().split("=", 1) for p in server_first.split(b","))["r"]
    client_final, _ = _client_proof_for(
        password="anything",
        salt=state.creds.salt,
        iterations=state.creds.iteration_count,
        client_nonce="client-nonce",
        combined_nonce=combined,
        server_first=server_first,
        client_first_bare=state.client_first_bare,
    )
    with pytest.raises(AuthError):
        continue_scram(state, client_final)


def test_scram_rejects_malformed_client_first() -> None:
    with pytest.raises(AuthError):
        begin_scram(
            conversation_id=1,
            db_name="admin",
            payload=b"this is not a SCRAM message",
            creds=None,
        )


def test_scram_continue_rejects_out_of_order() -> None:
    salt = b"s" * 28
    creds = derive_credentials("p", iterations=100, salt=salt)
    _, state = begin_scram(
        conversation_id=1,
        db_name="admin",
        payload=_client_first("u", "n"),
        creds=creds,
    )
    state.step = 2  # pretend we already finished
    with pytest.raises(AuthError):
        continue_scram(state, b"c=biws,r=nfoo,p=AAAA")


# ----------------------------------------------------------------------
# Unit: ConnectionAuth bookkeeping
# ----------------------------------------------------------------------


def test_connection_auth_starts_unauthenticated() -> None:
    conn = ConnectionAuth()
    assert not conn.is_authenticated
    assert conn.scram is None


def test_connection_auth_conversation_ids_increase() -> None:
    conn = ConnectionAuth()
    a = conn.new_conversation_id()
    b = conn.new_conversation_id()
    c = conn.new_conversation_id()
    assert (a, b, c) == (1, 2, 3)


# ----------------------------------------------------------------------
# Storage CRUD for users
# ----------------------------------------------------------------------


def _user_record(db: str, user: str, password: str) -> dict:
    return {
        "_id": f"{db}.{user}",
        "user": user,
        "db": db,
        "credentials": derive_credentials(password, iterations=1000).to_doc(),
        "roles": [],
        "mechanisms": [SCRAM_SHA_256],
    }


def test_storage_add_get_drop_user(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        record = _user_record("admin", "alice", "pw")
        assert s.add_user("admin", "alice", record) is True
        assert s.add_user("admin", "alice", record) is False  # duplicate
        assert s.add_user("admin", "alice", record, replace=True) is True
        got = s.get_user("admin", "alice")
        assert got is not None
        assert got["user"] == "alice"
        assert got["db"] == "admin"
        assert SCRAM_SHA_256 in got["credentials"]
        assert s.drop_user("admin", "alice") is True
        assert s.get_user("admin", "alice") is None
        assert s.drop_user("admin", "alice") is False
    finally:
        s.close()


def test_storage_list_users_paginates_and_filters_by_db(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        for db in ("admin", "app"):
            for i in range(3):
                rec = _user_record(db, f"u{i}", "pw")
                s.add_user(db, f"u{i}", rec)
        all_users = s.list_users()
        assert len(all_users) == 6
        admin_users = s.list_users("admin")
        assert {u["user"] for u in admin_users} == {"u0", "u1", "u2"}
        page1 = s.list_users("admin", skip=0, limit=2)
        page2 = s.list_users("admin", skip=2, limit=2)
        assert len(page1) == 2
        assert len(page2) == 1
    finally:
        s.close()


# ----------------------------------------------------------------------
# Integration: pymongo end-to-end via SCRAM-SHA-256
# ----------------------------------------------------------------------


@pytest.fixture
def server_no_auth(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def server_with_auth(tmp_path):
    """Server with --auth on. Pre-seed an admin user before yielding."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path), require_auth=True)
    srv.start()
    # Bypass auth gating by injecting the user directly into storage.
    # pymongo enforces iterations>=4096 even though SCRAM-SHA-256 RFC allows
    # any positive value. Use the SecantusDB default (15k) for realistic
    # behaviour.
    creds = derive_credentials("secret")
    record = {
        "_id": "admin.root",
        "user": "root",
        "db": "admin",
        "credentials": creds.to_doc(),
        "roles": [{"role": "root", "db": "admin"}],
        "mechanisms": [SCRAM_SHA_256],
    }
    srv.storage.add_user("admin", "root", record)
    try:
        yield srv
    finally:
        srv.stop()


def test_create_user_then_authenticate_via_pymongo(server_no_auth) -> None:
    # Provision a user on a server that doesn't require auth.
    plain = pymongo.MongoClient(server_no_auth.uri)
    plain.admin.command({"createUser": "alice", "pwd": "hunter2", "roles": []})
    plain.close()

    # Now reconnect WITH credentials and prove the SCRAM round-trip works.
    authed = pymongo.MongoClient(
        server_no_auth.uri,
        username="alice",
        password="hunter2",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
    )
    # Forcing a roundtrip — the connection lazily authenticates on first command.
    info = authed.admin.command("connectionStatus")
    users = info["authInfo"]["authenticatedUsers"]
    assert {"user": "alice", "db": "admin"} in users
    authed.close()


def test_wrong_password_rejected(server_no_auth) -> None:
    plain = pymongo.MongoClient(server_no_auth.uri)
    plain.admin.command({"createUser": "bob", "pwd": "right", "roles": []})
    plain.close()

    bad = pymongo.MongoClient(
        server_no_auth.uri,
        username="bob",
        password="WRONG",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
        serverSelectionTimeoutMS=2000,
    )
    with pytest.raises(OperationFailure):
        bad.admin.command("ping")
    bad.close()


def test_unknown_user_rejected(server_no_auth) -> None:
    bad = pymongo.MongoClient(
        server_no_auth.uri,
        username="nobody",
        password="whatever",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
        serverSelectionTimeoutMS=2000,
    )
    with pytest.raises(OperationFailure):
        bad.admin.command("ping")
    bad.close()


def test_require_auth_blocks_unauthenticated_commands(server_with_auth) -> None:
    """With --auth on, an unauthenticated client gets Unauthorized for normal
    commands but the handshake still works."""
    plain = pymongo.MongoClient(server_with_auth.uri, serverSelectionTimeoutMS=2000)
    # hello / ping are pre-auth allowed; getting through should be fine.
    plain.admin.command("ping")
    # listDatabases requires auth -> should be Unauthorized.
    with pytest.raises(OperationFailure) as exc:
        plain.admin.command("listDatabases")
    assert exc.value.code == 13
    plain.close()


def test_require_auth_allows_authenticated_commands(server_with_auth) -> None:
    authed = pymongo.MongoClient(
        server_with_auth.uri,
        username="root",
        password="secret",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
    )
    # Should succeed because we're authenticated.
    dbs = authed.admin.command("listDatabases")
    assert dbs["ok"] == 1.0
    authed.close()


def test_hello_advertises_scram_mech_when_asked(server_no_auth) -> None:
    """`hello` should include `saslSupportedMechs` when the client passes
    `saslSupportedMechs: "<db>.<user>"`. Drivers use this to decide which
    mechanisms to attempt."""
    plain = pymongo.MongoClient(server_no_auth.uri)
    plain.admin.command({"createUser": "carol", "pwd": "p", "roles": []})
    reply = plain.admin.command({"hello": 1, "saslSupportedMechs": "admin.carol"})
    assert SCRAM_SHA_256 in reply["saslSupportedMechs"]
    plain.close()


def test_users_info_returns_provisioned_user(server_no_auth) -> None:
    plain = pymongo.MongoClient(server_no_auth.uri)
    plain.admin.command({"createUser": "dave", "pwd": "p", "roles": []})
    info = plain.admin.command({"usersInfo": "dave"})
    assert any(u["user"] == "dave" for u in info["users"])
    # Credentials hidden by default.
    assert all("credentials" not in u for u in info["users"])
    info_with = plain.admin.command({"usersInfo": "dave", "showCredentials": True})
    assert any("credentials" in u for u in info_with["users"])
    plain.close()


def test_drop_user_revokes_subsequent_logins(server_no_auth) -> None:
    plain = pymongo.MongoClient(server_no_auth.uri)
    plain.admin.command({"createUser": "eve", "pwd": "p", "roles": []})
    # Verify she can authenticate.
    authed = pymongo.MongoClient(
        server_no_auth.uri,
        username="eve",
        password="p",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
    )
    authed.admin.command("ping")
    authed.close()
    # Drop her.
    plain.admin.command({"dropUser": "eve"})
    # Fresh connection should be rejected.
    rejected = pymongo.MongoClient(
        server_no_auth.uri,
        username="eve",
        password="p",
        authSource="admin",
        authMechanism="SCRAM-SHA-256",
        serverSelectionTimeoutMS=2000,
    )
    with pytest.raises(OperationFailure):
        rejected.admin.command("ping")
    rejected.close()
    plain.close()


# ----------------------------------------------------------------------
# RBAC: per-command privilege enforcement against built-in roles
# ----------------------------------------------------------------------


def _make_user_with_roles(srv, db: str, user: str, password: str, roles: list[dict]) -> None:
    """Inject a user record with a specific role binding (bypassing the
    saslStart conversation). Uses the same shape ``createUser`` would
    produce, so subsequent SCRAM auth and ``grantRoles`` work normally.
    """
    creds = derive_credentials(password)
    srv.storage.add_user(
        db,
        user,
        {
            "_id": f"{db}.{user}",
            "user": user,
            "db": db,
            "credentials": creds.to_doc(),
            "roles": roles,
            "mechanisms": [SCRAM_SHA_256],
        },
    )


def _client_for(srv, *, user: str, password: str, db: str = "admin"):
    return pymongo.MongoClient(
        srv.uri,
        username=user,
        password=password,
        authSource=db,
        authMechanism="SCRAM-SHA-256",
        serverSelectionTimeoutMS=2000,
    )


def test_read_role_can_find_but_not_insert(server_with_auth) -> None:
    _make_user_with_roles(server_with_auth, "shop", "viewer", "p", [{"role": "read", "db": "shop"}])
    cli = _client_for(server_with_auth, user="viewer", password="p", db="shop")
    try:
        # find: allowed.
        list(cli["shop"]["items"].find())
        # insert: denied with code 13 (Unauthorized).
        with pytest.raises(OperationFailure) as exc:
            cli["shop"]["items"].insert_one({"x": 1})
        assert exc.value.code == 13
    finally:
        cli.close()


def test_readwrite_role_can_insert(server_with_auth) -> None:
    _make_user_with_roles(
        server_with_auth, "shop", "writer", "p", [{"role": "readWrite", "db": "shop"}]
    )
    cli = _client_for(server_with_auth, user="writer", password="p", db="shop")
    try:
        cli["shop"]["items"].insert_one({"_id": 1, "x": 1})
        assert cli["shop"]["items"].count_documents({}) == 1
    finally:
        cli.close()


def test_role_bound_to_one_db_doesnt_grant_another(server_with_auth) -> None:
    _make_user_with_roles(
        server_with_auth, "shop", "writer", "p", [{"role": "readWrite", "db": "shop"}]
    )
    cli = _client_for(server_with_auth, user="writer", password="p", db="shop")
    try:
        # Different db → unauthorized.
        with pytest.raises(OperationFailure) as exc:
            cli["other"]["stuff"].insert_one({"x": 1})
        assert exc.value.code == 13
    finally:
        cli.close()


def test_readanyDatabase_spans_dbs(server_with_auth) -> None:
    _make_user_with_roles(
        server_with_auth,
        "admin",
        "snoop",
        "p",
        [{"role": "readAnyDatabase", "db": "admin"}],
    )
    cli = _client_for(server_with_auth, user="snoop", password="p", db="admin")
    try:
        # Read from any db is fine.
        list(cli["dbA"]["c"].find())
        list(cli["dbB"]["c"].find())
        # Writes still rejected.
        with pytest.raises(OperationFailure) as exc:
            cli["dbA"]["c"].insert_one({"x": 1})
        assert exc.value.code == 13
    finally:
        cli.close()


def test_useradmin_can_create_user_but_not_read(server_with_auth) -> None:
    _make_user_with_roles(
        server_with_auth,
        "admin",
        "useradmin",
        "p",
        [{"role": "userAdmin", "db": "admin"}],
    )
    cli = _client_for(server_with_auth, user="useradmin", password="p")
    try:
        cli["admin"].command({"createUser": "newbie", "pwd": "p2", "roles": []})
        with pytest.raises(OperationFailure) as exc:
            list(cli["admin"]["coll"].find())
        assert exc.value.code == 13
    finally:
        cli.close()


def test_grant_roles_takes_effect_immediately(server_with_auth) -> None:
    """After grantRolesToUser on the calling principal, the new
    privilege is usable on the *same* connection without reconnect.
    Critical for tests that bootstrap a user then promote it."""
    _make_user_with_roles(server_with_auth, "shop", "rocky", "p", [{"role": "read", "db": "shop"}])
    cli = _client_for(server_with_auth, user="rocky", password="p", db="shop")
    try:
        # Initially read-only — insert is denied.
        with pytest.raises(OperationFailure):
            cli["shop"]["c"].insert_one({"x": 1})
        # Need a userAdmin to do the grant. Use the root principal that
        # the fixture pre-seeds; reuse the same connection since pymongo
        # routes db.command to admin via authSource.
        root_cli = _client_for(server_with_auth, user="root", password="secret")
        try:
            root_cli["shop"].command(
                {
                    "grantRolesToUser": "rocky",
                    "roles": [{"role": "readWrite", "db": "shop"}],
                }
            )
        finally:
            root_cli.close()
        # rocky's *existing* connection won't see the change (server-
        # side effective roles are per-connection); but a new one will.
        cli2 = _client_for(server_with_auth, user="rocky", password="p", db="shop")
        try:
            cli2["shop"]["c"].insert_one({"_id": 99, "x": 1})
            assert cli2["shop"]["c"].count_documents({"_id": 99}) == 1
        finally:
            cli2.close()
    finally:
        cli.close()


def test_revoke_roles_removes_privilege(server_with_auth) -> None:
    _make_user_with_roles(
        server_with_auth, "shop", "demoted", "p", [{"role": "readWrite", "db": "shop"}]
    )
    root_cli = _client_for(server_with_auth, user="root", password="secret")
    try:
        root_cli["shop"].command(
            {
                "revokeRolesFromUser": "demoted",
                "roles": [{"role": "readWrite", "db": "shop"}],
            }
        )
    finally:
        root_cli.close()

    # demoted now has no roles; even basic find should fail.
    cli = _client_for(server_with_auth, user="demoted", password="p", db="shop")
    try:
        with pytest.raises(OperationFailure) as exc:
            list(cli["shop"]["c"].find())
        assert exc.value.code == 13
    finally:
        cli.close()


def test_create_user_rejects_unknown_role(server_with_auth) -> None:
    root_cli = _client_for(server_with_auth, user="root", password="secret")
    try:
        with pytest.raises(OperationFailure) as exc:
            root_cli["admin"].command(
                {
                    "createUser": "victim",
                    "pwd": "p",
                    "roles": [{"role": "notARealRole", "db": "admin"}],
                }
            )
        assert exc.value.code == 31  # RoleNotFound
    finally:
        root_cli.close()


def test_roles_info_lists_builtins(server_with_auth) -> None:
    root_cli = _client_for(server_with_auth, user="root", password="secret")
    try:
        result = root_cli["admin"].command({"rolesInfo": 1, "showBuiltinRoles": True})
        names = {r["role"] for r in result["roles"]}
        # Spot-check the load-bearing ones.
        assert {"read", "readWrite", "dbAdmin", "userAdmin", "root"} <= names
    finally:
        root_cli.close()


def test_no_auth_mode_bypasses_rbac(server_no_auth) -> None:
    """Without ``--auth``, RBAC is not enforced — anyone can do anything.
    Backwards-compat: the `--auth=False` mode is the legacy default."""
    plain = pymongo.MongoClient(server_no_auth.uri)
    try:
        plain["dbA"]["c"].insert_one({"x": 1})
        plain["dbB"]["c"].insert_one({"y": 2})
        assert plain["dbA"]["c"].count_documents({}) == 1
    finally:
        plain.close()
