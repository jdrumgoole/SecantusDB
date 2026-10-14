# ruff: noqa: SIM115 -- a probe script: each file is written or read once, and then it exits
"""The login options: PostgreSQL 15 against ``secantusd-pg``.

Five configurations, each a ``pg_hba.conf`` on one side and the options that
stand for it on the other: any connection allowed, TLS only
(``--tls-require``), and the three ``--tls-client-cert`` modes. Arguments
after the binary are passed to every ``secantusd-pg`` it starts; with
``--refuse-unknown-users`` all 219 scenarios are the same, and without it the
48 that differ are a user there is no role for being let in.

Every scenario is one login, made against a throwaway PostgreSQL cluster this
script creates and against ``secantusd-pg``, through psycopg and through a
client written by hand for the messages libpq never sends. The last line
printed is ``N of M the same``; ``tests/test_rust_pg_login_differential.py``
runs all three probes and requires N == M.

It needs a PostgreSQL 15 ``bin`` directory (``initdb``, ``pg_ctl``): the
answers were measured against 15.19, and another major may word them
differently. Without a binary it prints what PostgreSQL answers and compares
nothing, which is how to find out what a new scenario should expect.

Usage::

    python tools/probes/pg_login_options.py /opt/homebrew/opt/postgresql@15/bin \\
        crates/secantus-pg/target/debug/secantusd-pg --refuse-unknown-users
"""

import base64
import hashlib
import hmac
import os
import re
import shutil
import signal
import socket
import ssl
import struct
import subprocess
import sys
import tempfile

import psycopg
import trustme

PGB = sys.argv[1]
BIN = sys.argv[2] if len(sys.argv) > 2 else None
EXTRA = sys.argv[3:]
d = tempfile.mkdtemp(prefix="pgtlsauth-")
ca = trustme.CA()
server = ca.issue_cert("localhost", "127.0.0.1")
crt, key, capem = f"{d}/s.crt", f"{d}/s.key", f"{d}/ca.pem"
server.private_key_pem.write_to_path(key)
os.chmod(key, 0o600)
open(crt, "wb").write(b"".join(b.bytes() for b in server.cert_chain_pems))
ca.cert_pem.write_to_path(capem)


def client_cert(authority, name, cn):
    c = authority.issue_cert(f"{name}@example.com", common_name=cn)
    c.private_key_pem.write_to_path(f"{d}/{name}.key")
    os.chmod(f"{d}/{name}.key", 0o600)
    open(f"{d}/{name}.crt", "wb").write(b"".join(b.bytes() for b in c.cert_chain_pems))
    return {"sslcert": f"{d}/{name}.crt", "sslkey": f"{d}/{name}.key"}


ALICE = client_cert(ca, "alice", "alice")
PGCERT = client_cert(ca, "pguser", "postgres")
STRANGER = client_cert(trustme.CA(), "stranger", "alice")
NOBODY = client_cert(ca, "nobody", "nobody")
NOCERT = {"sslcert": f"{d}/none.crt", "sslkey": f"{d}/none.key"}


# --- a raw client, for the SCRAM exchanges libpq will not send -------------
class Raw:
    def __init__(self, port, tls=True, user="alice", cert=None):
        s = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.cert_hash = None
        if tls:
            s.sendall(struct.pack("!II", 8, 80877103))
            assert s.recv(1) == b"S"
            ctx = ssl.create_default_context(cafile=capem)
            ctx.check_hostname = False
            if cert:
                ctx.load_cert_chain(cert["sslcert"], cert["sslkey"])
            s = ctx.wrap_socket(s)
            self.cert_hash = hashlib.sha256(s.getpeercert(binary_form=True)).digest()
        self.s = s
        body = struct.pack("!I", 196608)
        for k, v in (("user", user), ("database", "postgres")):
            body += k.encode() + b"\0" + v.encode() + b"\0"
        body += b"\0"
        s.sendall(struct.pack("!I", len(body) + 4) + body)

    def read(self):
        head = self._exact(5)
        kind, n = head[:1], struct.unpack("!I", head[1:])[0]
        return kind, self._exact(n - 4)

    def _exact(self, n):
        out = b""
        while len(out) < n:
            chunk = self.s.recv(n - len(out))
            if not chunk:
                raise EOFError(out)
            out += chunk
        return out

    def send(self, kind, body):
        self.s.sendall(kind + struct.pack("!I", len(body) + 4) + body)


def error_fields(body):
    f = {}
    for part in body.split(b"\0"):
        if part:
            f[chr(part[0])] = part[1:].decode()
    return (f.get("S"), f.get("C"), f.get("M"), f.get("D"))


#: What a client sees when the server's close reaches it before the TLS alert
#: that says why.
_CLOSED_FIRST = ("SSLEOFError", "ConnectionResetError", "BrokenPipeError")


def _raw_first_once(port, user, tls=True, cert=None):
    try:
        c = Raw(port, tls=tls, user=user, cert=cert)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        return ("auth", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


def raw_first(port, user, tls=True, cert=None):
    """The server's first answer to a startup: an error's fields, or the request.

    A certificate the server refuses is answered with a TLS alert as it
    closes, and a client now and then reads the close first (PostgreSQL 15.19
    did so once in CI, in `pg_login_tls_files.py`). A close is asked about
    again, on either server, so the answer compared is the alert whenever
    the server sends one."""
    out = _raw_first_once(port, user, tls=tls, cert=cert)
    for _ in range(4):
        if not (out[0] == "client" and out[1] in _CLOSED_FIRST):
            break
        out = _raw_first_once(port, user, tls=tls, cert=cert)
    return out


def raw_scram(
    port, mechanism, gs2, cbind="right", tls=True, password="pw", user="alice", cert=None
):
    """One SCRAM login with the GS2 header and binding data chosen by hand."""
    try:
        c = Raw(port, tls=tls, user=user, cert=cert)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        assert kind == b"R" and struct.unpack("!I", body[:4])[0] == 10, (kind, body)
        offered = [m.decode() for m in body[4:].split(b"\0") if m]
        if mechanism is None:
            return ("offered", offered)
        nonce = base64.b64encode(os.urandom(18)).decode()
        bare = f"n=,r={nonce}"
        first = (gs2 + bare).encode()
        c.send(b"p", mechanism.encode() + b"\0" + struct.pack("!I", len(first)) + first)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        assert struct.unpack("!I", body[:4])[0] == 11, body
        server_first = body[4:].decode()
        attrs = dict(a.split("=", 1) for a in server_first.split(","))
        salt, iters = base64.b64decode(attrs["s"]), int(attrs["i"])
        data = gs2.encode()
        if gs2.startswith("p="):
            data += c.cert_hash if cbind == "right" else b"\x00" * 32
        elif cbind != "right":
            data = cbind.encode()
        without_proof = f"c={base64.b64encode(data).decode()},r={attrs['r']}"
        salted = hashlib.pbkdf2_hmac("sha256", password.encode(), salt, iters)
        client_key = hmac.new(salted, b"Client Key", "sha256").digest()
        stored = hashlib.sha256(client_key).digest()
        message = f"{bare},{server_first},{without_proof}".encode()
        sig = hmac.new(stored, message, "sha256").digest()
        proof = bytes(a ^ b for a, b in zip(client_key, sig, strict=True))
        c.send(b"p", f"{without_proof},p={base64.b64encode(proof).decode()}".encode())
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        assert struct.unpack("!I", body[:4])[0] == 12, body
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        return ("ok", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


def attempt(port, user="postgres", query=None, **kw):
    kw.setdefault("sslrootcert", capem)
    for k, v in NOCERT.items():
        kw.setdefault(k, v)
    try:
        with psycopg.connect(
            host="127.0.0.1",
            port=port,
            dbname="postgres",
            user=user,
            connect_timeout=5,
            autocommit=True,
            **kw,
        ) as c:
            if query:
                return ("ok", c.execute(query).fetchall())
            return ("ok", bool(c.pgconn.ssl_in_use))
    except psycopg.Error as e:
        msg = str(e).strip().splitlines()[-1].split("failed: ", 1)[-1]
        return (
            type(e).__name__,
            getattr(e, "sqlstate", None),
            re.sub(r"\d{4,5}", "PORT", msg)[:200],
        )


SSLROW = (
    "select ssl, version, cipher, bits, client_dn, client_serial is not null, issuer_dn "
    "from pg_stat_ssl where pid = pg_backend_pid()"
)


def setup(port):
    with psycopg.connect(
        host="127.0.0.1",
        port=port,
        dbname="postgres",
        user="postgres",
        autocommit=True,
        sslmode="require",
        **PGCERT,
    ) as c:
        c.execute("drop role if exists alice")
        c.execute("create role alice login password 'pw'")
        c.execute("drop role if exists bob")
        c.execute("create role bob login password 'pw'")
        c.execute("drop role if exists carol")
        c.execute("create role carol login")


def scenarios(port, config):
    out = {}
    tls = {"sslmode": "require"}
    pw = {"password": "pw"}
    out["plain postgres"] = attempt(port, sslmode="disable")
    out["plain alice"] = attempt(port, "alice", sslmode="disable", **pw)
    out["tls postgres, no cert"] = attempt(port, **tls)
    out["tls alice pw, no cert"] = attempt(port, "alice", **tls, **pw)
    out["tls alice pw, cert alice"] = attempt(port, "alice", **tls, **pw, **ALICE)
    out["tls alice no pw, cert alice"] = attempt(port, "alice", **tls, **ALICE)
    out["tls bob pw, cert alice"] = attempt(port, "bob", **tls, **pw, **ALICE)
    out["tls bob bad pw, cert alice"] = attempt(port, "bob", **tls, password="no", **ALICE)
    out["tls carol, cert alice"] = attempt(port, "carol", **tls, **ALICE)
    out["tls carol, no cert"] = attempt(port, "carol", **tls)
    out["tls nobody, cert alice"] = attempt(port, "nobody", **tls, **ALICE)
    out["plain nobody pw"] = attempt(port, "nobody", sslmode="disable", **pw)
    out["tls nobody pw, no cert"] = attempt(port, "nobody", **tls, **pw)
    out["tls nobody pw, cert alice"] = attempt(port, "nobody", **tls, **pw, **ALICE)
    out["tls nobody, cert nobody"] = attempt(port, "nobody", **tls, **NOBODY)
    out["tls nobody pw, cert nobody"] = attempt(port, "nobody", **tls, **pw, **NOBODY)
    out["first: plain nobody"] = raw_first(port, "nobody", tls=False)
    out["first: tls nobody, no cert"] = raw_first(port, "nobody")
    out["first: tls nobody, cert nobody"] = raw_first(port, "nobody", cert=NOBODY)
    out["scram: nobody pw, cert nobody"] = raw_scram(
        port, "SCRAM-SHA-256", "n,,", user="nobody", cert=NOBODY
    )
    out["scram plus: nobody pw, cert nobody"] = raw_scram(
        port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,", user="nobody", cert=NOBODY
    )
    out["scram plus wrong data: nobody"] = raw_scram(
        port,
        "SCRAM-SHA-256-PLUS",
        "p=tls-server-end-point,,",
        cbind="wrong",
        user="nobody",
        cert=NOBODY,
    )
    out["scram y downgrade: nobody"] = raw_scram(
        port, "SCRAM-SHA-256", "y,,", user="nobody", cert=NOBODY
    )
    out["scram offered: nobody"] = raw_scram(port, None, "", user="nobody", cert=NOBODY)
    out["tls postgres, cert postgres"] = attempt(port, **tls, **PGCERT)
    out["tls alice pw, stranger cert"] = attempt(port, "alice", **tls, **pw, **STRANGER)
    out["first: plain postgres"] = raw_first(port, "postgres", tls=False)
    out["first: tls postgres, no cert"] = raw_first(port, "postgres")
    out["first: tls carol, cert alice"] = raw_first(port, "carol", cert=ALICE)
    out["first: tls carol, cert carol-less"] = raw_first(port, "carol", cert=PGCERT)
    out["first: tls alice, cert alice"] = raw_first(port, "alice", cert=ALICE)
    out["first: tls bob, cert alice"] = raw_first(port, "bob", cert=ALICE)
    out["first: tls postgres, stranger"] = raw_first(port, "postgres", cert=STRANGER)[:2]
    plus = ("SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,")
    out["scram: bob pw, cert alice"] = raw_scram(port, *plus, user="bob", cert=ALICE)
    out["scram: alice pw, cert alice"] = raw_scram(port, *plus, cert=ALICE)
    if config in ("open", "tls-only"):
        out["cb require alice"] = attempt(port, "alice", **tls, **pw, channel_binding="require")
        out["cb disable alice"] = attempt(port, "alice", **tls, **pw, channel_binding="disable")
        out["stat no cert"] = attempt(port, query=SSLROW, **tls)
        out["stat cert alice"] = attempt(port, "alice", query=SSLROW, **tls, **pw, **ALICE)
        out["stat tls12"] = attempt(port, query=SSLROW, **tls, ssl_max_protocol_version="TLSv1.2")
        out["stat dn"] = attempt(
            port,
            "alice",
            query="select client_dn, issuer_dn from pg_stat_ssl where pid = pg_backend_pid()",
            **tls,
            **pw,
            **ALICE,
        )
        # Count only this session's TLS row: sessions an earlier step closed
        # can still be listed by PostgreSQL while their backends exit, which
        # made the count 3 against 1 once in CI.
        out["stat others"] = attempt(
            port,
            query=(
                "select count(*) >= 1, count(*) filter (where ssl and pid = pg_backend_pid())"
                " from pg_stat_ssl"
            ),
            **tls,
        )
        out["raw offered tls"] = raw_scram(port, None, "")
        out["raw plus right"] = raw_scram(port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,")
        out["raw plain n"] = raw_scram(port, "SCRAM-SHA-256", "n,,")
        out["raw plain y (downgrade)"] = raw_scram(port, "SCRAM-SHA-256", "y,,")
        out["raw plus with n"] = raw_scram(port, "SCRAM-SHA-256-PLUS", "n,,")
        out["raw plus with y"] = raw_scram(port, "SCRAM-SHA-256-PLUS", "y,,")
        out["raw plain with p"] = raw_scram(port, "SCRAM-SHA-256", "p=tls-server-end-point,,")
        out["raw plus tls-unique"] = raw_scram(port, "SCRAM-SHA-256-PLUS", "p=tls-unique,,")
        out["raw plus wrong data"] = raw_scram(
            port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,", cbind="wrong"
        )
        out["raw plain n, c=y"] = raw_scram(port, "SCRAM-SHA-256", "n,,", cbind="y,,")
        out["raw plain n, c=junk"] = raw_scram(port, "SCRAM-SHA-256", "n,,", cbind="zz")
        out["raw unknown mechanism"] = raw_scram(port, "SCRAM-SHA-1", "n,,")
        out["raw plus bad pw"] = raw_scram(
            port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,", password="no"
        )
    if config == "open":
        out["stat plain"] = attempt(port, query=SSLROW, sslmode="disable")
        out["raw offered plain"] = raw_scram(port, None, "", tls=False)
        out["raw plain y, no tls"] = raw_scram(port, "SCRAM-SHA-256", "y,,", tls=False)
        out["raw plus, no tls"] = raw_scram(
            port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,", tls=False
        )
    return out


# config -> (pg_hba lines, secantusd-pg flags)
CONFIGS = {
    "open": (
        "host all postgres 127.0.0.1/32 trust\nhost all carol 127.0.0.1/32 trust\n"
        "host all all 127.0.0.1/32 scram-sha-256\n",
        [],
    ),
    "tls-only": (
        "hostssl all postgres 127.0.0.1/32 trust\nhostssl all carol 127.0.0.1/32 trust\n"
        "hostssl all all 127.0.0.1/32 scram-sha-256\n",
        ["--tls-require"],
    ),
    "verify-ca": (
        "hostssl all postgres 127.0.0.1/32 trust clientcert=verify-ca\n"
        "hostssl all carol 127.0.0.1/32 trust clientcert=verify-ca\n"
        "hostssl all all 127.0.0.1/32 scram-sha-256 clientcert=verify-ca\n",
        ["--tls-client-cert", "verify-ca"],
    ),
    "verify-full": (
        "hostssl all postgres 127.0.0.1/32 trust clientcert=verify-full\n"
        "hostssl all carol 127.0.0.1/32 trust clientcert=verify-full\n"
        "hostssl all all 127.0.0.1/32 scram-sha-256 clientcert=verify-full\n",
        ["--tls-client-cert", "verify-full"],
    ),
    "cert": ("hostssl all all 127.0.0.1/32 cert\n", ["--tls-client-cert", "cert"]),
}

pgd = f"{d}/pgdata"
subprocess.run(
    [f"{PGB}/initdb", "-D", pgd, "-U", "postgres", "--auth=trust", "-E", "UTF8"],
    check=True,
    capture_output=True,
)
shutil.copy(crt, f"{pgd}/server.crt")
shutil.copy(key, f"{pgd}/server.key")
os.chmod(f"{pgd}/server.key", 0o600)
shutil.copy(capem, f"{pgd}/root.crt")
open(f"{pgd}/postgresql.conf", "a").write(
    "\nssl = on\nssl_ca_file = 'root.crt'\nport = 5417\nlisten_addresses = '127.0.0.1'\n"
    "unix_socket_directories = ''\npassword_encryption = 'scram-sha-256'\n"
)
ref, ours = {}, {}
first = True
for name, (hba, _flags) in CONFIGS.items():
    open(f"{pgd}/pg_hba.conf", "w").write(CONFIGS["open"][0] if first else hba)
    subprocess.run(
        [f"{PGB}/pg_ctl", "-D", pgd, "-l", f"{d}/pg.log", "-w", "start" if first else "reload"],
        check=True,
        capture_output=True,
    )
    if first:
        setup(5417)
        with psycopg.connect(host="127.0.0.1", port=5417, dbname="postgres", user="postgres") as c:
            print("reference:", c.execute("show server_version").fetchone()[0])
        first = False
        open(f"{pgd}/pg_hba.conf", "w").write(hba)
        subprocess.run([f"{PGB}/pg_ctl", "-D", pgd, "reload"], check=True, capture_output=True)
    ref[name] = scenarios(5417, name)
subprocess.run(
    [f"{PGB}/pg_ctl", "-D", pgd, "-m", "fast", "-w", "stop"], check=True, capture_output=True
)

if BIN:
    home = f"{d}/home"
    base = [
        BIN,
        home,
        "127.0.0.1:0",
        "--tls-cert-file",
        crt,
        "--tls-key-file",
        key,
        "--tls-ca-file",
        capem,
    ]

    def start(flags):
        p = subprocess.Popen(
            base + flags, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
        )
        line = p.stdout.readline()
        m = re.search(r"listening on \S+:(\d+)", line)
        if not m:
            raise SystemExit(f"server did not start with {flags}: {line}{p.stdout.read()}")
        return p, int(m.group(1))

    p, port = start([])
    setup(port)
    p.send_signal(signal.SIGTERM)
    p.wait(30)
    for name, (_, flags) in CONFIGS.items():
        p, port = start(flags + EXTRA)
        try:
            ours[name] = scenarios(port, name)
        finally:
            p.send_signal(signal.SIGTERM)
            p.wait(30)

same = total = 0
for name in CONFIGS:
    print(f"\n== {name}")
    for k, v in ref[name].items():
        total += 1
        if not BIN:
            print("  ", k, "->", v)
        elif ours[name].get(k) == v:
            same += 1
            print("  SAME", k, v)
        else:
            print("  DIFF", k, "\n      pg  :", v, "\n      ours:", ours[name].get(k))
if BIN:
    print(f"\n{same} of {total} the same")
shutil.rmtree(d)
