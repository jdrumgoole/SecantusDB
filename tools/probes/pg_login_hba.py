# ruff: noqa: SIM115 -- a probe script: each file is written or read once, and then it exits
"""``pg_hba.conf``: PostgreSQL 15 against ``secantusd-pg --hba-file``.

Two files, the second delivered by a reload. Between them: every line type,
method, database form and user form the server reads, an address that does
not match, ``clientcert=``, ``map=``, md5-stored passwords under ``md5``,
``scram-sha-256`` and ``password`` lines, a role with no password, a user
with no role, and a database that is not there. 118 scenarios.

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

    python tools/probes/pg_login_hba.py /opt/homebrew/opt/postgresql@15/bin \\
        crates/secantus-pg/target/debug/secantusd-pg
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
import time

import psycopg
import trustme

PGB = sys.argv[1]
BIN = sys.argv[2] if len(sys.argv) > 2 else None
d = tempfile.mkdtemp(prefix="pghba-")
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
    def __init__(self, port, tls=True, user="alice", cert=None, database="postgres"):
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
        for k, v in (("user", user), ("database", database)):
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


def raw_first(port, user, tls=True, cert=None, database="postgres"):
    """The server's first answer to a startup: an error's fields, or the request."""
    try:
        c = Raw(port, tls=tls, user=user, cert=cert, database=database)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        return ("auth", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


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


def attempt(port, user="postgres", query=None, database="postgres", **kw):
    kw.setdefault("sslrootcert", capem)
    for k, v in NOCERT.items():
        kw.setdefault(k, v)
    try:
        with psycopg.connect(
            host=kw.pop("host", "127.0.0.1"),
            port=port,
            dbname=database,
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


def cleartext(port, user, password, tls=False):
    """Answer an AuthenticationCleartextPassword by hand, to see the whole exchange."""
    try:
        c = Raw(port, tls=tls, user=user)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        code = struct.unpack("!I", body[:4])[0]
        if code != 3:
            return ("auth", code)
        c.send(b"p", password.encode() + b"\0")
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        return ("auth after password", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


def setup(port, connect):
    with connect() as c:
        for role in (
            "alice",
            "bob",
            "carol",
            "dave",
            "erin",
            "frank",
            "team",
            "member",
            "appuser",
            "md5user",
            "nopw",
        ):
            c.execute(f"drop role if exists {role}")
        for db in ("appdb", "alice", "team"):
            c.execute(f"drop database if exists {db}")
        c.execute("create role alice login password 'pw'")
        c.execute("create role bob login password 'pw'")
        c.execute("create role carol login")
        c.execute("create role dave login password 'pw'")
        c.execute("create role erin login password 'pw'")
        c.execute("create role frank login password 'pw'")
        c.execute("create role nopw login")
        c.execute("create role team")
        c.execute("create role member login in role team")
        c.execute("create role appuser login password 'pw'")
        for role in ("md5user", "md5scram", "md5plain"):
            c.execute(f"drop role if exists {role}")
            digest = hashlib.md5(("pw" + role).encode()).hexdigest()
            c.execute(f"create role {role} login password 'md5{digest}'")
        c.execute("create database appdb")
        c.execute("create database alice")
        c.execute("create database team")


TLS = {"sslmode": "require"}
PLAIN = {"sslmode": "disable"}
HBA = """\
# TYPE      DATABASE   USER        ADDRESS                    METHOD
host        all        postgres    127.0.0.1/32               trust
hostnossl   all        carol       127.0.0.1/32               trust
hostssl     all        carol       127.0.0.1/32               reject
host        appdb      appuser     127.0.0.1/32               scram-sha-256
host        all        rejected    127.0.0.1/32               reject
host        all        alice,bob   127.0.0.1/32               scram-sha-256
host        all        +team       127.0.0.1/32               trust
host        sameuser   erin        127.0.0.1/32               trust
host        samerole   frank       127.0.0.1/32               trust
host        all        dave        10.0.0.0/8                 trust
host        all        dave        127.0.0.1  255.255.255.255 password
host        all        erin        samehost                   md5
hostssl     all        frank       all                        scram-sha-256  clientcert=verify-ca
host        all        nopw        127.0.0.1/32               scram-sha-256
host        "appdb"    "nobody"    127.0.0.1/32               trust
"""


def scenarios(port):
    out = {}

    def both(label, **kw):
        out[f"{label}, plain"] = attempt(port, **PLAIN, **kw)
        out[f"{label}, tls"] = attempt(port, **TLS, **kw)

    both("postgres")
    both("postgres in appdb", database="appdb")
    both("carol (hostnossl trust, hostssl reject)", user="carol")
    both("appuser in appdb, pw", user="appuser", database="appdb", password="pw")
    both("appuser in appdb, wrong pw", user="appuser", database="appdb", password="no")
    both("appuser in postgres, pw (no line)", user="appuser", password="pw")
    both("rejected", user="rejected")
    both("rejected, pw", user="rejected", password="pw")
    both("alice pw", user="alice", password="pw")
    both("alice no pw", user="alice")
    both("bob pw", user="bob", password="pw")
    both("member (+team)", user="member")
    both("team itself (no login)", user="team")
    both("erin in db erin? none: erin in postgres, pw (md5 line)", user="erin", password="pw")
    both("erin wrong pw", user="erin", password="no")
    both("erin no pw", user="erin")
    both("frank in db team (samerole: not a member)", user="frank", database="team", password="pw")
    both("member in db team", user="member", database="team")
    both(
        "alice in db alice (sameuser is erin's line)", user="alice", database="alice", password="pw"
    )
    both("dave pw (password method)", user="dave", password="pw")
    both("dave wrong pw", user="dave", password="no")
    both("dave no pw", user="dave")
    both("frank pw, no cert", user="frank", password="pw")
    out["frank pw, cert alice, tls"] = attempt(port, user="frank", password="pw", **TLS, **ALICE)
    out["frank pw, stranger cert, tls"] = attempt(
        port, user="frank", password="pw", **TLS, **STRANGER
    )
    both("nopw (scram line, role has no password)", user="nopw")
    both("nopw, a password", user="nopw", password="pw")
    both("nobody in appdb (trust line, no role)", user="nobody", database="appdb")
    both("nobody in postgres (no line)", user="nobody")
    both("alice in a database that is not there", user="alice", password="pw", database="absent")
    both("postgres in a database that is not there", database="absent")
    for label, kw in (
        ("postgres", {}),
        ("carol", {"user": "carol"}),
        ("rejected", {"user": "rejected"}),
        ("alice", {"user": "alice"}),
        ("dave", {"user": "dave"}),
        ("erin", {"user": "erin"}),
        ("nopw", {"user": "nopw"}),
        ("nobody", {"user": "nobody"}),
        ("appuser in postgres", {"user": "appuser"}),
        ("frank", {"user": "frank"}),
    ):
        user = kw.get("user", "postgres")
        out[f"first answer: {label}, plain"] = raw_first(port, user, tls=False)
        out[f"first answer: {label}, tls"] = raw_first(port, user, tls=True)
    out["first answer: team (trust, no login), plain"] = raw_first(port, "team", tls=False)
    out["first answer: nobody in appdb (trust, no role), plain"] = raw_first(
        port, "nobody", tls=False, database="appdb"
    )
    out["first answer: alice in absent db, plain"] = raw_first(
        port, "alice", tls=False, database="absent"
    )
    out["first answer: postgres in absent db, plain"] = raw_first(
        port, "postgres", tls=False, database="absent"
    )
    out["dave cleartext pw, by hand"] = cleartext(port, "dave", "pw")
    out["dave cleartext wrong pw, by hand"] = cleartext(port, "dave", "no")
    out["dave cleartext empty pw, by hand"] = cleartext(port, "dave", "")
    out["scram: alice, plain"] = raw_scram(port, "SCRAM-SHA-256", "n,,", tls=False)
    out["scram: erin (md5 line), plain"] = raw_scram(
        port, "SCRAM-SHA-256", "n,,", tls=False, user="erin"
    )
    out["scram: nopw, plain"] = raw_scram(port, "SCRAM-SHA-256", "n,,", tls=False, user="nopw")
    out["scram offered: alice, tls"] = raw_scram(port, None, None, tls=True)
    return out


ALICECERT = client_cert(ca, "alicecert", "alicecert")
BOBCERT = client_cert(ca, "bobcert", "bob")
CAROLCERT = client_cert(ca, "carolcert", "carol")
IDENT = "m alicecert alice\n"
HBA2 = """\
host        all        postgres    127.0.0.1/32   trust
host        all        md5user     127.0.0.1/32   md5
host        all        md5scram    127.0.0.1/32   scram-sha-256
host        all        md5plain    127.0.0.1/32   password
hostssl     all        alice       127.0.0.1/32   cert map=m
hostssl     all        bob         127.0.0.1/32   scram-sha-256 clientcert=verify-full
hostssl     all        all         127.0.0.1/32   cert
"""


def scenarios2(port):
    out = {}
    for user in ("md5user", "md5scram", "md5plain"):
        out[f"{user} pw, plain"] = attempt(port, user=user, password="pw", **PLAIN)
        out[f"{user} wrong pw, plain"] = attempt(port, user=user, password="no", **PLAIN)
        out[f"{user} no pw, plain"] = attempt(port, user=user, **PLAIN)
        out[f"first answer: {user}, plain"] = raw_first(port, user, tls=False)[:2]
    out["md5plain cleartext pw, by hand"] = cleartext(port, "md5plain", "pw")
    out["alice, cert alicecert (map)"] = attempt(port, user="alice", **TLS, **ALICECERT)
    out["alice, cert alice (not in the map)"] = attempt(port, user="alice", **TLS, **ALICE)
    out["alice, no cert"] = attempt(port, user="alice", **TLS)
    out["alice, plain (no line)"] = attempt(port, user="alice", **PLAIN)
    out["bob pw, cert bob"] = attempt(port, user="bob", password="pw", **TLS, **BOBCERT)
    out["bob pw, cert alice"] = attempt(port, user="bob", password="pw", **TLS, **ALICE)
    out["bob wrong pw, cert bob"] = attempt(port, user="bob", password="no", **TLS, **BOBCERT)
    out["bob pw, no cert"] = attempt(port, user="bob", password="pw", **TLS)
    out["carol, cert carol"] = attempt(port, user="carol", **TLS, **CAROLCERT)
    out["carol, cert alice"] = attempt(port, user="carol", **TLS, **ALICE)
    out["carol, plain (no line)"] = attempt(port, user="carol", **PLAIN)
    out["nobody, cert nobody"] = attempt(port, user="nobody", **TLS, **NOBODY)
    out["team, cert stranger"] = attempt(port, user="team", **TLS, **STRANGER)
    out["first answer: nobody with its cert"] = raw_first(port, "nobody", tls=True, cert=NOBODY)
    return out


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
    "\nssl = on\nssl_ca_file = 'root.crt'\nport = 5419\nlisten_addresses = '127.0.0.1'\n"
    "unix_socket_directories = ''\npassword_encryption = 'scram-sha-256'\n"
)
open(f"{pgd}/pg_hba.conf", "w").write("host all all 127.0.0.1/32 trust\n")
subprocess.run(
    [f"{PGB}/pg_ctl", "-D", pgd, "-l", f"{d}/pg.log", "-w", "start"],
    check=True,
    capture_output=True,
)
setup(
    5419,
    lambda: psycopg.connect(
        host="127.0.0.1", port=5419, dbname="postgres", user="postgres", autocommit=True
    ),
)
with psycopg.connect(host="127.0.0.1", port=5419, dbname="postgres", user="postgres") as c:
    print("reference:", c.execute("show server_version").fetchone()[0])
open(f"{pgd}/pg_hba.conf", "w").write(HBA)
subprocess.run([f"{PGB}/pg_ctl", "-D", pgd, "reload"], check=True, capture_output=True)
time.sleep(1)
assert attempt(5419, **PLAIN) == ("ok", False), "self-check: postgres is trusted in the clear"
ref = scenarios(5419)
open(f"{pgd}/pg_hba.conf", "w").write(HBA2)
open(f"{pgd}/pg_ident.conf", "w").write(IDENT)
subprocess.run([f"{PGB}/pg_ctl", "-D", pgd, "reload"], check=True, capture_output=True)
time.sleep(1)
ref.update({f"[2] {k}": v for k, v in scenarios2(5419).items()})
subprocess.run(
    [f"{PGB}/pg_ctl", "-D", pgd, "-m", "fast", "-w", "stop"], check=True, capture_output=True
)

ours = {}
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
    setup(
        port,
        lambda: psycopg.connect(
            host="127.0.0.1", port=port, dbname="postgres", user="postgres", autocommit=True
        ),
    )
    p.send_signal(signal.SIGTERM)
    p.wait(30)
    open(f"{d}/pg_hba.conf", "w").write(HBA)
    open(f"{d}/pg_ident.conf", "w").write("")
    p, port = start(["--hba-file", f"{d}/pg_hba.conf", "--ident-file", f"{d}/pg_ident.conf"])
    try:
        ours = scenarios(port)
        # The second file arrives by a reload, as it did for PostgreSQL.
        open(f"{d}/pg_hba.conf", "w").write(HBA2)
        open(f"{d}/pg_ident.conf", "w").write(IDENT)
        p.send_signal(signal.SIGHUP)
        time.sleep(1)
        ours.update({f"[2] {k}": v for k, v in scenarios2(port).items()})
    finally:
        p.send_signal(signal.SIGTERM)
        p.wait(30)

same = 0
for k, v in ref.items():
    if not BIN:
        print("  ", k, "->", v)
    elif ours.get(k) == v:
        same += 1
        print("  SAME", k, v)
    else:
        print("  DIFF", k, "\n      pg  :", v, "\n      ours:", ours.get(k))
if BIN:
    print(f"\n{same} of {len(ref)} the same")
shutil.rmtree(d)
