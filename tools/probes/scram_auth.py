"""SCRAM authentication against an `--auth` mongod, per mechanism.

Needs both servers started with access control, and with NO users -- the probe
bootstraps its root user through the LOCALHOST EXCEPTION, which is itself one of
the things compared (the Rust server refused every `createUser` on a fresh
`--auth` store until 2026-09-30, so it could never be given a user):

    mongod --auth --port 27042 --dbpath <empty> &
    secantusd-rs --auth --port 27059 --storage-path <empty> &
    PROBE_MONGOD_AUTH="mongodb://127.0.0.1:27042/?directConnection=true" \\
    PROBE_SERVER_AUTH="mongodb://127.0.0.1:27059/?directConnection=true" \\
        python tools/probes/scram_auth.py

Compared: which mechanisms `createUser` stores by default and on request,
`hello`'s `saslSupportedMechs`, and the outcome -- code and message -- of
authenticating each user with each mechanism, right and wrong password.
"""

from __future__ import annotations

import os
import sys
from urllib.parse import urlsplit

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD_AUTH")
SERVER = os.environ.get("PROBE_SERVER_AUTH")
ROOT = ("probe_root", "probe_root_pw")

#: `(user, password, mechanisms or None for the default)`.
USERS = [
    ("both", "pw1", None),
    ("only256", "pw2", ["SCRAM-SHA-256"]),
    ("only1", "pw3", ["SCRAM-SHA-1"]),
]
MECHS = ["SCRAM-SHA-1", "SCRAM-SHA-256"]


def hostport(uri):
    return urlsplit(uri).netloc


def outcome(fn):
    try:
        return ("ok", fn())
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code, (e.details or {}).get("errmsg"))


def measure(uri):
    anon = pymongo.MongoClient(uri)
    rows = []
    rows.append(
        (
            "bootstrap root",
            outcome(
                lambda: anon.admin.command("createUser", ROOT[0], pwd=ROOT[1], roles=["root"])["ok"]
            ),
        )
    )
    root = pymongo.MongoClient(
        f"mongodb://{ROOT[0]}:{ROOT[1]}@{hostport(uri)}/?directConnection=true"
    )
    rows.append(
        (
            "second user unauthenticated",
            outcome(lambda: anon.admin.command("createUser", "x", pwd="y", roles=[])["ok"]),
        )
    )
    db = root.probe_scram
    for user, pwd, mechs in USERS:
        kw = {"mechanisms": mechs} if mechs else {}
        db.command("createUser", user, pwd=pwd, roles=[], **kw)
        info = db.command("usersInfo", user, showCredentials=True)["users"][0]
        rows.append((f"{user} mechanisms", info.get("mechanisms")))
        rows.append((f"{user} credential kinds", sorted(info.get("credentials", {}))))
        # Compared as a SET: mongod's order for this list is stable within one
        # process and differs between processes (`["SCRAM-SHA-1",
        # "SCRAM-SHA-256"]` on one 8.2.11, the reverse on the next) -- a hashed
        # container, so there is no order to match.
        mechs_reply = root.admin.command("hello", saslSupportedMechs=f"probe_scram.{user}")
        rows.append(
            (f"{user} saslSupportedMechs", sorted(mechs_reply.get("saslSupportedMechs", [])))
        )
        for mech in MECHS:
            for label, pw in (("right", pwd), ("wrong", pwd + "x")):
                client = pymongo.MongoClient(
                    f"mongodb://{user}:{pw}@{hostport(uri)}/probe_scram?directConnection=true&authMechanism={mech}"
                )
                rows.append(
                    (
                        f"{user} {mech} {label} password",
                        outcome(lambda c=client: c.probe_scram.command("ping")["ok"]),
                    )
                )
    rows.append(
        (
            "unknown user",
            outcome(
                lambda: root.admin.command("hello", saslSupportedMechs="probe_scram.nobody").get(
                    "saslSupportedMechs"
                )
            ),
        )
    )
    rows.append(
        (
            "usersInfo forAllDBs",
            sorted(
                u["user"] for u in root.admin.command("usersInfo", {"forAllDBs": True})["users"]
            ),
        )
    )
    root.drop_database("probe_scram")
    for user, _, _ in USERS:
        root.probe_scram.command("dropUser", user)
    root.admin.command("dropUser", ROOT[0])
    # With every user gone the exception stays CLOSED until a restart on mongod.
    # This row is why the probe needs freshly started servers.
    rows.append(
        (
            "exception after the last user is dropped",
            outcome(lambda: anon.admin.command("createUser", "again", pwd="x", roles=[])["ok"]),
        )
    )
    return rows


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD_AUTH and PROBE_SERVER_AUTH are required (see the module docstring)")
        return 2
    want, got = measure(MONGOD), measure(SERVER)
    bad = 0
    for (label, w), (_, g) in zip(want, got, strict=True):
        if w != g:
            bad += 1
            print(f"DIFF {label}\n  mongod: {w}\n  ours:   {g}")
    print(f"=== SCRAM authentication: {bad} of {len(want)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
