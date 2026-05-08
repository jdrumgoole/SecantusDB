"""Role-based access control: built-in roles, action mapping, privilege check.

Pure module — no I/O, no Storage import. Auth-time wiring (loading a
user's role set) lives in :mod:`secantus.auth`; per-command privilege
checks (deciding which action a command needs and what resource it
targets) live in :mod:`secantus.commands`.

This module's surface is small:

* :data:`BUILT_IN_ROLES` — the canonical role → set-of-actions table.
* :func:`role_grants_action` — single-role lookup (role on db X, target db Y).
* :func:`check_privilege` — the entry point used by the dispatcher.

Resource model
==============

MongoDB models a privilege as ``(actions, resource)`` where the
resource is a ``(db, collection)`` pair (or ``cluster``). For Secantus's
in-scope surface we collapse that to one of three shapes:

* **collection-level** action on ``(target_db, target_coll)`` — every
  CRUD command. The check passes if a user has the required action on
  ``target_db`` (built-in roles grant collection-level actions across
  all collections in their db, so the collection name is uniformly
  matched).
* **database-level** action on ``target_db`` (e.g. ``listCollections``,
  ``createCollection``, ``dropDatabase``).
* **cluster** action — ``serverStatus``, ``hostInfo``,
  ``listDatabases``, etc. Only ``root`` (and the explicit
  ``clusterAdmin`` / ``clusterMonitor`` roles, not implemented yet)
  grants cluster-wide actions.

Built-in role coverage
======================

The MVP set covers what real-world deployments use:

* ``read`` / ``readWrite`` / ``dbAdmin`` / ``userAdmin`` — single-db.
* ``readAnyDatabase`` / ``readWriteAnyDatabase`` /
  ``dbAdminAnyDatabase`` / ``userAdminAnyDatabase`` — admin-bound,
  cross-db.
* ``root`` — admin-bound, every action on every resource (including
  cluster-level actions).

``clusterAdmin`` / ``clusterMonitor`` / ``backup`` / ``restore`` and
custom roles are out of scope for the first slice; mongod recognises
them but our handlers don't enforce cluster-monitor-only actions
distinctly from root yet. ``createRole`` / ``dropRole`` / ``rolesInfo``
on custom roles return ``Unauthorized`` errors that match real mongod.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable, Mapping
from typing import Any

# ---------------------------------------------------------------------------
# Action constants
# ---------------------------------------------------------------------------

# Read-side
A_FIND = "find"
A_LIST_COLLECTIONS = "listCollections"
A_LIST_INDEXES = "listIndexes"
A_LIST_DATABASES = "listDatabases"
A_DB_STATS = "dbStats"
A_COLL_STATS = "collStats"
A_KILL_CURSORS = "killCursors"
A_CHANGE_STREAM = "changeStream"

# Write-side
A_INSERT = "insert"
A_UPDATE = "update"
A_REMOVE = "remove"
A_BYPASS_DOC_VALIDATION = "bypassDocumentValidation"

# DDL
A_CREATE_COLLECTION = "createCollection"
A_DROP_COLLECTION = "dropCollection"
A_DROP_DATABASE = "dropDatabase"
A_CREATE_INDEX = "createIndex"
A_DROP_INDEX = "dropIndex"
A_RENAME_COLL_SAME_DB = "renameCollectionSameDB"
A_COLL_MOD = "collMod"

# User / role admin
A_CREATE_USER = "createUser"
A_DROP_USER = "dropUser"
A_GRANT_ROLE = "grantRole"
A_REVOKE_ROLE = "revokeRole"
A_VIEW_USER = "viewUser"
A_VIEW_ROLE = "viewRole"
A_CHANGE_PASSWORD = "changeOwnPassword"
A_CREATE_ROLE = "createRole"
A_DROP_ROLE = "dropRole"

# Cluster-wide
A_SERVER_STATUS = "serverStatus"
A_HOST_INFO = "hostInfo"
A_GET_CMD_LINE_OPTS = "getCmdLineOpts"
A_GET_LOG = "getLog"
A_INPROG = "inprog"
A_FSYNC = "fsync"


# ---------------------------------------------------------------------------
# Resource scopes
# ---------------------------------------------------------------------------

SCOPE_COLLECTION = "collection"
SCOPE_DATABASE = "database"
SCOPE_CLUSTER = "cluster"


# ---------------------------------------------------------------------------
# Built-in roles
# ---------------------------------------------------------------------------

# A role's actions are "what it can do on its bound db". For the
# *AnyDatabase variants the same actions apply to every db. For ``root``
# every action applies including cluster-wide.

_READ_ACTIONS: frozenset[str] = frozenset(
    {
        A_FIND,
        A_LIST_COLLECTIONS,
        A_LIST_INDEXES,
        A_DB_STATS,
        A_COLL_STATS,
        A_KILL_CURSORS,
        A_CHANGE_STREAM,
    }
)

_READWRITE_ACTIONS: frozenset[str] = _READ_ACTIONS | frozenset(
    {
        A_INSERT,
        A_UPDATE,
        A_REMOVE,
        A_CREATE_COLLECTION,
        A_DROP_COLLECTION,
        A_CREATE_INDEX,
        A_DROP_INDEX,
        A_RENAME_COLL_SAME_DB,
        A_COLL_MOD,
        A_BYPASS_DOC_VALIDATION,
    }
)

_DBADMIN_ACTIONS: frozenset[str] = frozenset(
    {
        # Read-side helpers an admin needs to inspect the db.
        A_LIST_COLLECTIONS,
        A_LIST_INDEXES,
        A_DB_STATS,
        A_COLL_STATS,
        A_KILL_CURSORS,
        # DDL.
        A_CREATE_COLLECTION,
        A_DROP_COLLECTION,
        A_DROP_DATABASE,
        A_CREATE_INDEX,
        A_DROP_INDEX,
        A_RENAME_COLL_SAME_DB,
        A_COLL_MOD,
        A_VIEW_ROLE,
    }
)

_USERADMIN_ACTIONS: frozenset[str] = frozenset(
    {
        A_CREATE_USER,
        A_DROP_USER,
        A_GRANT_ROLE,
        A_REVOKE_ROLE,
        A_VIEW_USER,
        A_VIEW_ROLE,
        A_CHANGE_PASSWORD,
        # Custom-role management lives under userAdmin (mongod's
        # documented mapping) — separate ``createRole`` /
        # ``dropRole`` actions but the role grants both.
        A_CREATE_ROLE,
        A_DROP_ROLE,
    }
)

# Cluster actions only ``root`` grants in this slice.
_CLUSTER_ACTIONS: frozenset[str] = frozenset(
    {
        A_LIST_DATABASES,
        A_SERVER_STATUS,
        A_HOST_INFO,
        A_GET_CMD_LINE_OPTS,
        A_GET_LOG,
        A_INPROG,
        A_FSYNC,
    }
)


class _RoleSpec:
    """Internal: actions + scope flags for a built-in role.

    ``any_db`` means the role applies its actions to every database
    (the *AnyDatabase variants and ``root``). ``cluster`` enables the
    cluster-wide actions in :data:`_CLUSTER_ACTIONS`. ``admin_only``
    rejects the role if it's bound to a non-admin database, mirroring
    mongod (``readAnyDatabase`` etc. are only valid on ``admin``).
    """

    __slots__ = ("actions", "any_db", "cluster", "admin_only")

    def __init__(
        self,
        actions: frozenset[str],
        *,
        any_db: bool = False,
        cluster: bool = False,
        admin_only: bool = False,
    ) -> None:
        self.actions = actions
        self.any_db = any_db
        self.cluster = cluster
        self.admin_only = admin_only


BUILT_IN_ROLES: dict[str, _RoleSpec] = {
    "read": _RoleSpec(_READ_ACTIONS),
    "readWrite": _RoleSpec(_READWRITE_ACTIONS),
    "dbAdmin": _RoleSpec(_DBADMIN_ACTIONS),
    "userAdmin": _RoleSpec(_USERADMIN_ACTIONS),
    # `dbOwner` is the per-db union of readWrite + dbAdmin + userAdmin
    # (mongod's documented composition). Tests targeting non-admin
    # users typically grant this rather than enumerate the trio.
    "dbOwner": _RoleSpec(_READWRITE_ACTIONS | _DBADMIN_ACTIONS | _USERADMIN_ACTIONS),
    # *AnyDatabase variants are admin-bound and span every db.
    "readAnyDatabase": _RoleSpec(_READ_ACTIONS, any_db=True, admin_only=True),
    "readWriteAnyDatabase": _RoleSpec(_READWRITE_ACTIONS, any_db=True, admin_only=True),
    "dbAdminAnyDatabase": _RoleSpec(_DBADMIN_ACTIONS, any_db=True, admin_only=True),
    "userAdminAnyDatabase": _RoleSpec(_USERADMIN_ACTIONS, any_db=True, admin_only=True),
    # ``root`` covers everything: every action on every db, plus cluster.
    "root": _RoleSpec(
        _READWRITE_ACTIONS
        | _DBADMIN_ACTIONS
        | _USERADMIN_ACTIONS
        | _CLUSTER_ACTIONS
        | frozenset({A_DROP_DATABASE}),
        any_db=True,
        cluster=True,
        admin_only=True,
    ),
}


def is_known_role(role_name: str) -> bool:
    return role_name in BUILT_IN_ROLES


# ---------------------------------------------------------------------------
# Privilege check
# ---------------------------------------------------------------------------


def role_grants_action(
    role_name: str,
    role_db: str,
    action: str,
    *,
    target_db: str | None,
    cluster: bool = False,
) -> bool:
    """Does the bound role ``(role_name @ role_db)`` grant ``action`` on
    the target?

    * ``cluster=True`` checks cluster-wide grant (no target_db needed).
    * ``cluster=False`` requires ``target_db`` and matches when the
      role is bound to that db, OR the role is an *AnyDatabase variant.
    """
    spec = BUILT_IN_ROLES.get(role_name)
    if spec is None:
        return False
    if spec.admin_only and role_db != "admin":
        return False  # invalid binding; never grants anything
    if cluster:
        return spec.cluster and action in spec.actions
    if action not in spec.actions:
        return False
    if spec.any_db:
        return True
    return role_db == target_db


def _custom_role_grants(
    record: Mapping[str, Any],
    action: str,
    *,
    target_db: str | None,
    cluster: bool,
) -> bool:
    """Does a custom-role record's ``privileges`` array grant ``action``
    on the resource? Pure shape match — no graph walking here.
    """
    privs = record.get("privileges")
    if not isinstance(privs, list):
        return False
    for priv in privs:
        if not isinstance(priv, Mapping):
            continue
        actions = priv.get("actions") or []
        if action not in actions:
            continue
        resource = priv.get("resource") or {}
        if not isinstance(resource, Mapping):
            continue
        # `{anyResource: true}` matches anything.
        if resource.get("anyResource"):
            return True
        if cluster:
            if resource.get("cluster"):
                return True
            continue
        # Database-scoped resources. `{db: "<X>", collection: ""}`
        # matches any collection in db X; `{db: "", collection: ""}`
        # matches every database (mongod's "all dbs" sentinel).
        res_db = resource.get("db")
        if not isinstance(res_db, str):
            continue
        if res_db == "" or (target_db is not None and res_db == target_db):
            return True
    return False


def check_privilege(
    roles: Iterable[Mapping[str, Any]],
    action: str,
    *,
    target_db: str | None = None,
    cluster: bool = False,
    role_resolver: Callable[[str, str], Mapping[str, Any] | None] | None = None,
) -> bool:
    """True iff any role in ``roles`` grants ``action`` on the resource.

    ``roles`` is the user's role bindings as stored on the user record:
    a list of ``{"role": <name>, "db": <db>}`` dicts. Built-in role
    names short-circuit through :data:`BUILT_IN_ROLES`. If a role
    isn't built-in and ``role_resolver`` is supplied, it's looked up
    as a custom role: privileges are matched directly, and inherited
    roles in the record's ``roles`` array are expanded recursively
    with cycle detection. Unknown roles (no built-in, no resolver
    hit) silently grant nothing — matches mongod's "if you can't see
    the role you can't use its privileges" model.

    ``role_resolver(db, name) -> record-or-None`` matches ``Storage.
    get_role`` directly — production callers pass that as-is. The
    parameter order mirrors mongod's docs (db before role name).
    """
    visited: set[tuple[str, str]] = set()
    pending: list[tuple[str, str]] = []
    for role_dict in roles:
        if not isinstance(role_dict, Mapping):
            continue
        role_name = role_dict.get("role")
        role_db = role_dict.get("db")
        if not isinstance(role_name, str) or not isinstance(role_db, str):
            continue
        pending.append((role_name, role_db))

    while pending:
        role_name, role_db = pending.pop()
        if (role_name, role_db) in visited:
            continue
        visited.add((role_name, role_db))
        # Built-in roles win the lookup race — same name + db can't
        # collide because mongod refuses to ``createRole`` for a
        # built-in name (we mirror that in the command layer).
        if role_name in BUILT_IN_ROLES:
            if role_grants_action(role_name, role_db, action, target_db=target_db, cluster=cluster):
                return True
            continue
        if role_resolver is None:
            continue
        record = role_resolver(role_db, role_name)
        if record is None:
            continue
        if _custom_role_grants(record, action, target_db=target_db, cluster=cluster):
            return True
        # Walk inherited roles. Each entry is `{role, db}`.
        for inherited in record.get("roles") or []:
            if not isinstance(inherited, Mapping):
                continue
            inh_name = inherited.get("role")
            inh_db = inherited.get("db")
            if isinstance(inh_name, str) and isinstance(inh_db, str):
                pending.append((inh_name, inh_db))
    return False
