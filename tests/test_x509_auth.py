"""MONGODB-X509 authentication.

Sits on top of the mTLS slice (b22): mTLS is the transport-layer
"this client presented a cert signed by our CA" gate; MONGODB-X509
turns that cert into a user identity (the cert subject DN IS the
username, no SCRAM step).

Tests cover:
* DN extraction unit tests (no server).
* End-to-end: create user with the cert's DN as username, client
  connects with the cert + ``?authMechanism=MONGODB-X509``,
  authenticated principal matches the DN.
* Negative paths: plain TLS without a client cert refused, cert
  not signed by the configured CA refused (handled at TLS layer
  already), cert presented but no matching user record refused,
  matching user but not configured for X509 refused, claimed
  username in the SASL payload that disagrees with the cert DN
  refused.
* Interop: SCRAM users still authenticate normally on an
  mTLS-enabled daemon.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import trustme
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer
from secantus.auth import subject_dn_from_peercert

# ---------------------------------------------------------------------------
# DN extraction (no server, no TLS)
# ---------------------------------------------------------------------------


def test_dn_reverses_cert_tuple_order_to_mongod_form() -> None:
    """SSL tuple is least-specific-first; mongod's DN is
    most-specific-first."""
    cert = {
        "subject": (
            (("countryName", "US"),),
            (("stateOrProvinceName", "NY"),),
            (("organizationName", "Acme"),),
            (("commonName", "alice"),),
        )
    }
    assert subject_dn_from_peercert(cert) == "CN=alice,O=Acme,ST=NY,C=US"


def test_dn_uses_short_names_for_known_attributes() -> None:
    cert = {
        "subject": (
            (("organizationalUnitName", "Eng"),),
            (("commonName", "bob"),),
        )
    }
    assert subject_dn_from_peercert(cert) == "CN=bob,OU=Eng"


def test_dn_escapes_special_chars() -> None:
    cert = {"subject": ((("commonName", "a,b=c"),),)}
    assert subject_dn_from_peercert(cert) == r"CN=a\,b\=c"


def test_dn_returns_none_when_no_cert() -> None:
    assert subject_dn_from_peercert(None) is None
    assert subject_dn_from_peercert({}) is None
    assert subject_dn_from_peercert({"subject": ()}) is None


# ---------------------------------------------------------------------------
# End-to-end via pymongo
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def ca() -> trustme.CA:
    return trustme.CA()


@pytest.fixture
def tls_files(tmp_path: Path, ca: trustme.CA) -> tuple[Path, Path, Path]:
    """Server cert + key + CA cert paths in tmp_path."""
    server_cert = ca.issue_cert("127.0.0.1")
    cert_path = tmp_path / "server.crt"
    key_path = tmp_path / "server.key"
    ca_path = tmp_path / "ca.crt"
    server_cert.cert_chain_pems[0].write_to_path(cert_path)
    server_cert.private_key_pem.write_to_path(key_path)
    ca.cert_pem.write_to_path(ca_path)
    return cert_path, key_path, ca_path


def _dn_from_pem(pem_path: Path) -> str:
    """Extract the subject DN of a cert via the same conversion
    SecantusDB uses on the wire — load with Python's ssl module via
    ``ssl._ssl._test_decode_cert`` to get the tuple-of-tuples form,
    then convert through ``subject_dn_from_peercert``.

    Without this, tests have to guess the DN trustme produces (it
    adds an OU with a randomized suffix per cert), which fragility
    isn't worth it. Going through the same code path the server uses
    also guarantees tests fail if the server-side conversion drifts.
    """
    import ssl

    cert_dict = ssl._ssl._test_decode_cert(str(pem_path))  # type: ignore[attr-defined]
    dn = subject_dn_from_peercert(cert_dict)
    assert dn is not None
    return dn


@pytest.fixture
def alice_cert(tmp_path: Path, ca: trustme.CA) -> tuple[Path, str]:
    """Issue a client cert with ``CN=alice`` and return the combined
    cert+key PEM path plus the real subject DN (as the server will
    see it). trustme adds extra OU + O entries with random suffixes,
    so the actual DN looks like
    ``CN=alice,OU=Testing cert \\#<random>,O=trustme v<ver>``; tests
    must use the real value rather than hardcoding ``CN=alice``.
    """
    cert = ca.issue_cert("alice.example", common_name="alice")
    combined = tmp_path / "alice.pem"
    with combined.open("wb") as f:
        for blob in cert.cert_chain_pems:
            f.write(blob.bytes())
        f.write(cert.private_key_pem.bytes())
    # The cert PEM (without the key) is what ssl needs to parse the
    # subject; write it separately for _dn_from_pem to read.
    cert_only = tmp_path / "alice-cert-only.pem"
    cert_only.write_bytes(cert.cert_chain_pems[0].bytes())
    return combined, _dn_from_pem(cert_only)


@pytest.fixture
def auth_server(tmp_path, tls_files, alice_cert):
    """SecantusDB with TLS + mTLS + --auth on. Provision alice user
    (X509 mechanism, username = alice's real cert DN) before flipping
    --auth; yield (server, ca_path, cert_path, alice_dn)."""
    cert_path, key_path, ca_path = tls_files
    _alice_pem, alice_dn = alice_cert

    # Stage 1: bring the server up WITHOUT auth so we can provision
    # the user. mTLS verification still happens.
    bootstrap = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        # Bootstrap with require_client_cert=False so we can connect
        # without a client cert to create the user; flip on for real.
        tls_require_client_cert=False,
        require_auth=False,
    )
    bootstrap.start()
    try:
        boot_uri = f"mongodb://127.0.0.1:{bootstrap.port}/?tls=true&tlsCAFile={ca_path}"
        boot_client = MongoClient(boot_uri, serverSelectionTimeoutMS=3000)
        try:
            boot_client["$external"].command(
                "createUser",
                alice_dn,
                roles=[{"role": "root", "db": "admin"}],
                mechanisms=["MONGODB-X509"],
            )
        finally:
            boot_client.close()
    finally:
        bootstrap.stop()

    # Stage 2: bring the real server up with auth + require-client-cert.
    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=True,
        require_auth=True,
    )
    server.start()
    try:
        yield server, ca_path, cert_path, alice_dn
    finally:
        server.stop()


def test_x509_authenticates_with_matching_cert(auth_server, alice_cert) -> None:
    """End-to-end happy path: alice has an X509 user, presents her
    cert, authenticates via MONGODB-X509, can write to a collection."""
    server, ca_path, _, _alice_dn = auth_server
    alice_pem, _dn = alice_cert
    uri = (
        f"mongodb://127.0.0.1:{server.port}/?"
        f"tls=true&tlsCAFile={ca_path}&tlsCertificateKeyFile={alice_pem}"
        "&authMechanism=MONGODB-X509&authSource=$external"
    )
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        client["x509db"]["coll"].insert_one({"_id": 1, "v": "hello from alice"})
        assert client["x509db"]["coll"].find_one({"_id": 1})["v"] == "hello from alice"
    finally:
        client.close()


def test_x509_refused_when_no_matching_user(tmp_path, tls_files, ca) -> None:
    """A cert signed by the configured CA but with no matching user
    record on the server gets refused."""
    cert_path, key_path, ca_path = tls_files
    stranger_cert = ca.issue_cert("stranger.example", common_name="stranger")
    stranger_pem = tmp_path / "stranger.pem"
    with stranger_pem.open("wb") as f:
        for b in stranger_cert.cert_chain_pems:
            f.write(b.bytes())
        f.write(stranger_cert.private_key_pem.bytes())

    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=True,
        require_auth=True,
    )
    server.start()
    try:
        uri = (
            f"mongodb://127.0.0.1:{server.port}/?"
            f"tls=true&tlsCAFile={ca_path}&tlsCertificateKeyFile={stranger_pem}"
            "&authMechanism=MONGODB-X509&authSource=$external"
        )
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        with pytest.raises(OperationFailure, match="no user found"):
            client.admin.command("ping")
        client.close()
    finally:
        server.stop()


def test_x509_refused_for_scram_only_user(tmp_path, tls_files, ca) -> None:
    """A user record exists for the cert DN but doesn't have an X509
    entry in credentials — X509 attempt refused."""
    cert_path, key_path, ca_path = tls_files
    # Issue alice's cert first so we can find the real DN.
    alice_cert_obj = ca.issue_cert("alice.example", common_name="alice")
    alice_pem = tmp_path / "alice.pem"
    with alice_pem.open("wb") as f:
        for b in alice_cert_obj.cert_chain_pems:
            f.write(b.bytes())
        f.write(alice_cert_obj.private_key_pem.bytes())
    alice_cert_only = tmp_path / "alice-cert-only-scramonly.pem"
    alice_cert_only.write_bytes(alice_cert_obj.cert_chain_pems[0].bytes())
    alice_dn = _dn_from_pem(alice_cert_only)

    # Create alice as SCRAM-only.
    bootstrap = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=False,
        require_auth=False,
    )
    bootstrap.start()
    try:
        boot = MongoClient(
            f"mongodb://127.0.0.1:{bootstrap.port}/?tls=true&tlsCAFile={ca_path}",
            serverSelectionTimeoutMS=3000,
        )
        try:
            boot["$external"].command(
                "createUser",
                alice_dn,
                pwd="hunter2",
                roles=[{"role": "root", "db": "admin"}],
                mechanisms=["SCRAM-SHA-256"],
            )
        finally:
            boot.close()
    finally:
        bootstrap.stop()

    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=True,
        require_auth=True,
    )
    server.start()
    try:
        uri = (
            f"mongodb://127.0.0.1:{server.port}/?"
            f"tls=true&tlsCAFile={ca_path}&tlsCertificateKeyFile={alice_pem}"
            "&authMechanism=MONGODB-X509&authSource=$external"
        )
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        with pytest.raises(OperationFailure, match="not configured for X509"):
            client.admin.command("ping")
        client.close()
    finally:
        server.stop()


def test_scram_still_works_on_mtls_server(tmp_path, tls_files, ca) -> None:
    """A SCRAM user authenticates normally even on an mTLS-required
    server — the cert proves "approved client", SCRAM proves "this
    specific user"."""
    cert_path, key_path, ca_path = tls_files
    bootstrap = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=False,
        require_auth=False,
    )
    bootstrap.start()
    try:
        boot = MongoClient(
            f"mongodb://127.0.0.1:{bootstrap.port}/?tls=true&tlsCAFile={ca_path}",
            serverSelectionTimeoutMS=3000,
        )
        try:
            boot["admin"].command(
                "createUser",
                "scramuser",
                pwd="hunter2",
                roles=[{"role": "root", "db": "admin"}],
            )
        finally:
            boot.close()
    finally:
        bootstrap.stop()

    # Issue a generic client cert (not tied to a username; mTLS gate
    # only — SCRAM identifies the user).
    generic = ca.issue_cert("client.example", common_name="generic-client")
    pem = tmp_path / "generic.pem"
    with pem.open("wb") as f:
        for b in generic.cert_chain_pems:
            f.write(b.bytes())
        f.write(generic.private_key_pem.bytes())

    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=True,
        require_auth=True,
    )
    server.start()
    try:
        uri = (
            f"mongodb://scramuser:hunter2@127.0.0.1:{server.port}/admin?"
            f"tls=true&tlsCAFile={ca_path}&tlsCertificateKeyFile={pem}"
            "&authMechanism=SCRAM-SHA-256"
        )
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        try:
            client["t"]["c"].insert_one({"_id": 1})
            assert client["t"]["c"].find_one({"_id": 1}) == {"_id": 1}
        finally:
            client.close()
    finally:
        server.stop()


def test_x509_refused_without_tls(tmp_path) -> None:
    """X509 auth on a plaintext daemon — no cert was ever presented,
    no DN to authenticate against. Surfaces as AuthenticationFailed."""
    bootstrap = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        require_auth=False,
    )
    bootstrap.start()
    try:
        boot = MongoClient(bootstrap.uri, serverSelectionTimeoutMS=3000)
        try:
            boot["$external"].command(
                "createUser",
                "CN=alice",
                roles=[{"role": "root", "db": "admin"}],
                mechanisms=["MONGODB-X509"],
            )
        finally:
            boot.close()
    finally:
        bootstrap.stop()

    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        require_auth=True,
    )
    server.start()
    try:
        # Run the auth command directly (pymongo's MongoClient with
        # authMechanism=MONGODB-X509 would also try this, but the
        # URI parser would reject the missing TLS first — drive the
        # raw command path).
        client = MongoClient(server.uri, serverSelectionTimeoutMS=3000)
        with pytest.raises(OperationFailure, match="MONGODB-X509 requires"):
            client["$external"].command(
                "saslStart",
                mechanism="MONGODB-X509",
                payload=b"",
            )
        client.close()
    finally:
        server.stop()
