"""What an authenticated user may run: mongod `--auth` against the Rust server.

The probe starts both servers itself with access control on an empty store,
makes the first user through the localhost exception, and then gives one
user each of a list of roles -- every built-in role, and stored roles whose
privileges name one collection, one database, a collection name on every
database, the cluster, and another role. Each user runs the same list of
commands; what is compared is whether the command ran and, when it did not,
the error code. The message of an `Unauthorized` names the whole command
with its session id and is not compared.

    PROBE_RUST_BIN=crates/secantusdb/target/release/secantusd-rs \\
        python tools/probes/auth_enforcement.py

`PROBE_ONLY=<substring>` keeps the users or commands whose name contains it;
`PROBE_MONGOD_BIN` names another mongod; `PROBE_DUMP=<file>` writes every
row, agreeing or not, as `key<TAB>mongod<TAB>rust`. A command that changes
something is given a `prep`, run as root before each user's attempt, so that
every user meets the same state. A command may be a function of
`(connection, root connection, user name)` where what is run depends on
state only the probe can make, such as a cursor another user opened.

Both servers run with test commands enabled, so that `configureFailPoint`
is a command and not `CommandNotFound`.

The self-check: on mongod, root runs every command, and the user with no
role is refused `find`. The run aborts if either is not so.
"""

from __future__ import annotations

import contextlib
import functools
import os
import shutil
import subprocess
import sys
import tempfile
import time

import pymongo
from bson import Int64
from bson.code import Code
from pymongo.errors import OperationFailure, PyMongoError

MONGOD_BIN = os.environ.get("PROBE_MONGOD_BIN", "mongod")
RUST_BIN = os.environ.get("PROBE_RUST_BIN")
ONLY = os.environ.get("PROBE_ONLY", "")
DUMP = os.environ.get("PROBE_DUMP")
PORTS = {"mongod": 27461, "rust": 27462}
ROOT = ("ae_root", "ae_root_pw")
DB, DB2 = "ae", "ae2"

#: Stored roles, made on `admin` so that they may name any resource:
#: `name -> (privileges, roles)`.
CUSTOM = {
    "collFind": ([({"db": DB, "collection": "c1"}, ["find"])], []),
    "collWrite": ([({"db": DB, "collection": "c1"}, ["insert", "update", "remove"])], []),
    "collIndex": (
        [({"db": DB, "collection": "c1"}, ["createIndex", "dropIndex", "listIndexes"])],
        [],
    ),
    "dbFind": ([({"db": DB, "collection": ""}, ["find"])], []),
    "nameFind": ([({"db": "", "collection": "c1"}, ["find"])], []),
    "everyFind": ([({"db": "", "collection": ""}, ["find"])], []),
    "clusterStatus": ([({"cluster": True}, ["serverStatus", "listDatabases"])], []),
    "inherits": ([], [{"role": "collFind", "db": "admin"}]),
    "inheritsRead": ([], [{"role": "read", "db": DB}]),
    "anyResource": ([({"anyResource": True}, ["find", "insert"])], []),
    "twoColls": (
        [
            ({"db": DB, "collection": "c1"}, ["find"]),
            ({"db": DB, "collection": "c2"}, ["find", "insert"]),
        ],
        [],
    ),
    "outOnly": (
        [
            ({"db": DB, "collection": "c1"}, ["find"]),
            ({"db": DB, "collection": "out"}, ["insert", "remove"]),
        ],
        [],
    ),
    # `c1` exists, so `create c1` tells whether the action was checked (48)
    # or refused (13).
    "collDdl": (
        [
            (
                {"db": DB, "collection": "c1"},
                ["createCollection", "dropCollection", "convertToCapped", "collMod"],
            ),
            ({"db": DB, "collection": "tmp"}, ["dropCollection", "convertToCapped"]),
        ],
        [],
    ),
    "collInsertOnly": ([({"db": DB, "collection": "made"}, ["insert"])], []),
    "collRename": (
        [
            ({"db": DB, "collection": "tmp"}, ["renameCollectionSameDB", "find", "dropCollection"]),
            ({"db": DB, "collection": "renamed"}, ["insert", "createIndex", "dropCollection"]),
        ],
        [],
    ),
    "dbRename": ([({"db": DB, "collection": ""}, ["renameCollectionSameDB"])], []),
    "clusterKill": ([({"cluster": True}, ["killAnyCursor", "killAnySession", "killop"])], []),
    "collKill": ([({"db": DB, "collection": "c1"}, ["killCursors"])], []),
    "ownPwd": (
        [({"db": "admin", "collection": ""}, ["changeOwnPassword", "changeOwnCustomData"])],
        [],
    ),
    "dbViewUser": ([({"db": DB, "collection": ""}, ["viewUser", "viewRole"])], []),
    "sysProfileFind": ([({"db": DB, "collection": "system.profile"}, ["find"])], []),
    "dbProfiler": ([({"db": DB, "collection": ""}, ["enableProfiler"])], []),
    "localFind": ([({"db": "local", "collection": ""}, ["find"])], []),
    "configFind": ([({"db": "config", "collection": ""}, ["find"])], []),
    "nameSysJs": ([({"db": "", "collection": "system.js"}, ["find"])], []),
    "everyWrite": (
        [
            (
                {"db": "", "collection": ""},
                [
                    "insert",
                    "update",
                    "remove",
                    "createCollection",
                    "dropCollection",
                    "createIndex",
                    "dropIndex",
                    "renameCollectionSameDB",
                    "dropDatabase",
                    "listCollections",
                    "collStats",
                    "dbStats",
                ],
            )
        ],
        [],
    ),
    "dbUserAdminLike": (
        [
            (
                {"db": DB, "collection": ""},
                [
                    "revokeRole",
                    "grantRole",
                    "createRole",
                    "dropRole",
                    "createUser",
                    "dropUser",
                    "viewRole",
                    "viewUser",
                    "changePassword",
                    "changeCustomData",
                    "setAuthenticationRestriction",
                ],
            )
        ],
        [],
    ),
    "anyNormalRevoke": (
        [
            ({"db": "", "collection": ""}, ["revokeRole"]),
            ({"db": DB, "collection": ""}, ["grantRole"]),
        ],
        [],
    ),
    "grantAdmin": (
        [
            ({"db": "admin", "collection": ""}, ["grantRole"]),
            ({"db": DB, "collection": ""}, ["createRole", "grantRole"]),
        ],
        [],
    ),
    "viewMaker": (
        [
            ({"db": DB, "collection": "v"}, ["createCollection"]),
            ({"db": DB, "collection": "c1"}, ["find"]),
        ],
        [],
    ),
    "viewMakerNoFind": ([({"db": DB, "collection": "v"}, ["createCollection"])], []),
    "bypass": ([({"db": DB, "collection": "c1"}, ["insert", "bypassDocumentValidation"])], []),
    "clusterInprog": ([({"cluster": True}, ["inprog"])], []),
    "clusterListSessions": ([({"cluster": True}, ["listSessions"])], []),
    "collStatsOnly": (
        [
            (
                {"db": DB, "collection": "c1"},
                ["collStats", "indexStats", "dbStats", "listIndexes", "validate", "dbHash"],
            )
        ],
        [],
    ),
    "dbStatsOnly": (
        [({"db": DB, "collection": ""}, ["dbStats", "collStats", "listCollections"])],
        [],
    ),
    "mapOut": (
        [
            ({"db": DB, "collection": "c1"}, ["find"]),
            ({"db": DB, "collection": "out"}, ["insert", "update", "remove"]),
        ],
        [],
    ),
    "mapMerge": (
        [
            ({"db": DB, "collection": "c1"}, ["find"]),
            ({"db": DB, "collection": "out"}, ["insert", "update"]),
        ],
        [],
    ),
    "collUpdateOnly": ([({"db": DB, "collection": "c1"}, ["update"])], []),
    "collCreateOnly": ([({"db": DB, "collection": "made"}, ["createCollection"])], []),
    "cloneCapped": (
        [
            ({"db": DB, "collection": "c1"}, ["find"]),
            ({"db": DB, "collection": "capped"}, ["insert", "createIndex", "convertToCapped"]),
        ],
        [],
    ),
}

#: `user -> roles`. One role each, so a difference names the role.
USERS: dict[str, list] = {
    "none": [],
    **{
        f"db_{r}": [{"role": r, "db": DB}]
        for r in ("read", "readWrite", "dbAdmin", "userAdmin", "dbOwner", "enableSharding")
    },
    **{
        f"adm_{r}": [{"role": r, "db": "admin"}]
        for r in (
            "read",
            "readWrite",
            "dbAdmin",
            "userAdmin",
            "dbOwner",
            "readAnyDatabase",
            "readWriteAnyDatabase",
            "dbAdminAnyDatabase",
            "userAdminAnyDatabase",
            "clusterMonitor",
            "clusterManager",
            "hostManager",
            "clusterAdmin",
            "backup",
            "restore",
            "root",
            "directShardOperations",
        )
    },
    "other_readWrite": [{"role": "readWrite", "db": DB2}],
    **{f"c_{r}": [{"role": r, "db": "admin"}] for r in CUSTOM},
}


def reset(root):
    """The state every user's attempt starts from."""
    d = root[DB]
    # Dropped rather than emptied, so that a `collMod` or an index one user
    # was allowed does not reach the next.
    for name in ("c1", "c2"):
        d[name].drop()
        d[name].insert_many([{"_id": i, "a": i} for i in range(3)])
    for name in ("out", "tmp", "renamed", "made", "v", "capped", "system.js"):
        d[name].drop()
    # Left behind by a view, and listed to some users and not others.
    with contextlib.suppress(OperationFailure):
        d["system.views"].drop()
    root[DB2]["c1"].delete_many({})
    root[DB2]["c1"].insert_one({"_id": 1})
    root[DB2]["out"].drop()
    root["local"]["stuff"].drop()


def prep_tmp(root):
    root[DB]["tmp"].drop()
    root[DB]["renamed"].drop()
    root[DB]["tmp"].insert_one({"_id": 1})


def prep_tmp2(root):
    prep_tmp(root)
    root[DB]["renamed"].insert_one({"_id": 1})


def prep_capped(root):
    root[DB]["capped"].drop()


def prep_index(root):
    root[DB]["c1"].drop_indexes()
    root[DB]["c1"].create_index("a", name="a_1")


def prep_user(root):
    for cmd, name in (("dropUser", "victim"), ("dropRole", "vrole")):
        with contextlib.suppress(OperationFailure):
            root[DB].command(cmd, name)
    root[DB].command("createUser", "victim", pwd="x", roles=[])
    root[DB].command("createRole", "vrole", privileges=[], roles=[])


def prep_nouser(root):
    prep_user(root)
    root[DB].command("dropUser", "victim")
    root[DB].command("dropRole", "vrole")


def prep_scratch(root):
    root["ae_scratch"]["c"].insert_one({})


def prep_made(root):
    root[DB]["made"].drop()
    root[DB]["v"].drop()


def names(reply):
    return sorted(c["name"] for c in reply["cursor"]["firstBatch"])


def db_names(reply):
    return sorted(
        d["name"] for d in reply["databases"] if d["name"] in (DB, DB2, "admin", "local", "config")
    )


def killed(reply):
    return (len(reply["cursorsKilled"]), len(reply["cursorsNotFound"]))


def open_cursor(client):
    """A cursor on `c1` with two documents still to come."""
    return client[DB].command({"find": "c1", "batchSize": 1})["cursor"]["id"]


def getmore_own(conn, root, user):
    return conn[DB].command({"getMore": open_cursor(conn), "collection": "c1"})


def getmore_roots(conn, root, user):
    return conn[DB].command({"getMore": open_cursor(root), "collection": "c1"})


def kill_own(conn, root, user):
    return conn[DB].command({"killCursors": "c1", "cursors": [open_cursor(conn)]})


def kill_roots(conn, root, user):
    return conn[DB].command({"killCursors": "c1", "cursors": [open_cursor(root)]})


MAP_REDUCE = {
    "map": Code("function() { emit(this.a, 1); }"),
    "reduce": Code("function(k, v) { return Array.sum(v); }"),
}


def count(reply):
    return len(reply["cursor"]["firstBatch"])


#: `(label, database, command, prep, pick)`. `pick` reduces a reply that
#: ran to what is compared; without one only "it ran" is.
COMMANDS = [
    ("find c1", DB, {"find": "c1"}, None, count),
    ("find c2", DB, {"find": "c2"}, None, count),
    ("find other db", DB2, {"find": "c1"}, None, count),
    ("find missing collection", DB, {"find": "nope"}, None, count),
    ("find system.users", "admin", {"find": "system.users"}, None, None),
    ("find system.roles", "admin", {"find": "system.roles"}, None, None),
    ("find system.js", DB, {"find": "system.js"}, None, count),
    ("find system.profile", DB, {"find": "system.profile"}, None, count),
    ("find local oplog", "local", {"find": "oplog.rs", "limit": 1}, None, None),
    ("count c1", DB, {"count": "c1"}, None, None),
    ("distinct c1", DB, {"distinct": "c1", "key": "a"}, None, None),
    ("aggregate c1", DB, {"aggregate": "c1", "pipeline": [], "cursor": {}}, None, count),
    ("aggregate c2", DB, {"aggregate": "c2", "pipeline": [], "cursor": {}}, None, count),
    (
        "aggregate c1 $lookup c2",
        DB,
        {
            "aggregate": "c1",
            "pipeline": [
                {"$lookup": {"from": "c2", "localField": "a", "foreignField": "a", "as": "j"}}
            ],
            "cursor": {},
        },
        None,
        count,
    ),
    (
        "aggregate c1 $unionWith c2",
        DB,
        {"aggregate": "c1", "pipeline": [{"$unionWith": "c2"}], "cursor": {}},
        None,
        count,
    ),
    (
        "aggregate c1 $out",
        DB,
        {"aggregate": "c1", "pipeline": [{"$out": "out"}], "cursor": {}},
        reset,
        None,
    ),
    (
        "aggregate c1 $merge c2",
        DB,
        {"aggregate": "c1", "pipeline": [{"$merge": {"into": "c2"}}], "cursor": {}},
        reset,
        None,
    ),
    (
        "aggregate $collStats",
        DB,
        {"aggregate": "c1", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $indexStats",
        DB,
        {"aggregate": "c1", "pipeline": [{"$indexStats": {}}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $currentOp",
        "admin",
        {"aggregate": 1, "pipeline": [{"$currentOp": {}}, {"$limit": 1}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $listLocalSessions",
        DB,
        {"aggregate": 1, "pipeline": [{"$listLocalSessions": {}}, {"$limit": 1}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $changeStream",
        DB,
        {"aggregate": "c1", "pipeline": [{"$changeStream": {}}], "cursor": {}},
        None,
        None,
    ),
    ("explain find c1", DB, {"explain": {"find": "c1"}}, None, None),
    ("explain find c2", DB, {"explain": {"find": "c2"}}, None, None),
    ("insert c1", DB, {"insert": "c1", "documents": [{"_id": 99}]}, reset, None),
    ("insert c2", DB, {"insert": "c2", "documents": [{"_id": 99}]}, reset, None),
    ("insert new collection", DB, {"insert": "made", "documents": [{"_id": 1}]}, prep_made, None),
    ("insert other db", DB2, {"insert": "c1", "documents": [{"_id": 99}]}, reset, None),
    (
        "update c1",
        DB,
        {"update": "c1", "updates": [{"q": {"_id": 0}, "u": {"$set": {"b": 1}}}]},
        reset,
        None,
    ),
    (
        "update c1 upsert",
        DB,
        {"update": "c1", "updates": [{"q": {"_id": 50}, "u": {"$set": {"b": 1}}, "upsert": True}]},
        reset,
        None,
    ),
    (
        "update c2",
        DB,
        {"update": "c2", "updates": [{"q": {"_id": 0}, "u": {"$set": {"b": 1}}}]},
        reset,
        None,
    ),
    ("delete c1", DB, {"delete": "c1", "deletes": [{"q": {"_id": 0}, "limit": 1}]}, reset, None),
    ("delete c2", DB, {"delete": "c2", "deletes": [{"q": {"_id": 0}, "limit": 1}]}, reset, None),
    (
        "findAndModify update c1",
        DB,
        {"findAndModify": "c1", "query": {"_id": 0}, "update": {"$set": {"b": 1}}},
        reset,
        None,
    ),
    (
        "findAndModify remove c1",
        DB,
        {"findAndModify": "c1", "query": {"_id": 0}, "remove": True},
        reset,
        None,
    ),
    (
        "findAndModify upsert c1",
        DB,
        {"findAndModify": "c1", "query": {"_id": 60}, "update": {"$set": {"b": 1}}, "upsert": True},
        reset,
        None,
    ),
    (
        "findAndModify c2",
        DB,
        {"findAndModify": "c2", "query": {"_id": 0}, "update": {"$set": {"b": 1}}},
        reset,
        None,
    ),
    (
        "bulkWrite insert c1",
        "admin",
        {
            "bulkWrite": 1,
            "ops": [{"insert": 0, "document": {"_id": 98}}],
            "nsInfo": [{"ns": f"{DB}.c1"}],
        },
        reset,
        None,
    ),
    ("listIndexes c1", DB, {"listIndexes": "c1"}, prep_index, count),
    ("listIndexes c2", DB, {"listIndexes": "c2"}, reset, count),
    (
        "createIndexes c1",
        DB,
        {"createIndexes": "c1", "indexes": [{"key": {"b": 1}, "name": "b_1"}]},
        prep_index,
        None,
    ),
    (
        "createIndexes c2",
        DB,
        {"createIndexes": "c2", "indexes": [{"key": {"b": 1}, "name": "b_1"}]},
        None,
        None,
    ),
    ("dropIndexes c1", DB, {"dropIndexes": "c1", "index": "a_1"}, prep_index, None),
    ("listCollections", DB, {"listCollections": 1}, reset, names),
    (
        "listCollections nameOnly",
        DB,
        {"listCollections": 1, "nameOnly": True, "authorizedCollections": True},
        reset,
        names,
    ),
    (
        "listDatabases",
        "admin",
        {"listDatabases": 1, "nameOnly": True},
        reset,
        db_names,
    ),
    ("create", DB, {"create": "made"}, prep_made, None),
    ("create view", DB, {"create": "v", "viewOn": "c1", "pipeline": []}, prep_made, None),
    ("drop", DB, {"drop": "tmp"}, prep_tmp, None),
    ("collMod", DB, {"collMod": "c1", "validationLevel": "moderate"}, None, None),
    (
        "renameCollection",
        "admin",
        {"renameCollection": f"{DB}.tmp", "to": f"{DB}.renamed"},
        prep_tmp,
        None,
    ),
    (
        "renameCollection across",
        "admin",
        {"renameCollection": f"{DB}.tmp", "to": f"{DB2}.renamed"},
        prep_tmp,
        None,
    ),
    ("dropDatabase scratch", "ae_scratch", {"dropDatabase": 1}, prep_scratch, None),
    ("collStats", DB, {"collStats": "c1"}, None, None),
    ("dbStats", DB, {"dbStats": 1}, None, None),
    ("dataSize", DB, {"dataSize": f"{DB}.c1"}, None, None),
    ("validate", DB, {"validate": "c1"}, None, None),
    ("compact", DB, {"compact": "c1"}, None, None),
    ("reIndex", DB, {"reIndex": "c1"}, None, None),
    ("convertToCapped", DB, {"convertToCapped": "tmp", "size": 4096}, prep_tmp, None),
    ("profile read", DB, {"profile": -1}, None, None),
    ("profile set", DB, {"profile": 0}, None, None),
    ("dbHash", DB, {"dbHash": 1}, None, None),
    ("usersInfo", DB, {"usersInfo": 1}, prep_user, None),
    ("usersInfo self", "admin", {"usersInfo": "@self"}, None, None),
    ("usersInfo other", DB, {"usersInfo": "victim"}, prep_user, None),
    ("rolesInfo", DB, {"rolesInfo": 1}, prep_user, None),
    ("rolesInfo builtin", DB, {"rolesInfo": "read"}, None, None),
    ("rolesInfo stored", DB, {"rolesInfo": "vrole"}, prep_user, None),
    ("createUser", DB, {"createUser": "victim", "pwd": "x", "roles": []}, prep_nouser, None),
    (
        "createUser with role",
        DB,
        {"createUser": "victim", "pwd": "x", "roles": ["read"]},
        prep_nouser,
        None,
    ),
    ("updateUser other pwd", DB, {"updateUser": "victim", "pwd": "y"}, prep_user, None),
    (
        "updateUser other customData",
        DB,
        {"updateUser": "victim", "customData": {"k": 1}},
        prep_user,
        None,
    ),
    ("updateUser other roles", DB, {"updateUser": "victim", "roles": ["read"]}, prep_user, None),
    (
        "updateUser self customData",
        "admin",
        {"updateUser": "@self", "customData": {"k": 1}},
        None,
        None,
    ),
    ("dropUser", DB, {"dropUser": "victim"}, prep_user, None),
    ("grantRolesToUser", DB, {"grantRolesToUser": "victim", "roles": ["read"]}, prep_user, None),
    (
        "grantRolesToUser other db role",
        DB,
        {"grantRolesToUser": "victim", "roles": [{"role": "read", "db": DB2}]},
        prep_user,
        None,
    ),
    (
        "revokeRolesFromUser",
        DB,
        {"revokeRolesFromUser": "victim", "roles": ["read"]},
        prep_user,
        None,
    ),
    ("dropAllUsersFromDatabase", "ae_scratch", {"dropAllUsersFromDatabase": 1}, None, None),
    ("createRole", DB, {"createRole": "vrole", "privileges": [], "roles": []}, prep_nouser, None),
    (
        "createRole with privilege",
        DB,
        {
            "createRole": "vrole",
            "privileges": [{"resource": {"db": DB, "collection": "c1"}, "actions": ["find"]}],
            "roles": [],
        },
        prep_nouser,
        None,
    ),
    ("updateRole", DB, {"updateRole": "vrole", "roles": ["read"]}, prep_user, None),
    ("dropRole", DB, {"dropRole": "vrole"}, prep_user, None),
    (
        "grantPrivilegesToRole",
        DB,
        {
            "grantPrivilegesToRole": "vrole",
            "privileges": [{"resource": {"db": DB, "collection": "c1"}, "actions": ["find"]}],
        },
        prep_user,
        None,
    ),
    (
        "revokePrivilegesFromRole",
        DB,
        {
            "revokePrivilegesFromRole": "vrole",
            "privileges": [{"resource": {"db": DB, "collection": "c1"}, "actions": ["find"]}],
        },
        prep_user,
        None,
    ),
    ("grantRolesToRole", DB, {"grantRolesToRole": "vrole", "roles": ["read"]}, prep_user, None),
    (
        "revokeRolesFromRole",
        DB,
        {"revokeRolesFromRole": "vrole", "roles": ["read"]},
        prep_user,
        None,
    ),
    ("dropAllRolesFromDatabase", "ae_scratch", {"dropAllRolesFromDatabase": 1}, None, None),
    ("ping", DB, {"ping": 1}, None, None),
    ("hello", DB, {"hello": 1}, None, None),
    ("buildInfo", DB, {"buildInfo": 1}, None, None),
    ("connectionStatus", DB, {"connectionStatus": 1}, None, None),
    ("listCommands", DB, {"listCommands": 1}, None, None),
    ("whatsmyuri", "admin", {"whatsmyuri": 1}, None, None),
    ("serverStatus", "admin", {"serverStatus": 1}, None, None),
    ("hostInfo", "admin", {"hostInfo": 1}, None, None),
    ("top", "admin", {"top": 1}, None, None),
    ("getCmdLineOpts", "admin", {"getCmdLineOpts": 1}, None, None),
    ("getParameter", "admin", {"getParameter": 1, "logLevel": 1}, None, None),
    ("setParameter", "admin", {"setParameter": 1, "notablescan": False}, None, None),
    ("getLog", "admin", {"getLog": "global"}, None, None),
    ("currentOp", "admin", {"currentOp": 1}, None, None),
    ("currentOp own", "admin", {"currentOp": 1, "$ownOps": True}, None, None),
    ("killOp", "admin", {"killOp": 1, "op": 987654321}, None, None),
    ("fsync", "admin", {"fsync": 1}, None, None),
    ("logRotate", "admin", {"logRotate": 1}, None, None),
    ("replSetGetStatus", "admin", {"replSetGetStatus": 1}, None, None),
    ("replSetGetConfig", "admin", {"replSetGetConfig": 1}, None, None),
    ("getDefaultRWConcern", "admin", {"getDefaultRWConcern": 1}, None, None),
    (
        "setDefaultRWConcern",
        "admin",
        {"setDefaultRWConcern": 1, "defaultReadConcern": {"level": "local"}},
        None,
        None,
    ),
    (
        "setFeatureCompatibilityVersion",
        "admin",
        {"setFeatureCompatibilityVersion": "8.2", "confirm": True},
        None,
        None,
    ),
    ("shardingState", "admin", {"shardingState": 1}, None, None),
    ("connPoolStats", "admin", {"connPoolStats": 1}, None, None),
    ("startSession", DB, {"startSession": 1}, None, None),
    ("killAllSessions", DB, {"killAllSessions": []}, None, None),
    ("killSessions", DB, {"killSessions": []}, None, None),
    ("endSessions", DB, {"endSessions": []}, None, None),
    ("killCursors", DB, {"killCursors": "c1", "cursors": [Int64(123456789)]}, None, None),
    ("getMore unknown", DB, {"getMore": Int64(123456789), "collection": "c1"}, None, None),
    (
        "configureFailPoint",
        "admin",
        {"configureFailPoint": "failCommand", "mode": "off"},
        None,
        None,
    ),
    ("unknown command", DB, {"noSuchCommand": 1}, None, None),
    ("find config", "config", {"find": "settings"}, None, None),
    ("find admin system.version", "admin", {"find": "system.version"}, None, None),
    ("find local stuff", "local", {"find": "stuff"}, None, None),
    (
        "insert system.js",
        DB,
        {"insert": "system.js", "documents": [{"_id": "f", "value": 1}]},
        reset,
        None,
    ),
    (
        "insert c1 bypass",
        DB,
        {"insert": "c1", "documents": [{"_id": 97}], "bypassDocumentValidation": True},
        reset,
        None,
    ),
    ("insert local", "local", {"insert": "stuff", "documents": [{"_id": 1}]}, reset, None),
    ("count c2", DB, {"count": "c2"}, None, None),
    ("distinct c2", DB, {"distinct": "c2", "key": "a"}, None, None),
    (
        "aggregate c1 $out other db",
        DB,
        {"aggregate": "c1", "pipeline": [{"$out": {"db": DB2, "coll": "out"}}], "cursor": {}},
        reset,
        None,
    ),
    (
        "aggregate c1 $merge other db",
        DB,
        {
            "aggregate": "c1",
            "pipeline": [{"$merge": {"into": {"db": DB2, "coll": "out"}}}],
            "cursor": {},
        },
        reset,
        None,
    ),
    (
        "aggregate c1 $unionWith $merge",
        DB,
        {
            "aggregate": "c1",
            "pipeline": [{"$unionWith": {"coll": "c2", "pipeline": [{"$merge": {"into": "out"}}]}}],
            "cursor": {},
        },
        reset,
        None,
    ),
    (
        "aggregate $collStats c2",
        DB,
        {"aggregate": "c2", "pipeline": [{"$collStats": {"count": {}}}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $currentOp allUsers",
        "admin",
        {
            "aggregate": 1,
            "pipeline": [{"$currentOp": {"allUsers": True}}, {"$limit": 1}],
            "cursor": {},
        },
        None,
        None,
    ),
    (
        "aggregate $listLocalSessions allUsers",
        DB,
        {
            "aggregate": 1,
            "pipeline": [{"$listLocalSessions": {"allUsers": True}}, {"$limit": 1}],
            "cursor": {},
        },
        None,
        None,
    ),
    (
        "aggregate $changeStream db",
        DB,
        {"aggregate": 1, "pipeline": [{"$changeStream": {}}], "cursor": {}},
        None,
        None,
    ),
    (
        "aggregate $changeStream cluster",
        "admin",
        {
            "aggregate": 1,
            "pipeline": [{"$changeStream": {"allChangesForCluster": True}}],
            "cursor": {},
        },
        None,
        None,
    ),
    (
        "aggregate $documents",
        DB,
        {"aggregate": 1, "pipeline": [{"$documents": [{"a": 1}]}], "cursor": {}},
        None,
        count,
    ),
    ("explain count c1", DB, {"explain": {"count": "c1"}}, None, None),
    ("explain distinct c1", DB, {"explain": {"distinct": "c1", "key": "a"}}, None, None),
    (
        "explain aggregate c1 $out",
        DB,
        {"explain": {"aggregate": "c1", "pipeline": [{"$out": "out"}], "cursor": {}}},
        reset,
        None,
    ),
    (
        "explain update c1",
        DB,
        {"explain": {"update": "c1", "updates": [{"q": {}, "u": {"$set": {"b": 1}}}]}},
        None,
        None,
    ),
    (
        "explain delete c1",
        DB,
        {"explain": {"delete": "c1", "deletes": [{"q": {}, "limit": 1}]}},
        None,
        None,
    ),
    (
        "explain findAndModify remove c1",
        DB,
        {"explain": {"findAndModify": "c1", "query": {}, "remove": True}},
        None,
        None,
    ),
    ("getMore own cursor", DB, getmore_own, None, None),
    ("getMore root's cursor", DB, getmore_roots, None, None),
    ("killCursors own cursor", DB, kill_own, None, killed),
    ("killCursors root's cursor", DB, kill_roots, None, killed),
    ("mapReduce inline", DB, {"mapReduce": "c1", **MAP_REDUCE, "out": {"inline": 1}}, None, None),
    ("mapReduce replace", DB, {"mapReduce": "c1", **MAP_REDUCE, "out": "out"}, reset, None),
    (
        "mapReduce merge",
        DB,
        {"mapReduce": "c1", **MAP_REDUCE, "out": {"merge": "out"}},
        reset,
        None,
    ),
    (
        "mapReduce reduce",
        DB,
        {"mapReduce": "c1", **MAP_REDUCE, "out": {"reduce": "out"}},
        reset,
        None,
    ),
    (
        "listCollections authorizedCollections only",
        DB,
        {"listCollections": 1, "authorizedCollections": True},
        reset,
        names,
    ),
    (
        "listDatabases authorizedDatabases",
        "admin",
        {"listDatabases": 1, "nameOnly": True, "authorizedDatabases": True},
        reset,
        db_names,
    ),
    ("create c1 existing", DB, {"create": "c1"}, None, None),
    ("create capped", DB, {"create": "made", "capped": True, "size": 4096}, prep_made, None),
    ("create view no find", DB, {"create": "v", "viewOn": "c2", "pipeline": []}, prep_made, None),
    (
        "renameCollection dropTarget",
        "admin",
        {"renameCollection": f"{DB}.tmp", "to": f"{DB}.renamed", "dropTarget": True},
        prep_tmp2,
        None,
    ),
    (
        "cloneCollectionAsCapped",
        DB,
        {"cloneCollectionAsCapped": "c1", "toCollection": "capped", "size": 4096},
        prep_capped,
        None,
    ),
    ("collMod c2", DB, {"collMod": "c2", "validationLevel": "moderate"}, None, None),
    ("profile read slowms", DB, {"profile": -1, "slowms": 100}, None, None),
    ("usersInfo forAllDBs", "admin", {"usersInfo": {"forAllDBs": True}}, None, None),
    (
        "usersInfo self showPrivileges",
        "admin",
        {"usersInfo": "@self", "showPrivileges": True},
        None,
        None,
    ),
    ("rolesInfo showBuiltinRoles", DB, {"rolesInfo": 1, "showBuiltinRoles": True}, None, None),
    ("rolesInfo other db role", DB, {"rolesInfo": {"role": "read", "db": DB2}}, None, None),
    (
        "createUser with other db role",
        DB,
        {"createUser": "victim", "pwd": "x", "roles": [{"role": "read", "db": DB2}]},
        prep_nouser,
        None,
    ),
    (
        "createUser with restrictions",
        DB,
        {
            "createUser": "victim",
            "pwd": "x",
            "roles": [],
            "authenticationRestrictions": [{"clientSource": ["127.0.0.1"]}],
        },
        prep_nouser,
        None,
    ),
    (
        "updateUser other restrictions",
        DB,
        {"updateUser": "victim", "authenticationRestrictions": [{"clientSource": ["127.0.0.1"]}]},
        prep_user,
        None,
    ),
    ("updateUser self pwd", "admin", {"updateUser": "@self", "pwd": "pw"}, None, None),
    (
        "createRole with cluster privilege",
        DB,
        {
            "createRole": "vrole",
            "privileges": [{"resource": {"cluster": True}, "actions": ["serverStatus"]}],
            "roles": [],
        },
        prep_nouser,
        None,
    ),
    (
        "createRole with other db privilege",
        DB,
        {
            "createRole": "vrole",
            "privileges": [{"resource": {"db": DB2, "collection": ""}, "actions": ["find"]}],
            "roles": [],
        },
        prep_nouser,
        None,
    ),
    (
        "createRole with other db role",
        DB,
        {"createRole": "vrole", "privileges": [], "roles": [{"role": "read", "db": DB2}]},
        prep_nouser,
        None,
    ),
    (
        "updateRole privileges",
        DB,
        {
            "updateRole": "vrole",
            "privileges": [{"resource": {"db": DB, "collection": "c1"}, "actions": ["find"]}],
        },
        prep_user,
        None,
    ),
    (
        "grantPrivilegesToRole cluster",
        DB,
        {
            "grantPrivilegesToRole": "vrole",
            "privileges": [{"resource": {"cluster": True}, "actions": ["serverStatus"]}],
        },
        prep_user,
        None,
    ),
    ("refreshSessions", DB, {"refreshSessions": []}, None, None),
    ("logout", DB, {"logout": 1}, None, None),
    ("fsyncUnlock", "admin", {"fsyncUnlock": 1}, None, None),
    ("getParameter all", "admin", {"getParameter": "*"}, None, None),
    ("setParameter logLevel", "admin", {"setParameter": 1, "logLevel": 0}, None, None),
    # Last: a user that may do this loses its roles for the rest of its turn.
    ("updateUser self roles", "admin", {"updateUser": "@self", "roles": []}, None, None),
]


def start(kind: str, home: str):
    port = PORTS[kind]
    if kind == "mongod":
        # A one-member replica set, as the Rust server reports itself: a
        # standalone answers `$changeStream`, `replSetGetStatus` and the
        # default read/write concern commands differently whoever runs
        # them (RM-024). Access control on a set needs a key file.
        key_file = os.path.join(home, "probe-key")
        with open(key_file, "w") as f:
            f.write("secantusprobekey" * 4)
        os.chmod(key_file, 0o600)
        argv = [
            MONGOD_BIN,
            "--auth",
            "--replSet",
            "rs0",
            "--keyFile",
            key_file,
            "--port",
            str(port),
            "--dbpath",
            home,
            "--bind_ip",
            "127.0.0.1",
            "--setParameter",
            "enableTestCommands=1",
            # This server has no JavaScript engine and refuses what needs one
            # as a `--noscripting` mongod does (Joe, 2026-10-16).
            "--noscripting",
        ]
    else:
        argv = [
            RUST_BIN,
            "--auth",
            "--enable-test-commands",
            "--port",
            str(port),
            "--storage-path",
            home,
        ]
    with open(os.path.join(home, "probe-server.log"), "w") as log:
        proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT)
    uri = f"mongodb://127.0.0.1:{port}/?directConnection=true"
    for _ in range(300):
        try:
            pymongo.MongoClient(uri, serverSelectionTimeoutMS=200).admin.command("ping")
            return proc
        except OperationFailure:
            return proc
        except PyMongoError:
            if proc.poll() is not None:
                break
            time.sleep(0.1)
    proc.kill()
    raise SystemExit(f"{kind} did not start; see {home}/probe-server.log")


def client(kind: str, user: str | None = None, pwd: str | None = None, source: str = "admin"):
    cred = f"{user}:{pwd}@" if user else ""
    tail = f"&authSource={source}" if user else ""
    return pymongo.MongoClient(
        f"mongodb://{cred}127.0.0.1:{PORTS[kind]}/?directConnection=true{tail}"
    )


def outcome(fn, pick=None):
    try:
        reply = fn()
    except OperationFailure as e:
        return ("err", e.code)
    except PyMongoError as e:
        return ("client", type(e).__name__)
    return ("ok", pick(reply) if pick else None)


def setup(kind: str):
    anon = client(kind)
    if kind == "mongod":
        # Initiated through the localhost exception, before the first user.
        anon.admin.command(
            "replSetInitiate",
            {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{PORTS[kind]}"}]},
        )
        for _ in range(600):
            if anon.admin.command("hello").get("isWritablePrimary"):
                break
            time.sleep(0.1)
        else:
            raise SystemExit("mongod's replica set elected no primary")
    anon.admin.command("createUser", ROOT[0], pwd=ROOT[1], roles=["root"])
    root = client(kind, *ROOT)
    reset(root)
    # So that `admin` and `config` exist on both servers, and `listDatabases`
    # compares who may see them rather than whether they are there.
    for db in ("admin", "config"):
        root[db]["probe"].insert_one({"_id": 1})
    made = {}
    for name, (privileges, roles) in CUSTOM.items():
        made[f"role {name}"] = outcome(
            functools.partial(
                root.admin.command,
                "createRole",
                name,
                privileges=[{"resource": res, "actions": actions} for res, actions in privileges],
                roles=roles,
            )
        )
    for user, roles in USERS.items():
        made[f"user {user}"] = outcome(
            functools.partial(root.admin.command, "createUser", user, pwd="pw", roles=roles)
        )
    return root, made


def measure(kind: str):
    root, rows = setup(kind)
    for user in USERS:
        if ONLY and ONLY not in user and not any(ONLY in c[0] for c in COMMANDS):
            continue
        if rows[f"user {user}"][0] != "ok":
            continue
        conn = client(kind, user, "pw")
        for label, db, command, prep, pick in COMMANDS:
            if ONLY and ONLY not in user and ONLY not in label:
                continue
            if prep:
                prep(root)
            if callable(command):
                run = functools.partial(command, conn, root, user)
            else:
                command = dict(command)
                first = next(iter(command))
                if command[first] == "@self":
                    command[first] = user
                run = functools.partial(conn[db].command, command)
            rows[f"{user}: {label}"] = outcome(run, pick)
        conn.close()
    return rows


def self_check(rows):
    bad = []
    if rows.get("none: find c1") not in (None, ("err", 13)):
        bad.append(f"the user with no role was not refused find: {rows['none: find c1']}")
    for key, value in rows.items():
        if value[0] == "client":
            bad.append(f"{key}: the client raised {value[1]}")
    for label, *_ in COMMANDS:
        got = rows.get(f"adm_root: {label}")
        if got is not None and got == ("err", 13):
            # A few commands refuse even root (they belong to `__system`).
            print(f"  note: mongod refuses root: {label}")
    return bad


def main() -> int:
    if not RUST_BIN:
        print("PROBE_RUST_BIN is required (see the module docstring)")
        return 2
    homes = {kind: tempfile.mkdtemp(prefix=f"ae-{kind}-") for kind in PORTS}
    procs = {}
    try:
        results = {}
        for kind in PORTS:
            procs[kind] = start(kind, homes[kind])
            results[kind] = measure(kind)
        bad = self_check(results["mongod"])
        if bad:
            print("SELF-CHECK FAILED on mongod:")
            for line in bad:
                print("  " + line)
            return 3
        ref, ours = results["mongod"], results["rust"]
        if DUMP:
            with open(DUMP, "w") as f:
                for key in ref:
                    f.write(f"{key}\t{ref[key]}\t{ours.get(key)}\n")
        differ = 0
        by_user: dict[str, list[str]] = {}
        for key in ref:
            if ref[key] != ours.get(key):
                differ += 1
                user = key.split(":")[0]
                by_user.setdefault(user, []).append(
                    f"  {key}\n      mongod: {ref[key]}\n      rust  : {ours.get(key)}"
                )
        for user, lines in by_user.items():
            print(f"--- {user} ({len(lines)})")
            print("\n".join(lines))
        print(f"=== auth enforcement: {len(USERS)} users, {len(ref)} checks -- {differ} differ ===")
        return 1 if differ else 0
    finally:
        for proc in procs.values():
            proc.terminate()
        for proc in procs.values():
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
        for home in homes.values():
            shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
