# ruff: noqa: SIM115 -- a probe script: each file is written or read once, and then it exits
"""The TLS files: PostgreSQL 15 against ``secantusd-pg``.

Revocation lists (current, empty, expired, dated in the future, from another
CA, forged, with and without extensions, through an intermediate CA), user
name maps by exact name and by pattern, ``clientname=DN``, server
certificates whose signature is RSA-PSS or Ed25519, and what a reload picks
up. 513 scenarios. Arguments after the binary name the groups to run; with
none, all of them.

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

    python tools/probes/pg_login_tls_files.py /opt/homebrew/opt/postgresql@15/bin \\
        crates/secantus-pg/target/debug/secantusd-pg
"""

import base64
import datetime
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
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, padding, rsa
from cryptography.x509.oid import NameOID

PGB = sys.argv[1]
BIN = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] != "-" else None
ONLY = set(sys.argv[3:])
d = tempfile.mkdtemp(prefix="pgtlsgaps-")
NOW = datetime.datetime.now(datetime.timezone.utc)
DAY = datetime.timedelta(days=1)
PORT = 5418


# --- certificates -----------------------------------------------------------
class Authority:
    def __init__(self, cn, kind="rsa", pss_hash=None, parent=None):
        self.parent = parent
        self.key = (
            ed25519.Ed25519PrivateKey.generate()
            if kind == "ed25519"
            else rsa.generate_private_key(65537, 2048)
        )
        self.kind, self.pss_hash = kind, pss_hash
        self.name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])
        self.cert = (parent or self)._sign(
            x509.CertificateBuilder()
            .subject_name(self.name)
            .issuer_name((parent or self).name)
            .public_key(self.key.public_key())
            .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
            .add_extension(
                x509.KeyUsage(True, False, False, False, False, True, True, False, False),
                critical=True,
            )
        )

    def _sign_args(self):
        if self.kind == "ed25519":
            return {"algorithm": None}
        if self.pss_hash:
            return {
                "algorithm": self.pss_hash,
                "rsa_padding": padding.PSS(
                    mgf=padding.MGF1(self.pss_hash), salt_length=padding.PSS.DIGEST_LENGTH
                ),
            }
        return {"algorithm": hashes.SHA256()}

    def _sign(self, builder):
        return (
            builder.serial_number(x509.random_serial_number())
            .not_valid_before(NOW - DAY)
            .not_valid_after(NOW + 30 * DAY)
            .sign(self.key, **self._sign_args())
        )

    def issue(self, stem, subject, san=None, crldp=None):
        """`subject` is [(oid, value)] in the order the certificate holds it;
        `crldp` a URI for a CRL distribution points extension."""
        key = rsa.generate_private_key(65537, 2048)
        b = (
            x509.CertificateBuilder()
            .subject_name(x509.Name([x509.NameAttribute(o, v) for o, v in subject]))
            .issuer_name(self.name)
            .public_key(key.public_key())
        )
        if crldp:
            b = b.add_extension(
                x509.CRLDistributionPoints(
                    [
                        x509.DistributionPoint(
                            [x509.UniformResourceIdentifier(crldp)], None, None, None
                        )
                    ]
                ),
                critical=False,
            )
        if san:
            b = b.add_extension(
                x509.SubjectAlternativeName([x509.DNSName("localhost")]), critical=False
            )
        cert = self._sign(b)
        chain = cert.public_bytes(serialization.Encoding.PEM)
        if self.parent:
            chain += self.cert.public_bytes(serialization.Encoding.PEM)
        open(f"{d}/{stem}.crt", "wb").write(chain)
        open(f"{d}/{stem}.key", "wb").write(
            key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        )
        os.chmod(f"{d}/{stem}.key", 0o600)
        self.issued = getattr(self, "issued", {})
        self.issued[stem] = cert
        return {"sslcert": f"{d}/{stem}.crt", "sslkey": f"{d}/{stem}.key"}

    def pem(self, path):
        open(path, "wb").write(self.cert.public_bytes(serialization.Encoding.PEM))
        return path

    def crl(
        self,
        path,
        revoked=(),
        next_update=None,
        last_update=None,
        v1=False,
        serials=(),
        extensions=(),
    ):
        b = (
            x509.CertificateRevocationListBuilder()
            .issuer_name(self.name)
            .last_update(last_update or NOW - DAY)
            .next_update(next_update or NOW + 30 * DAY)
        )
        if not v1:
            b = b.add_extension(x509.CRLNumber(1), critical=False).add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(self.key.public_key()),
                critical=False,
            )
        for extension, critical in extensions:
            b = b.add_extension(extension, critical=critical)
        for stem in revoked:
            b = b.add_revoked_certificate(
                x509.RevokedCertificateBuilder()
                .serial_number(self.issued[stem].serial_number)
                .revocation_date(NOW - DAY)
                .build()
            )
        for serial in serials:
            b = b.add_revoked_certificate(
                x509.RevokedCertificateBuilder()
                .serial_number(serial)
                .revocation_date(NOW - DAY)
                .build()
            )
        open(path, "wb").write(
            b.sign(self.key, **self._sign_args()).public_bytes(serialization.Encoding.PEM)
        )
        return path


def cn(value):
    return [(NameOID.COMMON_NAME, value)]


ca = Authority("gaps client CA")
other_ca = Authority("another CA")
capem = ca.pem(f"{d}/ca.pem")
SERVER = ca.issue("s", cn("localhost"), san=True)
SERVER2 = ca.issue("s2", cn("localhost"), san=True)
PGCERT = ca.issue("pguser", cn("postgres"))
ALICE = ca.issue("alice", cn("alice"))
REVOKED = ca.issue("revoked", cn("postgres"))
ALICE_REVOKED = ca.issue("alice_revoked", cn("alice"))
ALICE_CERT = ca.issue("alice_cert", cn("alice_cert"))
SHARED = ca.issue("shared", cn("shared"))
BOBCERT = ca.issue("bobcert", cn("bobcert"))
CAROL_MAIL = ca.issue("carol_mail", cn("carol@example.com"))
PG_MAIL = ca.issue("pg_mail", cn("postgres@example.com"))
ALICE_DN = ca.issue(
    "alice_dn", [(NameOID.ORGANIZATION_NAME, "Example"), (NameOID.COMMON_NAME, "alice")]
)
ALICE_DN3 = ca.issue(
    "alice_dn3",
    [
        (NameOID.COUNTRY_NAME, "IE"),
        (NameOID.ORGANIZATION_NAME, "Ex, ample"),
        (NameOID.ORGANIZATIONAL_UNIT_NAME, "Eng"),
        (NameOID.COMMON_NAME, "alice"),
    ],
)
NOCERT = {"sslcert": f"{d}/none.crt", "sslkey": f"{d}/none.key"}
CRL = ca.crl(f"{d}/crl.pem", revoked=("revoked", "alice_revoked"))
CRL_EMPTY = ca.crl(f"{d}/crl_empty.pem")
CRL_EXPIRED = ca.crl(
    f"{d}/crl_expired.pem",
    revoked=("revoked",),
    last_update=NOW - 10 * DAY,
    next_update=NOW - 2 * DAY,
)
CRL_OTHER = other_ca.crl(f"{d}/crl_other.pem")
CRL_V1 = ca.crl(f"{d}/crl_v1.pem", revoked=("revoked", "alice_revoked"), v1=True)

# An intermediate CA under the client CA, and a CA that only shares its name.
inter = Authority("gaps intermediate", parent=ca)
INTER_GOOD = inter.issue("inter_good", cn("postgres"))
INTER_REVOKED = inter.issue("inter_revoked", cn("postgres"))
forger = Authority("gaps client CA")


def bundle(path, *parts):
    open(path, "wb").write(b"".join(open(part, "rb").read() for part in parts))
    return path


CRL_INTER = inter.crl(f"{d}/crl_inter.pem", revoked=("inter_revoked",))
CRL_ROOT_ONLY = CRL
CRL_BOTH = bundle(f"{d}/crl_both.pem", CRL, CRL_INTER)
CRL_INTER_ONLY = CRL_INTER
CRL_ROOT_REVOKES_INTER = bundle(
    f"{d}/crl_root_revokes_inter.pem",
    ca.crl(f"{d}/crl_ri.pem", serials=(inter.cert.serial_number,)),
    CRL_INTER,
)
CRL_FORGED = forger.crl(f"{d}/crl_forged.pem", serials=(ca.issued["revoked"].serial_number,))
CRL_FUTURE = ca.crl(
    f"{d}/crl_future.pem",
    revoked=("revoked",),
    last_update=NOW + 2 * DAY,
    next_update=NOW + 9 * DAY,
)
CRL_EXPIRED_AND_CURRENT = bundle(f"{d}/crl_two.pem", CRL_EXPIRED, CRL_EMPTY)
CRL_CURRENT_AND_EXPIRED = bundle(f"{d}/crl_two_b.pem", CRL, f"{d}/crl_expired.pem")
CRL_FORGED_AND_REAL = bundle(f"{d}/crl_forged_real.pem", CRL_FORGED, CRL_EMPTY)

# Lists scoped by an issuing distribution point (critical, as RFC 5280
# requires), a delta list, and certificates naming a distribution point.
DP_A, DP_B = "http://crl.example/a.crl", "http://crl.example/b.crl"
DP_GOOD = ca.issue("dp_good", cn("postgres"), crldp=DP_A)
DP_REVOKED = ca.issue("dp_revoked", cn("postgres"), crldp=DP_A)
DP_OTHER = ca.issue("dp_other", cn("postgres"), crldp=DP_B)


def idp(full_name=None, user=False, ca_only=False, reasons=None):
    return (
        x509.IssuingDistributionPoint(
            full_name=[x509.UniformResourceIdentifier(full_name)] if full_name else None,
            relative_name=None,
            only_contains_user_certs=user,
            only_contains_ca_certs=ca_only,
            only_some_reasons=reasons,
            indirect_crl=False,
            only_contains_attribute_certs=False,
        ),
        True,
    )


SCOPED = ("revoked", "dp_revoked")
CRL_IDP_DP = ca.crl(f"{d}/crl_idp_dp.pem", revoked=SCOPED, extensions=(idp(DP_A),))
CRL_IDP_USER = ca.crl(f"{d}/crl_idp_user.pem", revoked=SCOPED, extensions=(idp(user=True),))
CRL_IDP_CA = ca.crl(f"{d}/crl_idp_ca.pem", revoked=SCOPED, extensions=(idp(ca_only=True),))
CRL_IDP_REASONS = ca.crl(
    f"{d}/crl_idp_reasons.pem",
    revoked=SCOPED,
    extensions=(idp(reasons=frozenset({x509.ReasonFlags.key_compromise})),),
)
CRL_DELTA = ca.crl(
    f"{d}/crl_delta.pem", revoked=SCOPED, extensions=((x509.DeltaCRLIndicator(1), True),)
)

# Server certificates whose SIGNATURE is RSA-PSS or Ed25519.
pss256, pss384, ed = (
    Authority("pss256 CA", pss_hash=hashes.SHA256()),
    Authority("pss384 CA", pss_hash=hashes.SHA384()),
    Authority("ed25519 CA", kind="ed25519"),
)
for a, stem in ((pss256, "pss256"), (pss384, "pss384"), (ed, "ed")):
    a.issue(f"s_{stem}", cn("localhost"), san=True)
    a.pem(f"{d}/ca_{stem}.pem")


# --- a raw client -----------------------------------------------------------
class Raw:
    def __init__(self, port, user="alice", cert=None, cafile=None):
        s = socket.create_connection(("127.0.0.1", port), timeout=5)
        s.sendall(struct.pack("!II", 8, 80877103))
        assert s.recv(1) == b"S"
        ctx = ssl.create_default_context(cafile=cafile or capem)
        ctx.check_hostname = False
        if cert:
            ctx.load_cert_chain(cert["sslcert"], cert["sslkey"])
        s = ctx.wrap_socket(s)
        self.peer = s.getpeercert(binary_form=True)
        self.s = s
        body = struct.pack("!I", 196608)
        for k, v in (("user", user), ("database", "postgres")):
            body += k.encode() + b"\0" + v.encode() + b"\0"
        body += b"\0"
        s.sendall(struct.pack("!I", len(body) + 4) + body)

    def read(self):
        head = self._exact(5)
        return head[:1], self._exact(struct.unpack("!I", head[1:])[0] - 4)

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


def fingerprint(port, cafile=None):
    try:
        return hashlib.sha256(Raw(port, cafile=cafile).peer).hexdigest()[:12]
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


#: What a client sees when the server's close reaches it before the TLS alert
#: that says why.
_CLOSED_FIRST = ("SSLEOFError", "ConnectionResetError", "BrokenPipeError")


def _raw_first_once(port, user, cert=None):
    try:
        c = Raw(port, user=user, cert=cert)
        kind, body = c.read()
        if kind == b"E":
            return ("error",) + error_fields(body)
        return ("auth", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, re.sub(r"\(_ssl.c:\d+\)", "", str(e))[:90])


def raw_first(port, user, cert=None):
    """A server's first answer to a startup packet.

    A certificate the server refuses is answered with a TLS alert as it
    closes the connection, and a client now and then reads the close before
    the alert: PostgreSQL 15.19 did that once in CI (2026-10-12, "good cert,
    first answer" under a revocation list with no entry for the issuer), and
    479 of 480 scenarios then matched. So a close is asked about again, on
    either server, and the answer compared is the alert whenever the server
    sends one. A server that only ever closes still answers with the close.
    """
    out = _raw_first_once(port, user, cert)
    for _ in range(4):
        if not (out[0] == "client" and out[1] in _CLOSED_FIRST):
            break
        out = _raw_first_once(port, user, cert)
    return out


def raw_scram(port, mechanism, gs2, cafile, hash_name="sha256", password="pw", user="alice"):
    """One SCRAM login binding to the certificate's hash under `hash_name`."""
    try:
        c = Raw(port, user=user, cafile=cafile)
        kind, body = c.read()
        if kind == b"E":
            return ("error", "at startup") + error_fields(body)
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
            return ("error", "after client-first") + error_fields(body)
        assert struct.unpack("!I", body[:4])[0] == 11, body
        server_first = body[4:].decode()
        attrs = dict(a.split("=", 1) for a in server_first.split(","))
        salt, iters = base64.b64decode(attrs["s"]), int(attrs["i"])
        data = gs2.encode()
        if gs2.startswith("p="):
            data += hashlib.new(hash_name, c.peer).digest()
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
            return ("error", "after client-final") + error_fields(body)
        assert struct.unpack("!I", body[:4])[0] == 12, body
        kind, body = c.read()
        if kind == b"E":
            return ("error", "after server-final") + error_fields(body)
        return ("ok", struct.unpack("!I", body[:4])[0])
    except Exception as e:  # noqa: BLE001
        return ("client", type(e).__name__, str(e)[:90])


def attempt(port, user="postgres", query="select current_user::text", cafile=None, **kw):
    kw.setdefault("sslrootcert", cafile or capem)
    kw.setdefault("sslmode", "require")
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
            return ("ok", c.execute(query).fetchall())
    except psycopg.Error as e:
        msg = str(e).strip().splitlines()[-1].split("failed: ", 1)[-1]
        return (
            type(e).__name__,
            getattr(e, "sqlstate", None),
            re.sub(r"\d{4,5}", "PORT", msg)[:200],
        )


def setup(port):
    with psycopg.connect(
        host="127.0.0.1",
        port=port,
        dbname="postgres",
        user="postgres",
        autocommit=True,
        sslmode="require",
        sslrootcert=capem,
        **PGCERT,
    ) as c:
        for role, pw in (("alice", "pw"), ("bob", "pw"), ("carol", None)):
            c.execute(f"drop role if exists {role}")
            c.execute(f"create role {role} login" + (f" password '{pw}'" if pw else ""))


# --- scenario groups --------------------------------------------------------
def crl_scenarios(port):
    return {
        "good cert": attempt(port, **PGCERT),
        "no cert": attempt(port),
        "revoked cert": attempt(port, **REVOKED),
        "revoked cert, first answer": raw_first(port, "postgres", cert=REVOKED),
        "good cert, first answer": raw_first(port, "postgres", cert=PGCERT),
        "alice pw, good cert": attempt(port, user="alice", password="pw", **ALICE),
        "alice pw, revoked cert": attempt(port, user="alice", password="pw", **ALICE_REVOKED),
    }


def scope_scenarios(port):
    return {
        "no distribution point, good": attempt(port, **PGCERT),
        "no distribution point, revoked": attempt(port, **REVOKED),
        "distribution point a, good": attempt(port, **DP_GOOD),
        "distribution point a, revoked": attempt(port, **DP_REVOKED),
        "distribution point b": attempt(port, **DP_OTHER),
        "revoked, first answer": raw_first(port, "postgres", cert=REVOKED),
    }


def direct_scenarios(port):
    """`sslnegotiation=direct` is PostgreSQL 17's; 15 has no direct TLS and
    closes the connection. The usual negotiation still works beside it."""
    return {
        "direct, require": attempt(port, sslnegotiation="direct"),
        "direct, verify-full": attempt(port, sslnegotiation="direct", sslmode="verify-full"),
        "postgres negotiation": attempt(port, sslnegotiation="postgres"),
    }


def chain_scenarios(port):
    out = crl_scenarios(port)
    out["intermediate's good cert"] = attempt(port, **INTER_GOOD)
    out["intermediate's revoked cert"] = attempt(port, **INTER_REVOKED)
    return out


def cert_scenarios(port):
    out = {}
    for label, cert in (
        ("alice", ALICE),
        ("alice_cert", ALICE_CERT),
        ("shared", SHARED),
        ("bobcert", BOBCERT),
        ("carol@example.com", CAROL_MAIL),
        ("postgres@example.com", PG_MAIL),
        ("postgres", PGCERT),
        ("O=Example,CN=alice", ALICE_DN),
        ("C,O,OU,CN=alice", ALICE_DN3),
        ("none", {}),
    ):
        for user in ("alice", "postgres", "carol", "nobody"):
            out[f"cert {label} as {user}"] = attempt(port, user=user, **cert)
    out["cert alice_cert as alice, first answer"] = raw_first(port, "alice", cert=ALICE_CERT)
    out["cert bobcert as postgres, first answer"] = raw_first(port, "postgres", cert=BOBCERT)
    return out


def full_scenarios(port):
    out = {}
    for label, cert in (
        ("alice", ALICE),
        ("O=Example,CN=alice", ALICE_DN),
        ("C,O,OU,CN=alice", ALICE_DN3),
    ):
        out[f"cert {label} as alice, pw"] = attempt(port, user="alice", password="pw", **cert)
        out[f"cert {label} as alice, wrong pw"] = attempt(port, user="alice", password="no", **cert)
    for label, cert in (("postgres", PGCERT), ("alice", ALICE)):
        out[f"cert {label} as postgres"] = attempt(port, user="postgres", **cert)
    return out


def binding_scenarios(cafile):
    def run(port):
        out = {
            "offered": raw_scram(port, None, None, cafile),
            "libpq require": attempt(
                port, user="alice", password="pw", channel_binding="require", cafile=cafile
            ),
            "libpq prefer": attempt(
                port, user="alice", password="pw", channel_binding="prefer", cafile=cafile
            ),
            "libpq disable": attempt(
                port, user="alice", password="pw", channel_binding="disable", cafile=cafile
            ),
            "libpq require, wrong pw": attempt(
                port, user="alice", password="no", channel_binding="require", cafile=cafile
            ),
            "no binding, y": raw_scram(port, "SCRAM-SHA-256", "y,,", cafile),
            "no binding, n": raw_scram(port, "SCRAM-SHA-256", "n,,", cafile),
        }
        for h in ("sha256", "sha384", "sha512"):
            out[f"PLUS {h}"] = raw_scram(
                port, "SCRAM-SHA-256-PLUS", "p=tls-server-end-point,,", cafile, hash_name=h
            )
        return out

    return run


OPEN = (
    "hostssl all postgres 127.0.0.1/32 trust\nhostssl all carol 127.0.0.1/32 trust\n"
    "hostssl all all 127.0.0.1/32 scram-sha-256\n"
)
IDENT = (
    "m alice_cert alice\n"
    "m shared alice\n"
    "m shared postgres\n"
    'm "/^(.*)@example\\.com$" \\1\n'
    "other bobcert postgres\n"
    'dn "/^CN=(.*),O=Example$" \\1\n'
    'dn3 "CN=alice,OU=Eng,O=Ex\\, ample,C=IE" alice\n'
)
open(f"{d}/ident.conf", "w").write(IDENT)

# group -> (pg: conf lines, hba, ident; ours: flags; scenarios; server cert stem)
GROUPS = {
    "crl": ("ssl_crl_file = '{d}/crl.pem'", OPEN, ["--tls-crl-file", CRL], crl_scenarios, "s"),
    "crl-empty": (
        "ssl_crl_file = '{d}/crl_empty.pem'",
        OPEN,
        ["--tls-crl-file", CRL_EMPTY],
        crl_scenarios,
        "s",
    ),
    "crl-expired": (
        "ssl_crl_file = '{d}/crl_expired.pem'",
        OPEN,
        ["--tls-crl-file", CRL_EXPIRED],
        crl_scenarios,
        "s",
    ),
    "crl-other-ca": (
        "ssl_crl_file = '{d}/crl_other.pem'",
        OPEN,
        ["--tls-crl-file", CRL_OTHER],
        crl_scenarios,
        "s",
    ),
    "crl-v1": (
        "ssl_crl_file = '{d}/crl_v1.pem'",
        OPEN,
        ["--tls-crl-file", CRL_V1],
        crl_scenarios,
        "s",
    ),
    **{
        f"crl {label}": (
            f"ssl_crl_file = '{path}'",
            OPEN,
            ["--tls-crl-file", path],
            chain_scenarios,
            "s",
        )
        for label, path in (
            ("root only", CRL_ROOT_ONLY),
            ("root and intermediate", CRL_BOTH),
            ("intermediate only", CRL_INTER_ONLY),
            ("root revokes intermediate", CRL_ROOT_REVOKES_INTER),
            ("forged", CRL_FORGED),
            ("not yet valid", CRL_FUTURE),
            ("expired and current", CRL_EXPIRED_AND_CURRENT),
            ("current and expired", CRL_CURRENT_AND_EXPIRED),
            ("forged and real", CRL_FORGED_AND_REAL),
            ("none", None),
        )
        if path
    },
    **{
        f"crl scope {label}": (
            f"ssl_crl_file = '{path}'",
            OPEN,
            ["--tls-crl-file", path],
            scope_scenarios,
            "s",
        )
        for label, path in (
            ("distribution point", CRL_IDP_DP),
            ("user certs only", CRL_IDP_USER),
            ("CA certs only", CRL_IDP_CA),
            ("some reasons", CRL_IDP_REASONS),
            ("delta", CRL_DELTA),
        )
    },
    "direct tls": ("", OPEN, [], direct_scenarios, "s"),
    "no crl, chain": ("", OPEN, [], chain_scenarios, "s"),
    "crl-cert": (
        "ssl_crl_file = '{d}/crl.pem'",
        "hostssl all all 127.0.0.1/32 cert\n",
        ["--tls-crl-file", CRL, "--tls-client-cert", "cert"],
        crl_scenarios,
        "s",
    ),
    "cert": (
        "",
        "hostssl all all 127.0.0.1/32 cert\n",
        ["--tls-client-cert", "cert"],
        cert_scenarios,
        "s",
    ),
    "cert map=m": (
        "",
        "hostssl all all 127.0.0.1/32 cert map=m\n",
        ["--tls-client-cert", "cert", "--tls-ident-file", f"{d}/ident.conf", "--tls-user-map", "m"],
        cert_scenarios,
        "s",
    ),
    "cert map=other": (
        "",
        "hostssl all all 127.0.0.1/32 cert map=other\n",
        [
            "--tls-client-cert",
            "cert",
            "--tls-ident-file",
            f"{d}/ident.conf",
            "--tls-user-map",
            "other",
        ],
        cert_scenarios,
        "s",
    ),
    "cert map=absent": (
        "",
        "hostssl all all 127.0.0.1/32 cert map=absent\n",
        [
            "--tls-client-cert",
            "cert",
            "--tls-ident-file",
            f"{d}/ident.conf",
            "--tls-user-map",
            "absent",
        ],
        cert_scenarios,
        "s",
    ),
    "cert clientname=DN": (
        "",
        "hostssl all all 127.0.0.1/32 cert clientname=DN\n",
        ["--tls-client-cert", "cert", "--tls-client-name", "DN"],
        cert_scenarios,
        "s",
    ),
    "cert clientname=DN map=dn": (
        "",
        "hostssl all all 127.0.0.1/32 cert clientname=DN map=dn\n",
        [
            "--tls-client-cert",
            "cert",
            "--tls-client-name",
            "DN",
            "--tls-ident-file",
            f"{d}/ident.conf",
            "--tls-user-map",
            "dn",
        ],
        cert_scenarios,
        "s",
    ),
    "cert clientname=DN map=dn3": (
        "",
        "hostssl all all 127.0.0.1/32 cert clientname=DN map=dn3\n",
        [
            "--tls-client-cert",
            "cert",
            "--tls-client-name",
            "DN",
            "--tls-ident-file",
            f"{d}/ident.conf",
            "--tls-user-map",
            "dn3",
        ],
        cert_scenarios,
        "s",
    ),
    "verify-full clientname=DN": (
        "",
        "hostssl all postgres 127.0.0.1/32 trust clientcert=verify-full clientname=DN\n"
        "hostssl all all 127.0.0.1/32 scram-sha-256 clientcert=verify-full clientname=DN\n",
        ["--tls-client-cert", "verify-full", "--tls-client-name", "DN"],
        full_scenarios,
        "s",
    ),
    "binding pss256": ("", OPEN, [], binding_scenarios(f"{d}/ca_pss256.pem"), "s_pss256"),
    "binding pss384": ("", OPEN, [], binding_scenarios(f"{d}/ca_pss384.pem"), "s_pss384"),
    "binding ed25519": ("", OPEN, [], binding_scenarios(f"{d}/ca_ed.pem"), "s_ed"),
}
if ONLY:
    GROUPS = {k: v for k, v in GROUPS.items() if k in ONLY or k.split()[0] in ONLY}


def reload_story(port, reload, cert_path, key_path, crl_path):
    """What a reload picks up. `reload()` asks the server to re-read its files."""
    out = {
        "before": fingerprint(port)
        == hashlib.sha256(ca.issued["s"].public_bytes(serialization.Encoding.DER)).hexdigest()[:12]
    }
    shutil.copy(f"{d}/s2.crt", cert_path)
    shutil.copy(f"{d}/s2.key", key_path)
    os.chmod(key_path, 0o600)
    second = hashlib.sha256(ca.issued["s2"].public_bytes(serialization.Encoding.DER)).hexdigest()[
        :12
    ]
    out["files changed, no reload: still the first"] = fingerprint(port) != second
    reload()
    out["after reload: the second"] = fingerprint(port) == second
    open(cert_path, "w").write("not a certificate\n")
    reload()
    out["bad certificate, after reload: still the second"] = fingerprint(port) == second
    out["and it still serves"] = attempt(port, **PGCERT)
    shutil.copy(f"{d}/s2.crt", cert_path)
    out["revoked cert before the CRL names it"] = attempt(port, **REVOKED)
    shutil.copy(CRL, crl_path)
    reload()
    out["revoked cert after the CRL reload"] = attempt(port, **REVOKED)
    out["good cert after the CRL reload"] = attempt(port, **PGCERT)
    return out


# --- PostgreSQL ---------------------------------------------------------------
pgd = f"{d}/pgdata"
subprocess.run(
    [f"{PGB}/initdb", "-D", pgd, "-U", "postgres", "--auth=trust", "-E", "UTF8"],
    check=True,
    capture_output=True,
)
BASECONF = open(f"{pgd}/postgresql.conf").read() + (
    f"\nssl = on\nssl_ca_file = '{capem}'\nport = {PORT}\nlisten_addresses = '127.0.0.1'\n"
    "unix_socket_directories = ''\npassword_encryption = 'scram-sha-256'\n"
)
shutil.copy(f"{d}/ident.conf", f"{pgd}/pg_ident.conf")


def pg(action):
    r = subprocess.run(
        [f"{PGB}/pg_ctl", "-D", pgd, "-l", f"{d}/pg.log", "-w", action, "-m", "fast"],
        capture_output=True,
        text=True,
    )
    if r.returncode:
        raise SystemExit(
            f"pg_ctl {action}: {r.stdout}{r.stderr}\n{open(f'{d}/pg.log').read()[-2000:]}"
        )


def pg_config(conf, hba, stem):
    shutil.copy(f"{d}/{stem}.crt", f"{pgd}/server.crt")
    shutil.copy(f"{d}/{stem}.key", f"{pgd}/server.key")
    os.chmod(f"{pgd}/server.key", 0o600)
    open(f"{pgd}/postgresql.conf", "w").write(BASECONF + conf.format(d=d) + "\n")
    open(f"{pgd}/pg_hba.conf", "w").write(hba)


ref, ours = {}, {}
pg_config("", OPEN, "s")
pg("start")
setup(PORT)
with psycopg.connect(
    host="127.0.0.1",
    port=PORT,
    dbname="postgres",
    user="postgres",
    sslmode="require",
    sslrootcert=capem,
) as c:
    print("reference:", c.execute("show server_version").fetchone()[0])
    assert attempt(PORT, **PGCERT) == ("ok", [("postgres",)]), (
        "self-check: a good certificate connects"
    )
for name, (conf, hba, _flags, run, stem) in GROUPS.items():
    pg("stop")
    pg_config(conf, hba, stem)
    pg("start")
    ref[name] = run(PORT)
if not ONLY or "reload" in ONLY:
    pg("stop")
    shutil.copy(CRL_EMPTY, f"{d}/live_crl.pem")
    pg_config("ssl_crl_file = '{d}/live_crl.pem'", OPEN, "s")
    pg("start")
    ref["reload"] = reload_story(
        PORT,
        lambda: (
            subprocess.run([f"{PGB}/pg_ctl", "-D", pgd, "reload"], check=True, capture_output=True),
            time.sleep(1),
        ),
        f"{pgd}/server.crt",
        f"{pgd}/server.key",
        f"{d}/live_crl.pem",
    )
pg("stop")

# --- secantusd-pg -------------------------------------------------------------
if BIN:
    home = f"{d}/home"

    def start(flags, stem="s", cert_path=None, key_path=None):
        base = [
            BIN,
            home,
            "127.0.0.1:0",
            "--tls-cert-file",
            cert_path or f"{d}/{stem}.crt",
            "--tls-key-file",
            key_path or f"{d}/{stem}.key",
            "--tls-ca-file",
            capem,
        ]
        p = subprocess.Popen(
            base + flags, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
        )
        line = p.stdout.readline()
        m = re.search(r"listening on \S+:(\d+)", line)
        if not m:
            return None, f"did not start with {flags}: {line}{p.stdout.read()}"[:300]
        return p, int(m.group(1))

    def stop(p):
        p.send_signal(signal.SIGTERM)
        p.wait(30)

    p, port = start([])
    setup(port)
    stop(p)
    for name, (_c, _h, flags, run, stem) in GROUPS.items():
        p, port = start(flags, stem)
        if p is None:
            ours[name] = {"(start)": port}
            continue
        try:
            ours[name] = run(port)
        finally:
            stop(p)
    if "reload" in ref:
        for how in ("SIGHUP", "pg_reload_conf()"):
            shutil.copy(f"{d}/s.crt", f"{d}/live.crt")
            shutil.copy(f"{d}/s.key", f"{d}/live.key")
            shutil.copy(CRL_EMPTY, f"{d}/live_crl.pem")
            p, port = start(
                ["--tls-crl-file", f"{d}/live_crl.pem"],
                cert_path=f"{d}/live.crt",
                key_path=f"{d}/live.key",
            )
            if p is None:
                ours[f"reload by {how}"] = {"(start)": port}
                continue

            def reload(p=p, port=port, how=how):
                if how == "SIGHUP":
                    p.send_signal(signal.SIGHUP)
                else:
                    print(
                        "     pg_reload_conf ->",
                        attempt(port, query="select pg_reload_conf()", **PGCERT),
                    )
                time.sleep(1)

            try:
                ours[f"reload by {how}"] = reload_story(
                    port, reload, f"{d}/live.crt", f"{d}/live.key", f"{d}/live_crl.pem"
                )
            finally:
                if p.poll() is None:
                    stop(p)
                else:
                    ours[f"reload by {how}"]["(server exited)"] = p.returncode
            ref[f"reload by {how}"] = ref["reload"]
        del ref["reload"]

same = total = 0
for name in ref:
    print(f"\n== {name}")
    for k, v in ref[name].items():
        total += 1
        if not BIN:
            print("  ", k, "->", v)
        elif ours.get(name, {}).get(k) == v:
            same += 1
            print("  SAME", k, v)
        else:
            print("  DIFF", k, "\n      pg  :", v, "\n      ours:", ours.get(name, {}).get(k))
    for k in set(ours.get(name, {})) - set(ref[name]):
        print("  OURS ONLY", k, ours[name][k])
if BIN:
    print(f"\n{same} of {total} the same")
shutil.rmtree(d)
