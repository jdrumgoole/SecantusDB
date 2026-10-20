"""RM-219: `rotateCertificates`, measured against mongod 8.2.11.

With TLS on, mongod re-reads its certificate files and new connections get
the new certificate; a file that does not load is 140 and the certificate in
use stays. With TLS off there is nothing to rotate and it answers `ok`.
"""

from __future__ import annotations

import shutil
import socket
import ssl
import subprocess
from typing import Any

import pymongo
import pytest
from pymongo.errors import PyMongoError

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _answer(fn: Any) -> tuple:
    try:
        got = fn()
        return ("ok", {k: v for k, v in got.items() if k not in ("$clusterTime", "operationTime")})
    except PyMongoError as e:
        return ("err", getattr(e, "code", None), (getattr(e, "details", None) or {}).get("errmsg"))


def _arguments(client: Any) -> list:
    return [
        _answer(lambda: client.admin.command("rotateCertificates")),
        _answer(lambda: client.admin.command({"rotateCertificates": 1, "message": "hi"})),
        _answer(lambda: client.admin.command({"rotateCertificates": 1, "message": 5})),
        _answer(lambda: client.admin.command({"rotateCertificates": 1, "x": 1})),
        _answer(lambda: client.rm219r.command({"rotateCertificates": 1})),
    ]


@pytest.mark.backlog_fixed("RM-219")
def test_rotate_certificates_without_tls_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """No TLS: `ok`; a non-string `message` is 14, an unknown field 40415, a
    database other than `admin` 13."""
    assert _arguments(rust_mongo) == _arguments(mongod_rs)


def _cert(directory: Any, name: str) -> tuple[Any, Any]:
    cert, key = directory / f"{name}.crt", directory / f"{name}.key"
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", str(key), "-out", str(cert), "-days", "2",
            "-subj", f"/CN={name}", "-addext", "subjectAltName=IP:127.0.0.1",
        ],
        check=True,
        capture_output=True,
    )  # fmt: skip
    return cert, key


def _served_cert(port: int) -> bytes:
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection(("127.0.0.1", port)) as raw, ctx.wrap_socket(raw) as tls:
        return tls.getpeercert(binary_form=True) or b""


@pytest.mark.backlog_fixed("RM-219")
def test_rotate_certificates_reloads_the_files(tmp_path: Any) -> None:
    """mongod 8.2.11 (`--tlsMode requireTLS`): a rotation picks up the files
    as they now are; a broken or missing file is 140 "Unable to load PEM from
    '<path>': ...", and connections go on with the certificate in use."""
    if shutil.which("openssl") is None:
        pytest.skip("openssl is needed to make the test certificates")
    first, key = _cert(tmp_path, "first")
    second, second_key = _cert(tmp_path, "second")
    live, live_key = tmp_path / "live.crt", tmp_path / "live.key"
    shutil.copy(first, live)
    shutil.copy(key, live_key)
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(tmp_path / "store"),
         "--tls-cert-file", str(live), "--tls-key-file", str(live_key)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )  # fmt: skip
    try:
        assert proc.stdout is not None
        m = _RS_BANNER.search(proc.stdout.readline())
        assert m, "secantusd-rs did not start"
        drain_output(proc)
        port = int(m.group(2))
        client = pymongo.MongoClient(
            host="127.0.0.1",
            port=port,
            directConnection=True,
            tls=True,
            tlsAllowInvalidCertificates=True,
        )
        before = _served_cert(port)
        shutil.copy(second, live)
        shutil.copy(second_key, live_key)
        assert client.admin.command("rotateCertificates")["ok"] == 1.0
        after = _served_cert(port)
        assert before != after
        live.write_text("garbage")
        broken = _answer(lambda: client.admin.command("rotateCertificates"))
        live.unlink()
        missing = _answer(lambda: client.admin.command("rotateCertificates"))
        assert broken[:2] == ("err", 140) and broken[2].startswith(
            f"Unable to load PEM from '{live}': "
        )
        assert missing == ("err", 140, f"Unable to load PEM from '{live}': Failed opening file")
        assert _served_cert(port) == after
        client.close()
    finally:
        proc.terminate()
        proc.wait(timeout=20)
