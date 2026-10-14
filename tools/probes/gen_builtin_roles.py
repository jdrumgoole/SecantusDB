"""Generate crates/secantus-commands/src/builtin_roles.rs from a running mongod.

Reads three things from the server named by PROBE_MONGOD (default
mongodb://127.0.0.1:27041): the order in which it lists action types (by
creating a role on `admin` holding every action named in ACTION_CANDIDATES
and reading it back), and the built-in roles of a normal database and of
`admin`, each with its privileges (`rolesInfo: 1, showBuiltinRoles: true,
showPrivileges: true`).

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/gen_builtin_roles.py \\
        > crates/secantus-commands/src/builtin_roles.rs
"""
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _wire import Wire  # noqa: E402

URI = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
DB = "gen_builtin_roles_db"
HOLDER = "gen_builtin_roles_all_actions"

# The ActionType enum of mongodb/mongo r8.2.1 src/mongo/db/auth/action_type.idl,
# in file order. The server is asked to confirm every name and to say in what
# order it renders them; the file order is only the starting point.
ACTION_CANDIDATES = """addShard advanceClusterTime listClusterCatalog allCollectionStats analyze
analyzeShardKey anyAction appendOplogNote applicationMessage auditConfigure auditLogRotate
authCheck authenticate authSchemaUpgrade bypassDefaultMaxTimeMS bypassDocumentValidation
bypassWriteBlockingMode changeCustomData changePassword changeOwnPassword changeOwnCustomData
changeStream checkFreeMonitoringStatus checkMetadataConsistency cleanupOrphaned clearJumboFlag
cleanupStructuredEncryptionData closeAllDatabases collMod collStats configureBackgroundTask
compact compactStructuredEncryptionData configureQueryAnalyzer connPoolStats connPoolSync
convertToCapped cpuProfiler createCollection createDatabase createIndex createRole
createSearchIndexes createUser dbCheck dbHash dbStats dropAllRolesFromDatabase
dropAllUsersFromDatabase dropCollection dropConnections dropDatabase dropIndex dropRole
dropSearchIndex dropUser emptycapped enableProfiler enableSharding exportCollection find
flushRouterConfig forceUUID fsync fsyncUnlock getClusterParameter getDatabaseVersion
getDefaultRWConcern getCmdLineOpts getLog getParameter getShardMap getShardVersion grantRole
grantPrivilegesToRole grantRolesToRole grantRolesToUser hostInfo impersonate importCollection
indexStats inprog insert internal invalidateUserCache issueDirectShardOperations killAnyCursor
killAnySession killCursors killop listCachedAndActiveUsers listCollections listCursors
listDatabases listIndexes listSampledQueries listSearchIndexes listSessions listShards logRotate
updateSearchIndex moveChunk moveCollection netstat oidcListKeys oidcRefreshKeys oidReset
planCacheIndexFilter planCacheRead planCacheWrite querySettings queryStatsRead
queryStatsReadTransformed performRawDataOperations refineCollectionShardKey reIndex
releaseMemoryAnyCursor remove removeShard renameCollection renameCollectionSameDB repairDatabase
replSetConfigure replSetGetConfig replSetGetStatus replSetHeartbeat replSetReconfig
replSetResizeOplog replSetStateChange reshardCollection resync revokeRole revokePrivilegesFromRole
revokeRolesFromRole revokeRolesFromUser rotateCertificates runAsLessPrivilegedUser serverStatus
setAuthenticationRestriction setClusterParameter setDefaultRWConcern setFeatureCompatibilityVersion
setFreeMonitoring setParameter setUserWriteBlockMode shardCollection shardedDataDistribution
shardingState shutdown splitChunk splitVector storageDetails top touch trafficRecord
transitionFromDedicatedConfigServer transitionToDedicatedConfigServer unlock unshardCollection
useTenant useUUID update updateRole updateUser validate viewRole viewUser applyOps
setChangeStreamState getChangeStreamState""".split()


def command(wire, db, cmd):
    reply = wire.command(db, cmd)
    if reply.get("ok") != 1:
        raise SystemExit(f"{db} {cmd}: {reply}")
    return reply


def main():
    wire = Wire(URI)
    version = command(wire, "admin", {"buildInfo": 1})["version"]
    wire.command("admin", {"dropRole": HOLDER})

    # 1. The action order. Shuffled so the order read back is the server's.
    # `anyAction` swallows every other action of a privilege (the role read
    # back holds `anyAction` alone), so it is left out of the holder and kept
    # at its file position, which the other 169 confirm is the enum's order.
    # 2. The built-in roles, with privileges, of a normal database and of admin.
    def builtins(db):
        reply = command(
            wire, db, {"rolesInfo": 1, "showBuiltinRoles": True, "showPrivileges": True}
        )
        return [r for r in reply["roles"] if r["isBuiltin"]]

    normal = builtins(DB)
    admin = builtins("admin")
    wire.command(DB, {"dropDatabase": 1})

    # 1. The action order. The candidates are the file's names plus any a
    # built-in role holds that the file lacks (a later release's additions).
    # A name the server does not know is dropped, and said so on stderr.
    names = list(ACTION_CANDIDATES)
    for role in normal + admin:
        for privilege in role["privileges"]:
            for action in privilege["actions"]:
                if action not in names:
                    print(f"an action the file lacks, kept: {action}", file=sys.stderr)
                    names.append(action)
    known = []
    for name in names:
        if name == "anyAction":
            continue
        reply = wire.command(
            "admin",
            {
                "createRole": HOLDER,
                "privileges": [{"resource": {"anyResource": True}, "actions": [name]}],
                "roles": [],
            },
        )
        if reply.get("ok") == 1:
            known.append(name)
            wire.command("admin", {"dropRole": HOLDER})
        else:
            print(f"not an action of this build, dropped: {name}: {reply['errmsg']}", file=sys.stderr)
    candidates = list(known)
    random.shuffle(candidates)
    reply = wire.command(
        "admin",
        {
            "createRole": HOLDER,
            "privileges": [{"resource": {"anyResource": True}, "actions": candidates}],
            "roles": [],
        },
    )
    if reply.get("ok") != 1:
        raise SystemExit(f"the action list needs correcting: {reply}")
    info = command(wire, "admin", {"rolesInfo": HOLDER, "showPrivileges": True})
    actions = info["roles"][0]["privileges"][0]["actions"]
    if sorted(actions) != sorted(known):
        raise SystemExit(f"read back {len(actions)} actions for {len(known)}")
    # The server's order, with `anyAction` at the file's place for it (after
    # `analyzeShardKey`), which the rest of the file's order being the
    # server's makes trustworthy.
    file_order = [a for a in ACTION_CANDIDATES if a in known]
    if [a for a in actions if a in file_order] != file_order:
        raise SystemExit(f"the server's action order is not the file's: {actions}")
    at = actions.index("analyzeShardKey") + 1
    actions = actions[:at] + ["anyAction"] + actions[at:]
    wire.command("admin", {"dropRole": HOLDER})

    def resource(res, db):
        if res.get("cluster") is True:
            return "Res::Cluster"
        if res.get("anyResource") is True:
            return "Res::Any"
        if "system_buckets" in res:
            # A `system_buckets` resource of every database carries no `db`.
            return f'Res::SystemBuckets({rs(res.get("db", ""), db)}, {rs(res["system_buckets"], db)})'
        return f'Res::Ns({rs(res["db"], db)}, {rs(res["collection"], db)})'

    def rs(text, db):
        if text == db:
            return '"@db"'
        if db is not None and db in text:
            raise SystemExit(f"{text!r} embeds the database name")
        return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'

    out = []
    out.append("//! mongod's own built-in roles: which exist on a database, which only on")
    out.append("//! `admin`, and the privileges of each.")
    out.append("//!")
    out.append(f"//! GENERATED from mongod {version} (2026-10-13) by")
    out.append("//! `tools/probes/gen_builtin_roles.py`: `rolesInfo` with `showBuiltinRoles`")
    out.append("//! and `showPrivileges` on a normal database (whose name the table writes")
    out.append("//! as `@db`) and on `admin`, and a role created with every action type")
    out.append("//! for the order the server lists actions in. Do not edit a row by hand:")
    out.append("//! re-run the generator against the build being targeted.")
    out.append("")
    out.append("#![cfg_attr(rustfmt, rustfmt_skip)]")
    out.append("")
    out.append("use crate::privileges::{Res, RoleTable};")
    out.append("")
    out.append("/// Every action type, in the order mongod renders a privilege's actions.")
    out.append("pub(crate) const ACTIONS: &[&str] = &[")
    for a in actions:
        out.append(f'    "{a}",')
    out.append("];")
    out.append("")

    def table(name, doc, roles, db):
        out.append(f"/// {doc}")
        out.append(f"pub(crate) const {name}: RoleTable = &[")
        for role in roles:
            out.append(f'    ("{role["role"]}", &[')
            for p in role["privileges"]:
                acts = ", ".join(f'"{a}"' for a in p["actions"])
                out.append(f"        ({resource(p['resource'], db)}, &[{acts}]),")
            out.append("    ]),")
        out.append("];")
        out.append("")

    table(
        "DATABASE_ROLES",
        "The built-in roles of every database, in the order mongod listed them.",
        normal,
        DB,
    )
    table(
        "ADMIN_ROLES",
        "The built-in roles of `admin`, in the order mongod listed them.",
        admin,
        None,
    )
    sys.stdout.write("\n".join(out))
    wire.close()


if __name__ == "__main__":
    main()
