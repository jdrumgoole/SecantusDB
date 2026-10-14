"""Differential-probe the user and role management commands against mongod.

`createUser`, `updateUser`, `dropUser`, `dropAllUsersFromDatabase`,
`grantRolesToUser`, `revokeRolesFromUser`, `usersInfo`, `createRole`,
`updateRole`, `dropRole`, `dropAllRolesFromDatabase`,
`grantPrivilegesToRole`, `revokePrivilegesFromRole`, `grantRolesToRole`,
`revokeRolesFromRole` and `rolesInfo`, on a server with access control off:
what each stores, what each lists and what each refuses. Authentication
itself is `scram_auth.py`'s.

A scenario runs in its own database and against `admin`, and drops every
user and role of both before and after. A user's `userId` and credentials
are the server's own, so they are compared by type.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/user_role_commands.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Binary, Int64  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
NOISE = ("$clusterTime", "operationTime", "opTime", "electionId")
#: Values that are the server's own; the BSON type is still compared.
VOLATILE = {"userId", "salt", "storedKey", "serverKey"}

#: Lists mongod builds from a set, in an order that changes from run to run.
UNORDERED = {
    "inheritedRoles",
    "inheritedPrivileges",
    "inheritedAuthenticationRestrictions",
    "authenticatedUserRoles",
    "authenticatedUserPrivileges",
}

A = "admin"
S: list[tuple[str, list]] = []


def scenario(name, *steps):
    S.append((name, list(steps)))


def user(name="u", pwd="pw", roles=(), **more):
    return {"createUser": name, "pwd": pwd, "roles": list(roles), **more}


def role(name="r", privileges=(), roles=(), **more):
    return {"createRole": name, "privileges": list(privileges), "roles": list(roles), **more}


def info(arg=1, **more):
    return {"usersInfo": arg, **more}


def rinfo(arg=1, **more):
    return {"rolesInfo": arg, **more}


FIND = {"resource": {"db": "@db", "collection": "c"}, "actions": ["find"]}
WRITE = {"resource": {"db": "@db", "collection": ""}, "actions": ["insert", "update"]}
BAD = [1, None, True, 2.5, {}, [], ""]

# ------------------------------------------------------------- createUser

scenario(
    "createUser: the forms a user's roles take",
    user("a", roles=["read"]),
    user("b", roles=[{"role": "readWrite", "db": "@db"}]),
    user("c", roles=["read", {"role": "dbAdmin", "db": "other"}]),
    user("d", roles=[]),
    user("e", roles=["read", "read"]),
    user("f", roles=["nosuchrole"]),
    user("g", roles=[{"role": "nosuchrole", "db": "@db"}]),
    user("h", roles=[{"role": "read"}]),
    user("i", roles=[{"db": "@db"}]),
    user("j", roles=[{"role": "read", "db": "@db", "bogus": 1}]),
    user("k", roles=[1]),
    user("l", roles=["root"]),
    user("m", roles=[{"role": "root", "db": "admin"}]),
    user("n", roles=["__system"]),
    *[{"createUser": "o", "pwd": "pw", "roles": v} for v in [1, "read", None, {}]],
    {"createUser": "p", "pwd": "pw"},
    info(),
)
scenario(
    "createUser: the name and the password",
    *[{"createUser": v, "pwd": "pw", "roles": []} for v in BAD],
    *[{"createUser": "a", "pwd": v, "roles": []} for v in [1, None, True, {}, [], ""]],
    {"createUser": "nopwd", "roles": []},
    user("a.b"),
    user("a b"),
    user("a$b"),
    user("a\x00b"),
    user("é"),
    user("x" * 200),
    user("dup"),
    user("dup"),
    user("dup", pwd="other"),
    info(),
)
scenario(
    "createUser: its options",
    user("a", customData={"k": 1}),
    user("b", customData=1),
    user("c", customData=None),
    user("d", mechanisms=["SCRAM-SHA-256"]),
    user("e", mechanisms=["SCRAM-SHA-1"]),
    user("f", mechanisms=["SCRAM-SHA-1", "SCRAM-SHA-256"]),
    user("g", mechanisms=[]),
    user("h", mechanisms=["bogus"]),
    user("i", mechanisms="SCRAM-SHA-256"),
    user("j", mechanisms=["MONGODB-X509"]),
    user("k", digestPassword=True),
    user("l", digestPassword=False),
    user("m", digestPassword="x"),
    user("n", digestPassword=False, mechanisms=["SCRAM-SHA-256"]),
    user("o", authenticationRestrictions=[]),
    user("p", authenticationRestrictions=[{"clientSource": ["127.0.0.1"]}]),
    user("q", authenticationRestrictions=[{"bogus": 1}]),
    user("r", authenticationRestrictions=1),
    user("s", bogus=1),
    user("t", writeConcern={"w": 1}),
    user("u", comment="x"),
    user("v", pwd="p" * 2000),
    info(showCredentials=True),
    info(showCustomData=False),
)
scenario(
    "createUser: on admin, local and $external",
    (A, user("adm", roles=["root"])),
    (A, user("adm2", roles=[{"role": "read", "db": "@db"}])),
    (A, info()),
    ("local", user("loc")),
    ("$external", user("CN=x", roles=[])),
    ("$external", {"createUser": "CN=y", "roles": []}),
    ("$external", {"createUser": "CN=z", "roles": [], "mechanisms": ["SCRAM-SHA-256"]}),
    ("$external", info()),
    ("$external", {"dropAllUsersFromDatabase": 1}),
    ("config", user("cfg")),
    ("config", {"dropAllUsersFromDatabase": 1}),
    ("", user("nodb")),
)

# ---------------------------------------------------- update / drop a user

scenario(
    "updateUser",
    user("a", roles=["read"], customData={"k": 1}),
    {"updateUser": "a", "roles": ["readWrite"]},
    info("a"),
    {"updateUser": "a", "customData": {"k": 2}},
    info("a"),
    {"updateUser": "a", "pwd": "new"},
    {"updateUser": "a", "roles": []},
    info("a"),
    {"updateUser": "a"},
    {"updateUser": "a", "bogus": 1},
    {"updateUser": "nosuch", "roles": []},
    {"updateUser": "a", "roles": ["nosuchrole"]},
    {"updateUser": "a", "roles": 1},
    {"updateUser": "a", "pwd": 1},
    {"updateUser": "a", "pwd": ""},
    {"updateUser": "a", "mechanisms": ["SCRAM-SHA-256"]},
    {"updateUser": "a", "mechanisms": ["SCRAM-SHA-1"], "pwd": "x"},
    {"updateUser": "a", "mechanisms": ["bogus"]},
    {"updateUser": "a", "customData": 1},
    {"updateUser": "a", "authenticationRestrictions": []},
    {"updateUser": "a", "digestPassword": False, "pwd": "x"},
    *[{"updateUser": v, "roles": []} for v in [1, None, {}, ""]],
    info("a", showCredentials=True),
)
scenario(
    "dropUser and dropAllUsersFromDatabase",
    user("a"),
    user("b"),
    user("c"),
    {"dropUser": "a"},
    {"dropUser": "a"},
    {"dropUser": "nosuch"},
    *[{"dropUser": v} for v in [1, None, {}, ""]],
    {"dropUser": "b", "bogus": 1},
    {"dropUser": "b", "writeConcern": {"w": 1}},
    info(),
    {"dropAllUsersFromDatabase": 1},
    {"dropAllUsersFromDatabase": 1},
    {"dropAllUsersFromDatabase": "x"},
    {"dropAllUsersFromDatabase": 1, "bogus": 1},
    info(),
)
scenario(
    "grantRolesToUser and revokeRolesFromUser",
    user("a", roles=["read"]),
    {"grantRolesToUser": "a", "roles": ["readWrite"]},
    info("a"),
    {"grantRolesToUser": "a", "roles": ["readWrite", {"role": "dbAdmin", "db": "other"}]},
    info("a"),
    {"grantRolesToUser": "a", "roles": []},
    {"grantRolesToUser": "a", "roles": ["nosuchrole"]},
    {"grantRolesToUser": "a", "roles": 1},
    {"grantRolesToUser": "a"},
    {"grantRolesToUser": "nosuch", "roles": ["read"]},
    {"grantRolesToUser": "a", "roles": ["read"], "bogus": 1},
    *[{"grantRolesToUser": v, "roles": ["read"]} for v in [1, None, {}, ""]],
    {"revokeRolesFromUser": "a", "roles": ["read"]},
    info("a"),
    {"revokeRolesFromUser": "a", "roles": ["read"]},
    {"revokeRolesFromUser": "a", "roles": ["nosuchrole"]},
    {"revokeRolesFromUser": "a", "roles": [{"role": "dbAdmin", "db": "other"}]},
    {"revokeRolesFromUser": "a", "roles": []},
    {"revokeRolesFromUser": "a", "roles": 1},
    {"revokeRolesFromUser": "a"},
    {"revokeRolesFromUser": "nosuch", "roles": ["read"]},
    info("a"),
)

# -------------------------------------------------------------- usersInfo

scenario(
    "usersInfo: the forms of its argument",
    user("a", roles=["read"], customData={"k": 1}),
    user("b", roles=[{"role": "readWrite", "db": "other"}]),
    (A, user("adm", roles=["root"])),
    info(1),
    info("a"),
    info("nosuch"),
    info({"user": "a", "db": "@db"}),
    info({"user": "a", "db": "nosuchdb"}),
    info(["a", "b"]),
    info(["b", "a"]),
    info(["a", "nosuch"]),
    info([{"user": "a", "db": "@db"}, {"user": "adm", "db": "admin"}]),
    info([]),
    info({"forAllDBs": True}, filter={"db": {"$in": ["@db", "admin"]}}),
    info({"forAllDBs": True}, filter={"db": "@db", "user": "a"}),
    info({"forAllDBs": False}),
    info({"forAllDBs": 1}),
    (A, info(1)),
    (A, info("a")),
    (A, info({"user": "a", "db": "@db"})),
    info(1.0),
    info(Int64(1)),
    info(2),
    info(0),
    info(True),
)
scenario(
    "usersInfo: its options",
    user("a", roles=["read"], customData={"k": 1}),
    user("b", roles=["readWrite"], mechanisms=["SCRAM-SHA-256"]),
    info("a", showCredentials=True),
    info("b", showCredentials=True),
    info("a", showCustomData=False),
    info("a", showCustomData=True),
    info("a", showPrivileges=True),
    info("a", showAuthenticationRestrictions=True),
    info(1, showPrivileges=True),
    info(1, showAuthenticationRestrictions=True),
    info(["a", "b"], showPrivileges=True),
    info(1, filter={"roles.role": "read"}),
    info(1, filter={"user": {"$gt": "a"}}),
    info(1, filter={"customData.k": 1}),
    info(1, filter={"$bogus": 1}),
    info(1, filter={}),
    info("a", filter={"user": "b"}),
    info(1, showCredentials=1),
    info(1, showCredentials=None),
)

# ------------------------------------------------------------------ roles

scenario(
    "createRole: privileges and inherited roles",
    role("a"),
    role("b", [FIND]),
    role("c", [FIND, WRITE]),
    role("d", roles=["read"]),
    role("e", roles=["a", {"role": "b", "db": "@db"}]),
    role("f", roles=["nosuchrole"]),
    role("g", roles=[{"role": "read", "db": "other"}]),
    role("h", roles=["h"]),
    role("i", [{"resource": {"db": "other", "collection": "c"}, "actions": ["find"]}]),
    role("j", [{"resource": {"cluster": True}, "actions": ["serverStatus"]}]),
    role("k", [{"resource": {"anyResource": True}, "actions": ["anyAction"]}]),
    role("l", [{"resource": {"db": "@db", "collection": "c"}, "actions": ["bogusAction"]}]),
    role("m", [{"resource": {"db": "@db", "collection": "c"}, "actions": []}]),
    role("n", [{"resource": {"db": "@db"}, "actions": ["find"]}]),
    role("o", [{"resource": {}, "actions": ["find"]}]),
    role("p", [{"resource": {"db": "@db", "collection": "c"}}]),
    role("q", [{"actions": ["find"]}]),
    role("r", [{"resource": {"db": "@db", "collection": "c"}, "actions": "find"}]),
    role("s", [1]),
    role("t", [{"resource": {"db": "@db", "collection": "c", "bogus": 1}, "actions": ["find"]}]),
    role("u", [{"resource": {"db": "@db", "collection": "c"}, "actions": ["find"], "bogus": 1}]),
    role("v", [{"resource": {"db": "", "collection": ""}, "actions": ["find"]}]),
    role("w", [{"resource": {"db": "", "collection": "c"}, "actions": ["find"]}]),
    role("x", [{"resource": {"system_buckets": "c"}, "actions": ["find"]}]),
    rinfo(1, showPrivileges=True),
)
scenario(
    "createRole: the name, the fields and duplicates",
    *[{"createRole": v, "privileges": [], "roles": []} for v in BAD],
    {"createRole": "a", "roles": []},
    {"createRole": "b", "privileges": []},
    {"createRole": "c"},
    *[{"createRole": "d", "privileges": v, "roles": []} for v in [1, "x", None, {}]],
    *[{"createRole": "e", "privileges": [], "roles": v} for v in [1, "x", None, {}]],
    role("dup"),
    role("dup"),
    role("read"),
    role("root"),
    role("dbAdmin"),
    role("a.b"),
    role("a b"),
    role("a$b"),
    role("f", bogus=1),
    role("g", authenticationRestrictions=[]),
    role("h", authenticationRestrictions=[{"clientSource": ["127.0.0.1"]}]),
    role("i", writeConcern={"w": 1}),
    (A, role("admrole", [{"resource": {"cluster": True}, "actions": ["serverStatus"]}])),
    (A, role("admany", roles=[{"role": "read", "db": "@db"}])),
    ("local", role("locrole")),
    rinfo(1),
    (A, rinfo(1)),
)
scenario(
    "updateRole and dropRole",
    role("a", [FIND], ["read"]),
    role("b"),
    {"updateRole": "a", "privileges": [WRITE]},
    rinfo("a", showPrivileges=True),
    {"updateRole": "a", "roles": ["readWrite"]},
    rinfo("a", showPrivileges=True),
    {"updateRole": "a", "privileges": [], "roles": []},
    rinfo("a", showPrivileges=True),
    {"updateRole": "a"},
    {"updateRole": "a", "bogus": 1},
    {"updateRole": "nosuch", "roles": []},
    {"updateRole": "read", "roles": []},
    {"updateRole": "a", "roles": ["nosuchrole"]},
    {"updateRole": "a", "roles": ["a"]},
    {"updateRole": "a", "roles": ["b"]},
    {"updateRole": "b", "roles": ["a"]},
    {"updateRole": "a", "privileges": 1},
    {"updateRole": "a", "roles": 1},
    {"updateRole": "a", "authenticationRestrictions": []},
    *[{"updateRole": v, "roles": []} for v in [1, None, {}, ""]],
    user("u", roles=["a", "b"]),
    {"dropRole": "b"},
    info("u"),
    rinfo("a"),
    {"dropRole": "b"},
    {"dropRole": "nosuch"},
    {"dropRole": "read"},
    *[{"dropRole": v} for v in [1, None, {}, ""]],
    {"dropRole": "a", "bogus": 1},
    {"dropAllRolesFromDatabase": 1},
    {"dropAllRolesFromDatabase": 1},
    {"dropAllRolesFromDatabase": "x"},
    info("u"),
    rinfo(1),
)
scenario(
    "grantPrivilegesToRole and revokePrivilegesFromRole",
    role("a", [FIND]),
    {"grantPrivilegesToRole": "a", "privileges": [WRITE]},
    rinfo("a", showPrivileges=True),
    {
        "grantPrivilegesToRole": "a",
        "privileges": [{"resource": {"db": "@db", "collection": "c"}, "actions": ["insert"]}],
    },
    rinfo("a", showPrivileges=True),
    {"grantPrivilegesToRole": "a", "privileges": [FIND]},
    {"grantPrivilegesToRole": "a", "privileges": []},
    {"grantPrivilegesToRole": "a", "privileges": 1},
    {"grantPrivilegesToRole": "a"},
    {"grantPrivilegesToRole": "nosuch", "privileges": [FIND]},
    {"grantPrivilegesToRole": "read", "privileges": [FIND]},
    {
        "grantPrivilegesToRole": "a",
        "privileges": [{"resource": {"db": "@db", "collection": "c"}, "actions": ["bogus"]}],
    },
    {"grantPrivilegesToRole": "a", "privileges": [FIND], "bogus": 1},
    {
        "revokePrivilegesFromRole": "a",
        "privileges": [{"resource": {"db": "@db", "collection": "c"}, "actions": ["insert"]}],
    },
    rinfo("a", showPrivileges=True),
    {"revokePrivilegesFromRole": "a", "privileges": [FIND]},
    rinfo("a", showPrivileges=True),
    {"revokePrivilegesFromRole": "a", "privileges": [FIND]},
    {
        "revokePrivilegesFromRole": "a",
        "privileges": [{"resource": {"db": "other", "collection": "c"}, "actions": ["find"]}],
    },
    {"revokePrivilegesFromRole": "a", "privileges": []},
    {"revokePrivilegesFromRole": "a", "privileges": 1},
    {"revokePrivilegesFromRole": "a"},
    {"revokePrivilegesFromRole": "nosuch", "privileges": [FIND]},
    {"revokePrivilegesFromRole": "read", "privileges": [FIND]},
    rinfo("a", showPrivileges=True),
)
scenario(
    "grantRolesToRole and revokeRolesFromRole",
    role("a"),
    role("b"),
    role("c"),
    {"grantRolesToRole": "a", "roles": ["b"]},
    {"grantRolesToRole": "b", "roles": ["c", "read"]},
    rinfo("a"),
    rinfo("b"),
    {"grantRolesToRole": "c", "roles": ["a"]},
    {"grantRolesToRole": "a", "roles": ["a"]},
    {"grantRolesToRole": "a", "roles": ["nosuchrole"]},
    {"grantRolesToRole": "a", "roles": []},
    {"grantRolesToRole": "a", "roles": 1},
    {"grantRolesToRole": "a"},
    {"grantRolesToRole": "nosuch", "roles": ["b"]},
    {"grantRolesToRole": "read", "roles": ["b"]},
    {"grantRolesToRole": "a", "roles": [{"role": "read", "db": "other"}]},
    {"grantRolesToRole": "a", "roles": ["b"], "bogus": 1},
    rinfo("a"),
    {"revokeRolesFromRole": "a", "roles": ["b"]},
    rinfo("a"),
    {"revokeRolesFromRole": "a", "roles": ["b"]},
    {"revokeRolesFromRole": "a", "roles": ["nosuchrole"]},
    {"revokeRolesFromRole": "a", "roles": []},
    {"revokeRolesFromRole": "a", "roles": 1},
    {"revokeRolesFromRole": "a"},
    {"revokeRolesFromRole": "nosuch", "roles": ["b"]},
    {"revokeRolesFromRole": "read", "roles": ["b"]},
    rinfo(1),
)
scenario(
    "rolesInfo: the forms of its argument and its options",
    role("a", [FIND], ["read"]),
    role("b", roles=["a"]),
    (A, role("admrole")),
    rinfo(1),
    rinfo("a"),
    rinfo("b"),
    rinfo("nosuch"),
    rinfo("read"),
    rinfo("readWrite"),
    rinfo("dbAdmin"),
    rinfo("root"),
    (A, rinfo("root")),
    (A, rinfo("clusterAdmin")),
    (A, rinfo("__system")),
    rinfo({"role": "a", "db": "@db"}),
    rinfo({"role": "a", "db": "nosuchdb"}),
    rinfo({"role": "read", "db": "other"}),
    rinfo(["a", "b"]),
    rinfo(["b", "a"]),
    rinfo(["a", "nosuch", "read"]),
    rinfo([{"role": "a", "db": "@db"}, {"role": "admrole", "db": "admin"}]),
    rinfo([]),
    rinfo(1, showBuiltinRoles=True),
    (A, {"rolesInfo": 1, "showBuiltinRoles": True, "pick": "names"}),
    rinfo(1, showPrivileges=True),
    rinfo("a", showPrivileges=True),
    rinfo("b", showPrivileges=True),
    rinfo("read", showPrivileges=True),
    rinfo("b", showPrivileges="asUserFragment"),
    rinfo(1, showPrivileges="asUserFragment"),
    rinfo(["a", "b"], showPrivileges="asUserFragment"),
    rinfo("a", showAuthenticationRestrictions=True),
    rinfo(1, showAuthenticationRestrictions=True),
    rinfo(1.0),
    rinfo(2),
    rinfo(0),
    rinfo(True),
    rinfo({"forAllDBs": True}),
)
scenario(
    "a role in use: users, other roles and the databases they name",
    role("a", [FIND]),
    role("b", roles=["a"]),
    user("u", roles=["b"]),
    info("u", showPrivileges=True),
    {"updateRole": "a", "privileges": [WRITE]},
    info("u", showPrivileges=True),
    {"dropRole": "a"},
    rinfo("b", showPrivileges=True),
    info("u", showPrivileges=True),
    {"dropAllRolesFromDatabase": 1},
    info("u", showPrivileges=True),
    {"dropDatabase": 1},
    info(1),
    rinfo(1),
    {"connectionStatus": 1, "showPrivileges": True},
)

# ---------------------------------------------------------------- running


def resolve(value, db):
    if isinstance(value, dict):
        return {k: resolve(v, db) for k, v in value.items()}
    if isinstance(value, list):
        return [resolve(v, db) for v in value]
    if isinstance(value, str) and "@db" in value:
        return value.replace("@db", db)
    return value


UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def clean(value):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in NOISE:
                continue
            if k in VOLATILE:
                out[k] = f"<{type(v).__name__}>"
            elif k in UNORDERED and isinstance(v, list):
                out[k] = sorted((clean(item) for item in v), key=repr)
            else:
                out[k] = clean(v)
        return out
    if isinstance(value, list):
        return [clean(v) for v in value]
    if isinstance(value, str):
        text = UUID_TEXT.sub("UUID", value)
        return re.sub(
            r"expected types '\[([^\]]*)\]'",
            lambda m: "expected types '[" + ", ".join(sorted(m.group(1).split(", "))) + "]'",
            text,
        )
    if isinstance(value, Binary):
        return f"<Binary {len(value)}>"
    return value


def reduce(reply, pick):
    if pick == "names" and reply.get("ok") == 1:
        return sorted(f"{r.get('db')}.{r.get('role')}" for r in reply.get("roles", []))
    return clean(reply)


def wipe(wire, db):
    for name in (db, "other", A):
        wire.command(name, {"dropAllUsersFromDatabase": 1})
        wire.command(name, {"dropAllRolesFromDatabase": 1})
    wire.command("local", {"dropAllUsersFromDatabase": 1})
    wire.command("local", {"dropAllRolesFromDatabase": 1})


def run_scenario(uri, index, steps):
    db = f"urp_{index}"
    wire = Wire(uri)
    replies = []
    try:
        wipe(wire, db)
        for step in steps:
            where, command = step if isinstance(step, tuple) else (db, step)
            command = dict(command)
            pick = command.pop("pick", None)
            try:
                reply = wire.command(where, resolve(command, db))
            except TimeoutError:
                wire.close()
                wire = Wire(uri)
                replies.append({"probe": "TIMEOUT"})
                continue
            except (ConnectionError, OSError) as exc:
                wire.close()
                wire = Wire(uri)
                replies.append({"probe": f"CONNECTION {type(exc).__name__}"})
                continue
            replies.append(reduce(reply, pick))
        wipe(wire, db)
        wire.command(db, {"dropDatabase": 1})
    except (TimeoutError, ConnectionError, OSError):
        print(f"  NOTE: cleanup of {db} did not finish on {uri}", file=sys.stderr)
    finally:
        wire.close()
    return replies


def differences(want, got, path=""):
    """The paths at which two replies differ."""
    if isinstance(want, dict) and isinstance(got, dict):
        out = []
        for k in want:
            where = f"{path}.{k}" if path else str(k)
            if k not in got:
                out.append(f"-{where}")
            else:
                out.extend(differences(want[k], got[k], where))
        out.extend(f"+{path}.{k}" if path else f"+{k}" for k in got if k not in want)
        if not out and list(want) != list(got):
            out.append(f"~{path or 'reply'}: key order")
        return out
    if repr(want) != repr(got):
        return [f"~{path}: {str(want)[:400]} | {str(got)[:400]}"]
    return []


def show(step):
    where, command = step if isinstance(step, tuple) else (None, step)
    body = {k: v for k, v in command.items() if k != "pick"}
    return (("" if where is None else f"[{where}] ") + repr(body))[:200]


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "1500"))

    # Self-check: the reference must store a user, list it and refuse a
    # second of the same name.
    check = run_scenario(mongod_uri, 0, [user("a", roles=["read"]), info("a"), user("a")])
    listed = check[1].get("users", [{}])
    if not (
        check[0].get("ok") == 1
        and len(listed) == 1
        and listed[0].get("roles") == [{"role": "read", "db": "urp_0"}]
        and check[2].get("code") == 51003
    ):
        print("self-check failed on mongod:", check, file=sys.stderr)
        return 2

    divergent = replies = replies_bad = total = 0
    for index, (name, steps) in enumerate(S, start=1):
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, index, steps)
        got = run_scenario(server_uri, index, steps)
        replies += len(steps)
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name}")
        for i in bad:
            print(f"  step {i}: {show(steps[i])}")
            both = isinstance(want[i], dict) and isinstance(got[i], dict)
            found = differences(want[i], got[i]) if both else []
            one_failed = both and want[i].get("ok") != got[i].get("ok")
            if one_failed or len(found) > 10 or not found:
                print(f"    mongod: {str(want[i])[:width]}")
                print(f"    server: {str(got[i])[:width]}")
            else:
                for line in found:
                    print(f"    {line[:width]}")
    print(
        f"\n=== user and role commands: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
