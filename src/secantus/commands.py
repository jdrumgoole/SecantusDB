from __future__ import annotations

import datetime as _dt
import logging
import os
import random as _random
import time as _time
from collections.abc import Callable, Mapping
from dataclasses import dataclass, replace
from typing import Any

import bson

from secantus.aggregate import AggregateError, PipelineContext, apply_pipeline
from secantus.auth import (
    SCRAM_SHA_1,
    SCRAM_SHA_256,
    AuthError,
    ConnectionAuth,
    StoredCredentials,
    begin_scram,
    continue_scram,
    derive_credentials,
)
from secantus.connreg import ConnectionRegistry
from secantus.cursors import CursorNotFound, CursorRegistry
from secantus.expressions import ExpressionError
from secantus.failpoints import FailPointRegistry
from secantus.geo import GeoError
from secantus.logbuf import LogBuffer
from secantus.metrics import Metrics
from secantus.projection import ProjectionError, apply_projection
from secantus.query import QueryError, matches
from secantus.rbac import (
    A_CHANGE_PASSWORD,
    A_COLL_MOD,
    A_COLL_STATS,
    A_CREATE_COLLECTION,
    A_CREATE_INDEX,
    A_CREATE_ROLE,
    A_CREATE_USER,
    A_DB_STATS,
    A_DROP_COLLECTION,
    A_DROP_DATABASE,
    A_DROP_INDEX,
    A_DROP_ROLE,
    A_DROP_USER,
    A_ENABLE_PROFILER,
    A_FIND,
    A_FSYNC,
    A_GET_CMD_LINE_OPTS,
    A_GET_LOG,
    A_GRANT_ROLE,
    A_HOST_INFO,
    A_INPROG,
    A_INSERT,
    A_KILL_CURSORS,
    A_LIST_COLLECTIONS,
    A_LIST_DATABASES,
    A_LIST_INDEXES,
    A_REMOVE,
    A_RENAME_COLL_SAME_DB,
    A_REVOKE_ROLE,
    A_SERVER_STATUS,
    A_UPDATE,
    A_VIEW_ROLE,
    A_VIEW_USER,
    BUILT_IN_ROLES,
    SCOPE_CLUSTER,
    SCOPE_COLLECTION,
    SCOPE_DATABASE,
    check_privilege,
    is_known_role,
)
from secantus.sessions import SessionRegistry
from secantus.storage import DuplicateKeyError, GeoExtractError, Storage
from secantus.update import UpdateError
from secantus.wire import MAX_BSON_OBJECT_SIZE, MAX_MESSAGE_SIZE

logger = logging.getLogger(__name__)

# Exception classes whose ``str()`` is intentionally user-facing — they
# carry mongod-shaped error messages (validation failures, malformed
# operators, etc.) and must be surfaced verbatim so pymongo / mongo-go /
# mongo-node / mongo-java tests see the same text real mongod produces.
# Anything OUTSIDE this tuple is treated as an internal error and gets a
# generic message + a server-side log line, never the raw exception text.
_USER_FACING_EXCEPTIONS: tuple[type[BaseException], ...] = (
    AggregateError,
    AuthError,
    DuplicateKeyError,
    ExpressionError,
    GeoError,
    GeoExtractError,
    ProjectionError,
    QueryError,
    UpdateError,
)

WIRE_VERSION = 17
SERVER_VERSION = "7.0.0"
SERVER_VERSION_ARRAY = [7, 0, 0, 0]
DEFAULT_BATCH_SIZE = 101


# Mongod's well-known error codes that crop up in failpoint tests +
# the ad-hoc errors we already emit. Used by `_code_name_for` to give
# failpoint-injected errors a plausible ``codeName`` instead of a
# generic one. The map is intentionally small — drivers only assert on
# ``code`` (the integer) for failpoint errors, so unknown codes get a
# best-effort ``Location<N>`` to match mongod's fallback shape.
_ERROR_CODE_NAMES: dict[int, str] = {
    1: "InternalError",
    2: "BadValue",
    9: "FailedToParse",
    11: "DuplicateKey",
    13: "Unauthorized",
    14: "TypeMismatch",
    18: "AuthenticationFailed",
    26: "NamespaceNotFound",
    43: "CursorNotFound",
    50: "MaxTimeMSExpired",
    59: "CommandNotFound",
    100: "UnsatisfiableWriteConcern",
    136: "CappedPositionLost",
}


def _code_name_for(code: int) -> str:
    return _ERROR_CODE_NAMES.get(code, f"Location{code}")


def _resolve_let_vars(let: Any) -> dict[str, Any] | None:
    # MongoDB 5.0+ command-level ``let`` values are aggregation
    # expressions: ``{y: {$literal: "bar"}}`` binds ``$$y`` to "bar",
    # ``{n: {$add: [1, 2]}}`` binds ``$$n`` to 3. Driver tests
    # (mongo-java-driver's ``UnifiedCrudTest#updateMany-let``)
    # depend on this — passing the raw mapping through would bind
    # ``$$y`` to the dict ``{$literal: "bar"}`` instead of the
    # string. Scalars are passed through unchanged.
    if not isinstance(let, dict):
        return None
    from secantus.expressions import evaluate

    return {name: evaluate(value, {}) for name, value in let.items()}


@dataclass
class CommandContext:
    connection_id: int
    storage: Storage
    cursors: CursorRegistry
    db_name: str = "admin"
    server_address: tuple[str, int] | None = None
    replica_set_name: str | None = None
    connection_auth: ConnectionAuth | None = None
    require_auth: bool = False
    metrics: Metrics | None = None
    connections: ConnectionRegistry | None = None
    logs: LogBuffer | None = None
    sessions: SessionRegistry | None = None
    failpoints: FailPointRegistry | None = None


def _split_into_cursor(
    docs: list[dict[str, Any]],
    batch_size: int,
    namespace: str,
    cursors: CursorRegistry,
) -> tuple[list[dict[str, Any]], int]:
    # ``batch_size == 0`` is a real value, not a "use default":
    # MongoDB defines it as "open the cursor with an empty
    # firstBatch and let the client pull via getMore". A cursor id
    # is registered so the next getMore can find the docs.
    if batch_size < 0:
        batch_size = DEFAULT_BATCH_SIZE
    first = docs[:batch_size]
    remaining = docs[batch_size:]
    if not remaining:
        return first, 0
    cursor_id = cursors.register(namespace, remaining)
    return first, cursor_id


CommandHandler = Callable[[dict[str, Any], CommandContext], dict[str, Any]]


def _hello(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    # `topologyVersion.counter` and `connectionId` MUST be int64 on the wire.
    # pymongo accepts either, but the official Go driver (mongodump/restore,
    # mongo-go-driver) refuses the handshake with "expected 'counter' to be
    # an int64 but it's a BSON 32-bit integer" when these are int32.
    response: dict[str, Any] = {
        "isWritablePrimary": True,
        "ismaster": True,
        "topologyVersion": {
            "processId": bson.ObjectId.from_datetime(_dt.datetime.now(_dt.timezone.utc)),
            "counter": bson.Int64(0),
        },
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "maxMessageSizeBytes": MAX_MESSAGE_SIZE,
        "maxWriteBatchSize": 100_000,
        "localTime": _dt.datetime.now(_dt.timezone.utc),
        "logicalSessionTimeoutMinutes": 30,
        "connectionId": bson.Int64(ctx.connection_id),
        "minWireVersion": 0,
        "maxWireVersion": WIRE_VERSION,
        "readOnly": False,
        "ok": 1.0,
    }
    if ctx.replica_set_name and ctx.server_address is not None:
        addr = f"{ctx.server_address[0]}:{ctx.server_address[1]}"
        cluster_time = ctx.storage.current_cluster_time()
        response.update(
            {
                "setName": ctx.replica_set_name,
                "setVersion": 1,
                "hosts": [addr],
                "passives": [],
                "arbiters": [],
                "primary": addr,
                "me": addr,
                "electionId": bson.ObjectId("7fffffff0000000000000001"),
                "lastWrite": {
                    "opTime": {"ts": cluster_time, "t": 1},
                    "lastWriteDate": _dt.datetime.now(_dt.timezone.utc),
                    "majorityOpTime": {"ts": cluster_time, "t": 1},
                    "majorityWriteDate": _dt.datetime.now(_dt.timezone.utc),
                },
            }
        )
    # saslSupportedMechs: drivers pass `saslSupportedMechs: "<db>.<user>"`
    # to discover which mechanisms they should attempt for that
    # principal. The reply lists exactly what the user record stores
    # credentials for — SCRAM-SHA-256 (modern default) and/or
    # SCRAM-SHA-1 (legacy). If we don't recognise the principal we
    # advertise the modern default so the driver still picks
    # something reasonable.
    sasl_mechs_for = doc.get("saslSupportedMechs")
    if isinstance(sasl_mechs_for, str):
        response["saslSupportedMechs"] = _mechs_for_principal(ctx, sasl_mechs_for)
    if ctx.require_auth:
        # Tell the client this server has access control on so it knows
        # to treat the connection as needing auth before commands flow.
        response["accessControlEnabled"] = True
    # Speculative authentication: pymongo / mongo-go-driver fold the
    # SCRAM client-first message into the `hello` body. We pull it
    # back out, run it through the regular `saslStart` handler, and
    # attach the reply under `speculativeAuthenticate`. The client
    # then skips its own saslStart and goes straight to saslContinue
    # — saving one wire round-trip on every connect.
    spec = doc.get("speculativeAuthenticate")
    if isinstance(spec, dict):
        spec_reply = _speculative_auth(spec, ctx)
        if spec_reply is not None:
            response["speculativeAuthenticate"] = spec_reply
    return response


def _speculative_auth(spec: dict[str, Any], ctx: CommandContext) -> dict[str, Any] | None:
    """Run the SCRAM saslStart that's been folded into a `hello`.

    The inner document carries `saslStart`, `mechanism`, `payload`,
    `db`. We synthesize a regular saslStart command from it (with
    `db_name` overridden to the spec's `db`) and run the existing
    handler. On failure we return ``None`` rather than the error
    envelope — the client treats a missing `speculativeAuthenticate`
    field as "speculation rejected, fall back to explicit
    saslStart", which is the right UX (a typo'd password shouldn't
    abort the whole hello).
    """
    if "saslStart" not in spec:
        return None
    inner_doc = {
        "saslStart": 1,
        "mechanism": spec.get("mechanism"),
        "payload": spec.get("payload"),
    }
    # `db` defaults to "admin" for spec auth. Run saslStart with a
    # context cloned to that database so credential lookup hits the
    # right user table.
    spec_db = spec.get("db") if isinstance(spec.get("db"), str) else "admin"
    spec_ctx = replace(ctx, db_name=spec_db)
    reply = _sasl_start(inner_doc, spec_ctx)
    if reply.get("ok") != 1.0:
        # Speculation failed — let the client retry explicitly.
        return None
    return reply


def _ping(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _build_info(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    # `version` stays at the MongoDB-compatibility value so drivers enable
    # the right feature flags (change streams, $changeStream pre-images,
    # etc.). `secantusVersion` is the SecantusDB-specific marker admin
    # tools and the ./about-page can read to know which actual build is
    # running.
    import secantus

    return {
        "version": SERVER_VERSION,
        "secantusVersion": secantus.__version__,
        "gitVersion": "0" * 40,
        "versionArray": SERVER_VERSION_ARRAY,
        "bits": 64,
        "debug": False,
        "maxBsonObjectSize": MAX_BSON_OBJECT_SIZE,
        "ok": 1.0,
    }


def _lsid_bytes_from_arg(entry: Any) -> bytes | None:
    """Extract the 16-byte UUID payload from a session-id document.

    The wire shape is ``{id: BinData(4, <uuid>)}``; the BSON layer
    surfaces the value as ``bson.Binary``. Returns ``None`` for any
    shape we don't recognise so the handler can skip silently rather
    than fail the whole command.
    """
    from bson import Binary

    if not isinstance(entry, Mapping):
        return None
    inner = entry.get("id")
    if isinstance(inner, Binary):
        raw = bytes(inner)
        if len(raw) == 16:
            return raw
    elif isinstance(inner, (bytes, bytearray)) and len(inner) == 16:
        return bytes(inner)
    return None


def _end_sessions(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop the listed sessions from the registry.

    Real ``mongod`` also kills any cursors associated with these
    sessions; SecantusDB doesn't track cursor → session affinity
    yet, so cursors live on under their own idle TTL.
    """
    if ctx.sessions is not None:
        for entry in doc.get("endSessions") or []:
            lsid = _lsid_bytes_from_arg(entry)
            if lsid is not None:
                ctx.sessions.unregister(lsid)
    return {"ok": 1.0}


def _start_session(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Mint a fresh session id and register it.

    The driver receives ``{id: BinData(4, <uuid>)}`` and threads it
    onto every subsequent command via the top-level ``lsid`` field;
    the dispatch layer refreshes the registry's last-access timestamp
    on each such command.
    """
    import uuid

    from bson import Binary

    raw = uuid.uuid4().bytes
    if ctx.sessions is not None:
        ctx.sessions.register(raw)
    return {
        "id": {"id": Binary(raw, 4)},
        "timeoutMinutes": 30,
        "ok": 1.0,
    }


def _kill_all_sessions(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop every active session.

    Driver test suites (mongo-ruby-driver in particular) call this
    between tests to guarantee clean session state. Real mongod
    accepts an optional ``users`` array filter to limit the kill;
    SecantusDB ignores the filter and clears all sessions, since the
    sessions registry is not yet partitioned by principal.
    """
    if ctx.sessions is not None:
        ctx.sessions.clear()
    return {"ok": 1.0}


def _kill_all_sessions_by_pattern(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Pattern-filtered variant of ``killAllSessions``.

    Same effective semantics as ``killAllSessions`` — drop every
    session — because the registry isn't lsid-pattern-indexed.
    """
    if ctx.sessions is not None:
        ctx.sessions.clear()
    return {"ok": 1.0}


def _kill_sessions(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop the listed sessions (driver-callable variant)."""
    if ctx.sessions is not None:
        for entry in doc.get("killSessions") or []:
            lsid = _lsid_bytes_from_arg(entry)
            if lsid is not None:
                ctx.sessions.unregister(lsid)
    return {"ok": 1.0}


def _refresh_sessions(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Bump the idle TTL on listed sessions.

    Implicitly creates any session not already in the registry —
    matching mongod's behaviour of treating ``refreshSessions`` as
    "ensure this session is alive", not "fail if it isn't".
    """
    if ctx.sessions is not None:
        for entry in doc.get("refreshSessions") or []:
            lsid = _lsid_bytes_from_arg(entry)
            if lsid is not None:
                ctx.sessions.refresh(lsid)
    return {"ok": 1.0}


def _abort_transaction(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _commit_transaction(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    return {"ok": 1.0}


def _current_op(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    # mongod's currentOp returns a heterogeneous array of "in-progress"
    # records: per-connection ops + idle cursors. We don't track per-op
    # state (no in-flight queue), so each connection collapses to a
    # single "op" record carrying its last command + counters. Cursor
    # entries are emitted as ``type: "idleCursor"`` to match mongod.
    inprog: list[dict[str, Any]] = []
    if ctx.connections is not None:
        for conn in ctx.connections.snapshot():
            host_port = f"{conn.peer_addr[0]}:{conn.peer_addr[1]}"
            opened_iso = _dt.datetime.fromtimestamp(conn.opened_at, tz=_dt.timezone.utc).isoformat()
            inprog.append(
                {
                    "type": "op",
                    "desc": f"conn{conn.conn_id}",
                    "connectionId": bson.Int64(conn.conn_id),
                    "client": host_port,
                    "opid": bson.Int64(conn.conn_id),
                    "active": conn.last_command_name is not None,
                    "op": conn.last_command_name or "none",
                    "ns": "",
                    "currentOpTime": opened_iso,
                    "secs_running": 0,
                    "microsecs_running": 0,
                    "effectiveUsers": (
                        [{"user": conn.user.split("@", 1)[0], "db": conn.user.split("@", 1)[1]}]
                        if conn.user and "@" in conn.user
                        else []
                    ),
                }
            )
    if ctx.cursors is not None:
        for snap in ctx.cursors.snapshot():
            inprog.append(
                {
                    "type": "idleCursor",
                    "ns": snap["namespace"],
                    "cursorId": bson.Int64(snap["cursor_id"]),
                    "tailable": snap["tailable"],
                    "awaitData": snap["await_data"],
                    "originatingCommand": {},
                    "lsid": None,
                }
            )
    return {"inprog": inprog, "ok": 1.0}


def _fsync(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    # mongod's `fsync` defaults to flushing all data to disk. With
    # ``lock: true`` it also blocks subsequent writes until ``fsyncUnlock``
    # — a feature that requires real cluster-style coordination we don't
    # have. Reject the locked variant rather than silently skipping the
    # lock, which would mislead backup tools that rely on it.
    if doc.get("lock") is True:
        return {
            "ok": 0.0,
            "errmsg": "fsync with lock:true is not supported by SecantusDB",
            "code": 9,
            "codeName": "FailedToParse",
        }
    ctx.storage.checkpoint()
    return {"numFiles": 1, "ok": 1.0}


def _configure_fail_point(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Mongod-shaped ``configureFailPoint`` admin command.

    Driver test suites lean on this heavily — mongo-go-driver's
    cursor / database tests open by saying "fail ``getMore`` /
    ``killCursors`` / ``insert`` with code 100, then assert the driver
    surfaces it as a ``mongo.CommandError``." See
    ``secantus.failpoints`` for the exact subset implemented here.
    """
    if ctx.failpoints is None:
        return {"ok": 1.0}
    name = doc.get("configureFailPoint")
    if not isinstance(name, str):
        return {
            "ok": 0.0,
            "errmsg": "configureFailPoint requires a string name",
            "code": 9,
            "codeName": "FailedToParse",
        }
    mode = doc.get("mode")
    data = doc.get("data") or {}
    if not isinstance(data, dict):
        data = {}
    ctx.failpoints.configure(name, mode, data)
    return {"ok": 1.0}


def _secantus_admin_prune_oplog(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """SecantusDB extension: drop oplog rows past the retention window.

    Real mongod auto-prunes the oplog opportunistically; SecantusDB
    does the same on every emit. Surfacing this as a wire command lets
    operators force an immediate sweep from the admin UI without
    waiting for the next write.
    """
    pruned = ctx.storage.prune_oplog()
    return {"pruned": int(pruned), "ok": 1.0}


def _secantus_admin_prune_ttl(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """SecantusDB extension: run TTL pruning against every collection.

    The background sweeper handles this on a 60-second cadence; the
    wire command lets callers (the admin UI, tests) drive an immediate
    pass when they need deterministic timing.
    """
    pruned = ctx.storage.prune_ttl_all_collections()
    return {"pruned": int(pruned), "ok": 1.0}


def _profile(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Get / set per-database profiling level (mongod ``profile`` shape).

    ``{profile: -1}`` reads the current state. ``{profile: 0|1|2,
    slowms: N, sampleRate: F}`` updates it. Returns the previous values
    under ``was`` / ``slowms`` / ``sampleRate`` so monitoring scripts
    can confirm the change.
    """
    db = ctx.db_name or "admin"
    arg = doc.get("profile")
    prev = ctx.storage.get_profile(db)

    if arg == -1:
        return {
            "was": prev["level"],
            "slowms": prev["slowms"],
            "sampleRate": prev["sampleRate"],
            "ok": 1.0,
        }
    if not isinstance(arg, int) or arg not in (0, 1, 2):
        return {
            "ok": 0.0,
            "errmsg": "profile must be -1, 0, 1, or 2",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    new_slowms = doc.get("slowms", prev["slowms"])
    new_sample_rate = doc.get("sampleRate", prev["sampleRate"])
    try:
        ctx.storage.set_profile(
            db,
            level=int(arg),
            slowms=int(new_slowms),
            sample_rate=float(new_sample_rate),
        )
    except ValueError as exc:
        return {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 14,
            "codeName": "TypeMismatch",
        }
    return {
        "was": prev["level"],
        "slowms": prev["slowms"],
        "sampleRate": prev["sampleRate"],
        "ok": 1.0,
    }


def _get_log(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    # mongod returns the in-memory log as a list of pre-formatted strings.
    # Format mirrors mongod's: "<ts> <level> <component> <msg>" — close
    # enough that tooling that grep-parses the response keeps working.
    if ctx.logs is None:
        return {"totalLinesWritten": 0, "log": [], "ok": 1.0}
    entries = ctx.logs.tail()
    formatted: list[str] = []
    for e in entries:
        ts = _dt.datetime.fromtimestamp(e.ts, tz=_dt.timezone.utc).isoformat()
        formatted.append(f"{ts} {e.level} {e.component} {e.msg}")
    return {
        "totalLinesWritten": len(entries),
        "log": formatted,
        "ok": 1.0,
    }


def _whatsmyuri(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Return the requesting client's connection peer (``host:port``).

    Drivers and ``mongosh`` use this to disambiguate which client they
    are. Serving the actual peer (looked up from the connection
    registry) instead of a hardcoded placeholder makes the helpers
    work end-to-end.
    """
    peer_str = "unknown"
    if ctx.connections is not None:
        info = ctx.connections.get(ctx.connection_id)
        if info is not None:
            peer_str = f"{info.peer_addr[0]}:{info.peer_addr[1]}"
    return {"you": peer_str, "ok": 1.0}


def _hostinfo(_doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    """Real ``hostInfo``: hostname, OS, CPU arch, core count, RAM size.

    Hostname comes from ``socket.gethostname()``; CPU architecture
    from ``platform.machine()``; OS type / name / version from
    ``platform.system()`` / ``platform.release()``. Memory size in MB
    is read via ``sysconf(SC_PHYS_PAGES) * sysconf(SC_PAGE_SIZE)`` on
    POSIX (Linux / macOS); falls back to 0 on platforms where sysconf
    doesn't expose those keys (Windows, some BSDs).
    """
    import platform
    import socket

    mem_mb = 0
    try:
        page_size = os.sysconf("SC_PAGE_SIZE")
        phys_pages = os.sysconf("SC_PHYS_PAGES")
        if page_size > 0 and phys_pages > 0:
            mem_mb = (page_size * phys_pages) // (1024 * 1024)
    except (ValueError, OSError, AttributeError):
        # ``os.sysconf`` doesn't exist on Windows (AttributeError) and
        # the specific keys may be missing on some Unix variants.
        pass

    os_type = platform.system() or "Unknown"  # "Darwin", "Linux", "Windows"
    os_release = platform.release() or ""
    os_version = platform.version() or os_release

    return {
        "system": {
            "currentTime": _dt.datetime.now(_dt.timezone.utc),
            "hostname": socket.gethostname() or "localhost",
            "cpuAddrSize": 64,
            "memSizeMB": mem_mb,
            "numCores": os.cpu_count() or 1,
            "cpuArch": platform.machine() or "unknown",
            "numaEnabled": False,
        },
        "os": {
            "type": os_type,
            "name": os_type,
            "version": os_release,
        },
        "extra": {"versionString": os_version},
        "ok": 1.0,
    }


def _server_status(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Real metrics from :class:`secantus.metrics.Metrics` if the server
    constructed one (production path); falls back to zeroed values for
    embedded callers that didn't thread a metrics instance through (e.g.
    ad-hoc test harnesses that use ``CommandContext`` directly)."""
    base: dict[str, Any] = {
        "host": "secantus",
        "version": SERVER_VERSION,
        "process": "secantus",
        "pid": os.getpid(),
        "localTime": _dt.datetime.now(_dt.timezone.utc),
        "ok": 1.0,
    }
    if ctx.metrics is not None:
        base.update(ctx.metrics.snapshot())
    else:
        base.update(
            {
                "uptime": 0,
                "uptimeMillis": 0,
                "uptimeEstimate": 0,
                "connections": {
                    "current": 0,
                    "available": 0,
                    "totalCreated": 0,
                },
                "opcounters": {
                    "insert": 0,
                    "query": 0,
                    "update": 0,
                    "delete": 0,
                    "getmore": 0,
                    "command": 0,
                },
                "network": {"numRequests": 0, "bytesIn": 0, "bytesOut": 0},
            }
        )
    return base


def _get_parameter(doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    """Return server parameters.

    Real `mongod` exposes hundreds of tunables. SecantusDB returns a
    minimal set so admin tooling that probes one or two well-known
    parameters (e.g. ``featureCompatibilityVersion`` for version
    gating, ``enableTestCommands`` to detect a test mongod) gets a
    sensible answer instead of a "no such command" error.

    Caller forms accepted:
      * ``{getParameter: 1, <name>: 1, ...}`` — return only the named
        parameters.
      * ``{getParameter: "*"}`` — return all known parameters.
      * ``{getParameter: 1}`` — same as ``"*"`` (legacy form).
    """
    params: dict[str, Any] = {
        "featureCompatibilityVersion": {"version": "7.0"},
        "enableTestCommands": False,
        "logLevel": 0,
        "quiet": False,
    }
    arg = doc.get("getParameter")
    if isinstance(arg, str) and arg == "*":
        return {**params, "ok": 1.0}
    if isinstance(arg, dict):
        # ``{getParameter: {showDetails: true}, <name>: 1, ...}`` form.
        keys = [k for k in doc if k != "getParameter" and not k.startswith("$")]
        return {**{k: params[k] for k in keys if k in params}, "ok": 1.0}
    # Default: name list passed alongside ``getParameter: 1``.
    keys = [k for k in doc if k != "getParameter" and not k.startswith("$")]
    if not keys:
        return {**params, "ok": 1.0}
    return {**{k: params[k] for k in keys if k in params}, "ok": 1.0}


def _get_cmd_line_opts(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Return a parsed `mongod`-shaped command line.

    pymongo's test bootstrap (and other admin tooling) reads this to
    detect whether the server was started with `--auth`: it inspects
    ``parsed.security.authorization`` and treats ``"enabled"`` as the
    auth-on signal.
    """
    parsed: dict[str, Any] = {"net": {}, "storage": {}}
    if ctx.require_auth:
        parsed["security"] = {"authorization": "enabled"}
    argv = ["secantus"]
    if ctx.require_auth:
        argv.append("--auth")
    return {"argv": argv, "parsed": parsed, "ok": 1.0}


def _connection_status(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    principals = ctx.connection_auth.authenticated_principals if ctx.connection_auth else []
    roles = list(ctx.connection_auth.effective_roles) if ctx.connection_auth else []
    return {
        "authInfo": {
            "authenticatedUsers": [{"user": user, "db": db} for db, user in principals],
            "authenticatedUserRoles": roles,
            # Per-action expansion is heavy and clients rarely check;
            # leave empty for now (mongod also surfaces this with a
            # `showPrivileges` toggle which we don't honour).
            "authenticatedUserPrivileges": [],
        },
        "ok": 1.0,
    }


def _db_stats(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    colls = ctx.storage.list_collections(ctx.db_name)
    objects = sum(ctx.storage.count_matching(ctx.db_name, c, None) for c in colls)
    data_size = sum(ctx.storage.collection_data_size(ctx.db_name, c) for c in colls)
    index_size = sum(sum(ctx.storage.index_sizes(ctx.db_name, c).values()) for c in colls)
    avg_obj_size = (data_size / objects) if objects else 0
    return {
        "db": ctx.db_name,
        "collections": len(colls),
        "objects": objects,
        "avgObjSize": avg_obj_size,
        "dataSize": data_size,
        "storageSize": data_size,
        "indexes": sum(len(ctx.storage.list_indexes(ctx.db_name, c)) for c in colls),
        "indexSize": index_size,
        "totalSize": data_size + index_size,
        "scaleFactor": 1,
        "ok": 1.0,
    }


def _coll_stats(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc.get("collStats")
    if not isinstance(coll, str):
        return {
            "ok": 0.0,
            "errmsg": "collStats requires a collection name",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    count = ctx.storage.count_matching(ctx.db_name, coll, None)
    data_size = ctx.storage.collection_data_size(ctx.db_name, coll)
    index_sizes = ctx.storage.index_sizes(ctx.db_name, coll)
    total_index_size = sum(index_sizes.values())
    avg_obj_size = (data_size / count) if count else 0
    return {
        "ns": f"{ctx.db_name}.{coll}",
        "count": count,
        "size": data_size,
        "avgObjSize": avg_obj_size,
        "storageSize": data_size,
        "totalIndexSize": total_index_size,
        "indexSizes": index_sizes,
        "nindexes": len(ctx.storage.list_indexes(ctx.db_name, coll)),
        "scaleFactor": 1,
        "capped": False,
        "ok": 1.0,
    }


def _explain(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    inner = doc.get("explain") or {}
    coll = ""
    filter_: dict[str, Any] = {}
    sort = None
    hint = None
    if isinstance(inner, dict):
        cmd_name = next(iter(inner), "")
        coll_value = inner.get(cmd_name)
        coll = coll_value if isinstance(coll_value, str) else ""
        filter_ = inner.get("filter") or inner.get("query") or {}
        sort = inner.get("sort")
        hint = inner.get("hint")
    # ``verbosity`` controls which sections of the explain reply the
    # client gets. Mongod's three levels:
    #   queryPlanner       — winningPlan only, no execution
    #   executionStats     — winningPlan + actual execution counts
    #   allPlansExecution  — adds rejected-plan stats (we treat as
    #                        executionStats since we have no
    #                        rejected plans).
    # mongo-java-driver's AbstractExplainTest asserts that QUERY_PLANNER
    # verbosity *omits* the executionStats key, so we have to honour
    # it rather than always returning both sections.
    verbosity = doc.get("verbosity", "executionStats")
    # Reject unknown verbosity strings. Mongod's three valid values
    # are ``queryPlanner``, ``executionStats``, ``allPlansExecution``;
    # anything else surfaces as ``BadValue`` (code 2). Driver tests
    # like mongo-node-driver's ``explain.test.ts`` parametrize over
    # invalid verbosity (``'invalid'``) and assert the response is a
    # ``MongoServerError`` — they fail silently if we accept the bad
    # value and return a normal explain doc.
    if not isinstance(verbosity, str) or verbosity not in (
        "queryPlanner", "executionStats", "allPlansExecution"
    ):
        return {
            "ok": 0.0,
            "errmsg": (
                f"verbosity {verbosity!r} not recognized; expected one of "
                "['queryPlanner', 'executionStats', 'allPlansExecution']"
            ),
            "code": 2,
            "codeName": "BadValue",
        }
    namespace = _ns(ctx.db_name, coll) if coll else f"{ctx.db_name}.$cmd"
    if coll:
        plan = ctx.storage.explain_plan(ctx.db_name, coll, filter_, sort=sort, hint=hint)
    else:
        plan = {"kind": "COLLSCAN"}
    if plan["kind"] == "IXSCAN":
        winning_plan = {
            "stage": "FETCH",
            "filter": filter_,
            "inputStage": {
                "stage": "IXSCAN",
                "indexName": plan["index_name"],
                "keyPattern": plan["key_pattern"],
                "direction": plan["direction"],
            },
        }
        execution_stage = {
            "stage": "FETCH",
            "nReturned": 0,
            "inputStage": {"stage": "IXSCAN", "nReturned": 0},
        }
    else:
        winning_plan = {"stage": "COLLSCAN", "filter": filter_}
        execution_stage = {"stage": "COLLSCAN", "nReturned": 0}
    query_planner = {
        "namespace": namespace,
        "indexFilterSet": False,
        "parsedQuery": filter_,
        "winningPlan": winning_plan,
        "rejectedPlans": [],
    }
    server_info = {
        "host": "secantus",
        "port": 0,
        "version": SERVER_VERSION,
        "gitVersion": "0" * 40,
    }
    # Aggregate-explain has a different shape from find-explain:
    # mongod returns ``stages: [{$cursor: {queryPlanner, ...}}, ...]``
    # so drivers can iterate the pipeline. Each non-cursor stage
    # gets its own entry. mongo-node-driver's
    # ``aggregation.test.ts#should correctly return a cursor and
    # call explain`` asserts ``JSON.stringify(result)`` includes
    # ``$cursor`` — without the stages wrapper that substring never
    # appears.
    if isinstance(inner, dict) and "aggregate" in inner:
        pipeline = inner.get("pipeline") or []
        cursor_stage: dict[str, Any] = {
            "$cursor": {
                "queryPlanner": query_planner,
            }
        }
        if verbosity != "queryPlanner":
            cursor_stage["$cursor"]["executionStats"] = {
                "executionSuccess": True,
                "nReturned": 0,
                "executionTimeMillis": 0,
                "totalKeysExamined": 0,
                "totalDocsExamined": 0,
                "executionStages": execution_stage,
            }
        stages: list[dict[str, Any]] = [cursor_stage]
        for stage_doc in pipeline:
            if isinstance(stage_doc, Mapping):
                stages.append(dict(stage_doc))
        return {
            "stages": stages,
            "explainVersion": "1",
            "command": inner,
            "serverInfo": server_info,
            "ok": 1.0,
        }
    reply: dict[str, Any] = {
        "queryPlanner": query_planner,
        "command": inner if isinstance(inner, dict) else {},
        "serverInfo": server_info,
        "ok": 1.0,
    }
    if verbosity != "queryPlanner":
        reply["executionStats"] = {
            "executionSuccess": True,
            "nReturned": 0,
            "executionTimeMillis": 0,
            "totalKeysExamined": 0,
            "totalDocsExamined": 0,
            "executionStages": execution_stage,
        }
    return reply


def _ns(db: str, coll: str) -> str:
    return f"{db}.{coll}"


def _insert(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["insert"]
    documents = doc.get("documents", [])
    if not isinstance(documents, list) or len(documents) == 0:
        # mongod rejects an empty `documents` array with code 4
        # (InvalidLength). Drivers (mongo-go-driver, mongo-java-driver)
        # have command-error tests that check for this specific code
        # / codeName combo, so it's load-bearing for the gauge.
        return {
            "ok": 0.0,
            "errmsg": "Write batch sizes must be between 1 and 100000. Got 0 operations.",
            "code": 4,
            "codeName": "InvalidLength",
        }
    ordered = doc.get("ordered", True)
    inserted, errors = ctx.storage.insert(ctx.db_name, coll, documents, ordered=ordered)
    reply: dict[str, Any] = {"n": inserted, "ok": 1.0}
    if errors:
        reply["writeErrors"] = errors
    return reply


def _find(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.query import QueryError
    from secantus.storage import BadHint

    coll = doc["find"]
    filter_ = doc.get("filter") or {}
    skip = int(doc.get("skip", 0) or 0)
    limit = int(doc.get("limit", 0) or 0)
    sort = doc.get("sort") or None
    projection = doc.get("projection") or None
    hint = doc.get("hint")
    # ``let`` declares user-vars visible to ``$expr`` clauses in the
    # filter (MongoDB 5.0+).
    let = _resolve_let_vars(doc.get("let"))
    # ``batchSize`` is genuinely tri-state: absent (use default),
    # 0 ("open the cursor but send no docs in firstBatch"), or
    # explicit positive. The 0 case is load-bearing for drivers
    # that want to set a streaming batch size via ``getMore`` — the
    # mongo-go-driver test ``TestCursor/set_batchSize`` opens a
    # cursor with batchSize 0, then calls SetBatchSize on the
    # batch cursor and asserts the next getMore carries that size.
    raw_batch_size = doc.get("batchSize")
    batch_size = DEFAULT_BATCH_SIZE if raw_batch_size is None else int(raw_batch_size)
    single_batch = bool(doc.get("singleBatch", False))
    # Validate filter syntax up-front. matches() raises QueryError for
    # unknown top-level operators; running it against an empty doc is
    # cheap and triggers the same validation paths the real query would
    # hit. Without this, an empty collection with `{$foo: 1}` returns
    # an empty cursor instead of an error — real mongod (and the
    # mongo-go-driver test ``find/invalid_identifier_error``) rejects
    # at the find command level.
    try:
        matches({}, filter_, vars=let)
    except QueryError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    except ExpressionError:
        # $expr against empty doc with unresolved field refs is fine —
        # the validation pass is only meant to catch parse-level errors.
        # Real evaluation happens per-doc inside find_matching with the
        # actual document and threaded ``let`` vars.
        pass
    try:
        docs = ctx.storage.find_matching(
            ctx.db_name,
            coll,
            filter_,
            skip=skip,
            limit=limit,
            sort=sort,
            projection=projection,
            hint=hint,
            let=let,
        )
    except BadHint as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    except QueryError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    ns = _ns(ctx.db_name, coll)
    # Tailable cursor on a (capped) collection. Real mongod rejects
    # ``tailable: true`` on a non-capped collection with code 2
    # (BadValue). The driver-spec ``find`` command uses
    # ``tailable: true`` + ``awaitData: true`` for the legacy
    # newest-doc-poll workload (replicated by mongo-go-driver's
    # ``TestCursor_TryNext/one_getMore_sent`` against a capped
    # ``logs`` collection). Note the change-stream tailable path
    # is separate — it goes through the ``aggregate``/``$changeStream``
    # pipeline, not ``find``.
    tailable = bool(doc.get("tailable", False))
    if tailable:
        if not ctx.storage.collection_is_capped(ctx.db_name, coll):
            return {
                "ok": 0.0,
                "errmsg": (
                    f"error processing query: tailable cursor requested on non capped "
                    f"collection {ns}"
                ),
                "code": 2,
                "codeName": "BadValue",
            }
        await_data = bool(doc.get("awaitData", False))
        return _find_tailable(coll, docs, batch_size, ns, await_data, ctx)
    if single_batch:
        first_batch, cursor_id = docs, 0
    else:
        first_batch, cursor_id = _split_into_cursor(docs, batch_size, ns, ctx.cursors)
    return {
        # Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
        "cursor": {"firstBatch": first_batch, "id": bson.Int64(cursor_id), "ns": ns},
        "ok": 1.0,
    }


def _find_tailable(
    coll: str,
    initial_docs: list[dict[str, Any]],
    batch_size: int,
    ns: str,
    await_data: bool,
    ctx: CommandContext,
) -> dict[str, Any]:
    """Build a tailable cursor on a capped collection.

    The producer scans the doc table for rows with ``id_key`` strictly
    greater than the last one we've returned. ``id_key`` is the
    byte-sortable ``_id`` encoding (see ``secantus.sortkey``); for
    monotonic ``ObjectId``-style ``_id`` values that order matches
    insertion order, which is exactly what tailable consumers expect.
    Capped collections eviction-prune oldest rows in the same order,
    so the producer naturally tracks the trailing edge.
    """
    db_name = ctx.db_name
    storage = ctx.storage
    # Track our current-watermark id_key on a mutable container so the
    # producer closure can update it. Walk the collection once now to
    # find the highest current id_key — that becomes the starting
    # checkpoint after we hand back ``firstBatch``. Subsequent
    # ``getMore`` polls re-scan from that checkpoint forward.
    rows = storage.scan_docs_after_id_key(db_name, coll, after=None)
    state = {"after_id_key": rows[-1][0] if rows else None}

    def producer() -> list[dict[str, Any]]:
        new_rows = storage.scan_docs_after_id_key(db_name, coll, after=state["after_id_key"])
        if not new_rows:
            return []
        state["after_id_key"] = new_rows[-1][0]
        return [doc for _id_k, doc in new_rows]

    first_batch = initial_docs[:batch_size]
    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=await_data,
    )
    return {
        "cursor": {
            "firstBatch": first_batch,
            "id": bson.Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    }


def _update(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import GeoExtractError, IndexConflict
    from secantus.update import _PIPELINE_UPDATE_STAGES

    coll = doc["update"]
    updates = doc.get("updates", [])
    ordered = bool(doc.get("ordered", True))
    # ``let`` — see ``_delete`` for the wire-shape rationale.
    let = _resolve_let_vars(doc.get("let"))
    n = 0
    n_modified = 0
    upserted: list[dict[str, Any]] = []
    write_errors: list[dict[str, Any]] = []
    for index, spec in enumerate(updates):
        # Pre-validate the pipeline-update shape upfront so a no-match
        # filter still surfaces parse errors to the client. Real
        # mongod parses the pipeline before scanning the collection
        # and returns a **command-level** error (``ok: 0``) for an
        # unknown stage — not a per-doc writeError. The
        # ``CommandLoggingTest#Failed bulk write command log
        # message`` driver test asserts the driver fires
        # ``Command failed`` (``ok: 0``) for ``$invalidOperator`` in
        # the pipeline, so we mirror mongod's strict path here even
        # though invalid query operators (``$unsupported``) DO go to
        # writeErrors below.
        u = spec.get("u")
        if isinstance(u, list):
            for stage in u:
                if not isinstance(stage, Mapping) or len(stage) != 1:
                    return {
                        "ok": 0.0,
                        "errmsg": "each pipeline stage must be a single-key document",
                        "code": 9,
                        "codeName": "FailedToParse",
                    }
                (name,) = stage.keys()
                if name not in _PIPELINE_UPDATE_STAGES:
                    return {
                        "ok": 0.0,
                        "errmsg": (f"stage {name} not allowed in pipeline updates"),
                        "code": 168,
                        "codeName": "InvalidPipelineOperator",
                    }
        try:
            result = ctx.storage.update_matching(
                ctx.db_name,
                coll,
                spec.get("q", {}),
                spec.get("u", {}),
                multi=bool(spec.get("multi", False)),
                upsert=bool(spec.get("upsert", False)),
                array_filters=spec.get("arrayFilters"),
                let=let,
            )
        except IndexConflict as exc:
            err: dict[str, Any] = {"index": index, "code": 11000, "errmsg": str(exc)}
            if exc.key_pattern is not None:
                err["keyPattern"] = exc.key_pattern
            if exc.key_value is not None:
                err["keyValue"] = exc.key_value
            write_errors.append(err)
            if ordered:
                break
            continue
        except GeoExtractError as exc:
            # Mongod's documented code for "Can't extract geo keys from
            # object" — surfaces to the driver as a write error on the
            # specific operation index without aborting unordered batches.
            write_errors.append({"index": index, "code": 16572, "errmsg": str(exc)})
            if ordered:
                break
            continue
        except QueryError as exc:
            # Bad filter syntax (unknown operator, malformed regex, etc.)
            # is a per-update writeError per mongod's wire shape — the
            # ``update`` command itself succeeds with ``ok: 1`` and
            # ``writeErrors: [...]`` populated, NOT ``ok: 0``. Drivers
            # depend on this: mongo-java-driver's
            # ``CommandMonitoringTest#updateOne with write errors``
            # asserts a ``commandSucceededEvent`` for the call, not a
            # ``commandFailedEvent``.
            write_errors.append({"index": index, "code": 2, "errmsg": str(exc)})
            if ordered:
                break
            continue
        except UpdateError as exc:
            # Same shape for malformed update operators.
            write_errors.append({"index": index, "code": 9, "errmsg": str(exc)})
            if ordered:
                break
            continue
        n += result["matched"]
        n_modified += result["modified"]
        if result["upserted_id"] is not None:
            upserted.append({"index": index, "_id": result["upserted_id"]})
            n += 1
    reply: dict[str, Any] = {"n": n, "nModified": n_modified, "ok": 1.0}
    if upserted:
        reply["upserted"] = upserted
    if write_errors:
        reply["writeErrors"] = write_errors
    return reply


def _delete(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["delete"]
    deletes = doc.get("deletes", [])
    ordered = bool(doc.get("ordered", True))
    # ``let`` declares user-variables visible to ``$expr`` clauses in the
    # filter (MongoDB 5.0+). Threaded through to ``matches()`` via the
    # storage layer; without it, ``{$expr: {$eq: ['$_id', '$$id']}}``
    # against ``let: {id: 1}`` raises "system variable $$id is not
    # defined" and the test framework asserts failure.
    let = _resolve_let_vars(doc.get("let"))
    n = 0
    write_errors: list[dict[str, Any]] = []
    for index, spec in enumerate(deletes):
        try:
            n += ctx.storage.delete_matching(
                ctx.db_name, coll, spec.get("q", {}),
                limit=int(spec.get("limit", 0)),
                let=let,
            )
        except QueryError as exc:
            # Same per-delete writeError shape as ``_update`` — the
            # ``delete`` command must succeed with ``ok: 1`` and
            # ``writeErrors: [...]`` rather than failing the whole
            # batch on one bad filter.
            write_errors.append({"index": index, "code": 2, "errmsg": str(exc)})
            if ordered:
                break
            continue
    reply: dict[str, Any] = {"n": n, "ok": 1.0}
    if write_errors:
        reply["writeErrors"] = write_errors
    return reply


def _count(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["count"]
    filter_ = doc.get("query") or {}
    n = ctx.storage.count_matching(ctx.db_name, coll, filter_)
    # Mongod's ``count`` honours ``limit`` and ``skip`` — the cursor-side
    # ``cursor.count()`` API in the Node / legacy drivers translates to a
    # ``count`` command with these fields populated from the cursor's
    # configured limits, and ``count`` is expected to return
    # ``min(max(matches - skip, 0), limit)``. mongo-node-driver's
    # ``crud_api`` integration test asserts this directly: a 4-doc
    # collection, ``.limit(2)``, then ``cursor.count()`` should yield 2.
    skip = int(doc.get("skip") or 0)
    if skip > 0:
        n = max(n - skip, 0)
    limit = int(doc.get("limit") or 0)
    if limit > 0:
        n = min(n, limit)
    return {"n": n, "ok": 1.0}


def _distinct(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.paths import get_path

    coll = doc["distinct"]
    key = doc.get("key", "")
    filter_ = doc.get("query") or {}
    if not isinstance(key, str):
        return {
            "ok": 0.0,
            "errmsg": "distinct key must be a string",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    matched = ctx.storage.find_matching(ctx.db_name, coll, filter_)
    seen: list[Any] = []
    for d in matched:
        value = get_path(d, key)
        if isinstance(value, list):
            for elem in value:
                if elem not in seen:
                    seen.append(elem)
        elif (value is not None or _key_present(d, key)) and value not in seen:
            seen.append(value)
    return {"values": seen, "ok": 1.0}


def _key_present(doc: dict[str, Any], path: str) -> bool:
    cur: Any = doc
    for part in path.split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        else:
            return False
    return True


def _find_and_modify(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import GeoExtractError, IndexConflict

    coll = doc["findAndModify"]
    query = doc.get("query") or {}
    sort = doc.get("sort") or None
    fields = doc.get("fields") or None
    return_new = bool(doc.get("new", False))
    upsert = bool(doc.get("upsert", False))
    is_remove = bool(doc.get("remove", False))
    update = doc.get("update")
    # ``let`` user-vars threaded into the filter / update predicate.
    let = _resolve_let_vars(doc.get("let"))
    # arrayFilters carries ``[{<id>: <subfilter>}, ...]`` entries
    # the update's ``$[<id>]`` positional refs resolve against. Used
    # by mongo-java-driver's ``findOneAndUpdate-arrayFilters``
    # tests — without plumbing through, the update raises
    # ``UpdateError: arrayFilters has no entry for identifier 'i'``
    # before reaching the actual array element.
    array_filters = doc.get("arrayFilters")

    if is_remove and update is not None:
        return {
            "ok": 0.0,
            "errmsg": "Cannot specify both update and remove=true",
            "code": 9,
            "codeName": "FailedToParse",
        }
    if not is_remove and update is None:
        return {
            "ok": 0.0,
            "errmsg": "Either an update or remove=true must be specified",
            "code": 9,
            "codeName": "FailedToParse",
        }

    candidates = ctx.storage.find_matching(
        ctx.db_name, coll, query, sort=sort, limit=1, let=let
    )

    if not candidates:
        if upsert and not is_remove:
            try:
                result = ctx.storage.update_matching(
                    ctx.db_name, coll, query, update,
                    multi=False, upsert=True, let=let,
                    array_filters=array_filters,
                )
            except IndexConflict as exc:
                reply: dict[str, Any] = {
                    "ok": 0.0,
                    "errmsg": str(exc),
                    "code": 11000,
                    "codeName": "DuplicateKey",
                }
                if exc.key_pattern is not None:
                    reply["keyPattern"] = exc.key_pattern
                if exc.key_value is not None:
                    reply["keyValue"] = exc.key_value
                return reply
            except GeoExtractError as exc:
                return {
                    "ok": 0.0,
                    "errmsg": str(exc),
                    "code": 16572,
                    "codeName": "Location16572",
                }
            upserted_id = result["upserted_id"]
            value: Any = None
            if return_new and upserted_id is not None:
                new_docs = ctx.storage.find_matching(ctx.db_name, coll, {"_id": upserted_id})
                if new_docs:
                    value = new_docs[0]
                    if fields:
                        value = apply_projection(value, fields)
            return {
                "lastErrorObject": {
                    "n": 1,
                    "updatedExisting": False,
                    "upserted": upserted_id,
                },
                "value": value,
                "ok": 1.0,
            }
        return {
            "lastErrorObject": {"n": 0, "updatedExisting": False},
            "value": None,
            "ok": 1.0,
        }

    matched_doc = candidates[0]
    matched_id = matched_doc["_id"]

    if is_remove:
        ctx.storage.delete_matching(ctx.db_name, coll, {"_id": matched_id}, limit=1)
        value = matched_doc
        if fields:
            value = apply_projection(value, fields)
        return {
            "lastErrorObject": {"n": 1, "updatedExisting": True},
            "value": value,
            "ok": 1.0,
        }

    try:
        ctx.storage.update_matching(
            ctx.db_name, coll, {"_id": matched_id}, update,
            multi=False, array_filters=array_filters,
        )
    except IndexConflict as exc:
        reply2: dict[str, Any] = {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 11000,
            "codeName": "DuplicateKey",
        }
        if exc.key_pattern is not None:
            reply2["keyPattern"] = exc.key_pattern
        if exc.key_value is not None:
            reply2["keyValue"] = exc.key_value
        return reply2
    except GeoExtractError as exc:
        return {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 16572,
            "codeName": "Location16572",
        }

    if return_new:
        new_docs = ctx.storage.find_matching(ctx.db_name, coll, {"_id": matched_id})
        value = new_docs[0] if new_docs else None
    else:
        value = matched_doc

    if fields and value is not None:
        value = apply_projection(value, fields)

    return {
        "lastErrorObject": {"n": 1, "updatedExisting": True},
        "value": value,
        "ok": 1.0,
    }


def _drop(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["drop"]
    existed = ctx.storage.drop_collection(ctx.db_name, coll)
    if not existed:
        return {"ok": 0.0, "errmsg": "ns not found", "code": 26, "codeName": "NamespaceNotFound"}
    return {"ns": _ns(ctx.db_name, coll), "nIndexesWas": 1, "ok": 1.0}


def _drop_database(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    ctx.storage.drop_database(ctx.db_name)
    return {"dropped": ctx.db_name, "ok": 1.0}


def _rename_collection(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    src_ns = doc.get("renameCollection")
    dst_ns = doc.get("to")
    drop_target = bool(doc.get("dropTarget", False))
    if not isinstance(src_ns, str) or not isinstance(dst_ns, str):
        return {
            "ok": 0.0,
            "errmsg": "renameCollection requires two string namespaces",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    src_db, _, src_coll = src_ns.partition(".")
    dst_db, _, dst_coll = dst_ns.partition(".")
    if not src_coll or not dst_coll:
        return {
            "ok": 0.0,
            "errmsg": "renameCollection namespaces must be of the form db.coll",
            "code": 73,
            "codeName": "InvalidNamespace",
        }
    ok, err = ctx.storage.rename_collection(
        src_db, src_coll, dst_db, dst_coll, drop_target=drop_target
    )
    if not ok:
        code = 26 if err and "does not exist" in err else 48
        code_name = "NamespaceNotFound" if code == 26 else "NamespaceExists"
        return {"ok": 0.0, "errmsg": err, "code": code, "codeName": code_name}
    return {"ok": 1.0}


def _create(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["create"]
    capped = bool(doc.get("capped", False))
    if capped:
        size = doc.get("size")
        if not isinstance(size, (int, float)) or isinstance(size, bool) or size <= 0:
            return {
                "ok": 0.0,
                "errmsg": (
                    "the 'size' field is required when 'capped' is true "
                    "and must be a positive number"
                ),
                "code": 72,
                "codeName": "InvalidOptions",
            }
        max_docs = doc.get("max")
        if max_docs is not None and (
            not isinstance(max_docs, (int, float)) or isinstance(max_docs, bool) or max_docs <= 0
        ):
            return {
                "ok": 0.0,
                "errmsg": "the 'max' field must be a positive integer when set",
                "code": 72,
                "codeName": "InvalidOptions",
            }
    ctx.storage.create_collection(ctx.db_name, coll)
    if capped:
        opts: dict[str, Any] = {"capped": True, "size": int(doc["size"])}
        if doc.get("max") is not None:
            opts["max"] = int(doc["max"])
        ctx.storage.set_collection_options(ctx.db_name, coll, **opts)
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        ctx.storage.set_collection_options(
            ctx.db_name, coll, changeStreamPreAndPostImages=dict(pre_post)
        )
    return {"ok": 1.0}


def _coll_mod(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["collMod"]
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        ctx.storage.set_collection_options(
            ctx.db_name, coll, changeStreamPreAndPostImages=dict(pre_post)
        )
    return {"ok": 1.0}


def _list_collections(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """List collections honouring ``filter`` and ``nameOnly`` per mongod.

    ``filter`` is a regular query predicate evaluated against each
    collection descriptor (``{name, type, options, info, idIndex}``).
    ``nameOnly: true`` strips down each entry to ``{name, type}`` —
    drivers use this when they only care about names. Both options
    are normal mongod arguments; failing to honour them causes the
    Go driver's ``ListCollectionNames`` filter test to see
    unexpected matches.
    """
    names = ctx.storage.list_collections(ctx.db_name)
    name_only = bool(doc.get("nameOnly", False))
    filter_doc = doc.get("filter")

    batch: list[dict[str, Any]] = []
    for n in names:
        raw = ctx.storage.get_collection_options(ctx.db_name, n)
        opts: dict[str, Any] = {}
        if raw.get("capped"):
            opts["capped"] = True
            if "size" in raw:
                opts["size"] = raw["size"]
            if "max" in raw:
                opts["max"] = raw["max"]
        batch.append(
            {"name": n, "type": "collection", "options": opts, "info": {"readOnly": False}}
        )

    if isinstance(filter_doc, dict) and filter_doc:
        batch = [d for d in batch if matches(d, filter_doc)]

    if name_only:
        batch = [{"name": d["name"], "type": d["type"]} for d in batch]

    # Honour ``batchSize`` so drivers that pass a small batch get a
    # real cursor + getMore round trip — the mongo-go-driver
    # ``listCollections/getMore_commands_are_monitored`` test asserts
    # that at least one getMore is emitted when batchSize=2 on a
    # database with three collections. Without pagination, listCollections
    # always returns id=0 and the test fails.
    cursor_spec = doc.get("cursor")
    if isinstance(cursor_spec, dict) and "batchSize" in cursor_spec:
        raw_batch_size: Any = cursor_spec.get("batchSize")
    else:
        raw_batch_size = doc.get("batchSize")
    batch_size = DEFAULT_BATCH_SIZE if raw_batch_size is None else int(raw_batch_size)
    ns = f"{ctx.db_name}.$cmd.listCollections"
    first_batch, cursor_id = _split_into_cursor(batch, batch_size, ns, ctx.cursors)

    return {
        "cursor": {
            "firstBatch": first_batch,
            "id": bson.Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    }


def _list_databases(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """List databases honouring ``filter``, ``nameOnly``, ``authorizedDatabases``.

    The ``filter`` document is a regular query predicate evaluated
    against each per-database descriptor (``{name, sizeOnDisk,
    empty}``). ``nameOnly: true`` strips the size/empty fields from
    each entry — drivers use this when they only care about names.
    ``authorizedDatabases`` is accepted for wire compatibility but
    has no effect (we don't gate listing by per-db privileges in the
    initial RBAC slice).
    """
    names = ctx.storage.list_databases()
    name_only = bool(doc.get("nameOnly", False))
    filter_doc = doc.get("filter")

    descriptors: list[dict[str, Any]] = [
        {"name": n, "sizeOnDisk": 0, "empty": False} for n in names
    ]

    if isinstance(filter_doc, dict) and filter_doc:
        descriptors = [d for d in descriptors if matches(d, filter_doc)]

    if name_only:
        descriptors = [{"name": d["name"]} for d in descriptors]

    return {
        "databases": descriptors,
        "totalSize": 0,
        "ok": 1.0,
    }


def _list_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["listIndexes"]
    indexes = ctx.storage.list_indexes(ctx.db_name, coll)
    if not indexes:
        return {
            "ok": 0.0,
            "errmsg": f"ns does not exist: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    return {
        "cursor": {
            "firstBatch": indexes,
            "id": bson.Int64(0),
            "ns": f"{ctx.db_name}.$cmd.listIndexes.{coll}",
        },
        "ok": 1.0,
    }


def _create_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import GeoExtractError, IndexConflict

    coll = doc["createIndexes"]
    indexes = doc.get("indexes", [])
    created_auto = not ctx.storage.collection_exists(ctx.db_name, coll)
    num_before = len(ctx.storage.list_indexes(ctx.db_name, coll)) if not created_auto else 1
    ctx.storage.create_collection(ctx.db_name, coll)
    created = 0
    for idx_spec in indexes:
        if not isinstance(idx_spec, dict):
            return {
                "ok": 0.0,
                "errmsg": "index spec must be a document",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        key_spec = idx_spec.get("key")
        name = idx_spec.get("name")
        if not isinstance(key_spec, dict) or not isinstance(name, str):
            return {
                "ok": 0.0,
                "errmsg": "index requires key (document) and name (string)",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        options = {k: v for k, v in idx_spec.items() if k not in ("key", "name")}
        try:
            new = ctx.storage.create_index(ctx.db_name, coll, name, key_spec, options)
        except IndexConflict as exc:
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 11000,
                "codeName": "DuplicateKey",
            }
        except GeoExtractError as exc:
            # `createIndex` on existing docs hit a doc the geo extractor
            # can't make sense of — fail the whole index creation, like
            # mongod. The collection is left without the index; the
            # client must clean up bad docs before retrying.
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 16572,
                "codeName": "Location16572",
            }
        if new:
            created += 1
    return {
        "createdCollectionAutomatically": created_auto,
        "numIndexesBefore": num_before,
        "numIndexesAfter": num_before + created,
        "ok": 1.0,
    }


def _drop_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    coll = doc["dropIndexes"]
    target = doc.get("index", "*")
    num_before = len(ctx.storage.list_indexes(ctx.db_name, coll))
    if num_before == 0:
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    if target == "*":
        ctx.storage.drop_all_indexes(ctx.db_name, coll)
        return {"nIndexesWas": num_before, "ok": 1.0}
    if target == "_id_":
        return {
            "ok": 0.0,
            "errmsg": "cannot drop _id index",
            "code": 67,
            "codeName": "InvalidOptions",
        }
    if not isinstance(target, str):
        return {
            "ok": 0.0,
            "errmsg": "index must be a string name or '*'",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    ok = ctx.storage.drop_index(ctx.db_name, coll, target)
    if not ok:
        return {
            "ok": 0.0,
            "errmsg": f"index not found with name [{target}]",
            "code": 27,
            "codeName": "IndexNotFound",
        }
    return {"nIndexesWas": num_before, "ok": 1.0}


def _kill_cursors(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    cursor_ids = [int(c) for c in doc.get("cursors", [])]
    # Wake any in-flight `_get_more` on these cursors BEFORE removing them
    # from the registry. The tailable getMore handler holds an `entry`
    # reference fetched at command start and sleeps in
    # `_oplog_cv.wait_for(...)` for up to its maxTimeMS / 1s budget. If we
    # only `pop` from the registry without touching `entry.invalidated` or
    # notifying the cv, that thread sleeps the full budget, wakes, sees
    # `entry.invalidated == False`, and returns `cursor.id != 0` — telling
    # the client the cursor is still alive after the client just killed it.
    # Drivers like the official Java driver then block in
    # `waitForLastRelease(...)` waiting for the connection holding that
    # zombie getMore to go idle, which manifests as the validate-java
    # suite hanging on `ChangeStreamOperationProseTestSpecification`.
    for cid in cursor_ids:
        try:
            entry = ctx.cursors.get(cid)
        except CursorNotFound:
            continue
        entry.invalidated = True
    if cursor_ids:
        with ctx.storage._oplog_cv:
            ctx.storage._oplog_cv.notify_all()
    killed, not_found = ctx.cursors.kill(cursor_ids)
    return {
        "cursorsKilled": killed,
        "cursorsNotFound": not_found,
        "cursorsAlive": [],
        "cursorsUnknown": [],
        "ok": 1.0,
    }


def _get_more(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    cursor_id = int(doc["getMore"])
    coll = doc.get("collection", "")
    batch_size = int(doc.get("batchSize", 0) or 0) or DEFAULT_BATCH_SIZE
    max_time_ms = int(doc.get("maxTimeMS", 0) or 0)
    ns = _ns(ctx.db_name, coll)
    try:
        entry = ctx.cursors.get(cursor_id)
    except CursorNotFound:
        return {
            "ok": 0.0,
            "errmsg": f"cursor id {cursor_id} not found",
            "code": 43,
            "codeName": "CursorNotFound",
        }
    # Cursor ownership check. ``getMore`` is in ``_NO_PRIVILEGE_COMMANDS``
    # (the original ``find`` / ``aggregate`` already authorised the
    # namespace), so without this check a connection that learns or
    # guesses someone else's cursor id can pull pages from a namespace
    # it has no privilege on, just by claiming the right ``collection``
    # in the request. Reject any getMore where the caller's claimed
    # namespace doesn't match the cursor's stored namespace — same
    # response mongod returns (CursorNotFound, code 43) so we don't
    # confirm-or-deny which cursor IDs exist on other connections.
    if entry.namespace and ns != entry.namespace:
        return {
            "ok": 0.0,
            "errmsg": f"cursor id {cursor_id} not found",
            "code": 43,
            "codeName": "CursorNotFound",
        }
    if not entry.tailable:
        try:
            batch, exhausted = ctx.cursors.next_batch(cursor_id, batch_size)
        except CursorNotFound:
            return {
                "ok": 0.0,
                "errmsg": f"cursor id {cursor_id} not found",
                "code": 43,
                "codeName": "CursorNotFound",
            }
        return {
            "cursor": {
                "nextBatch": batch,
                "id": bson.Int64(0 if exhausted else cursor_id),
                "ns": ns,
            },
            "ok": 1.0,
        }
    # Tailable path.
    if entry.invalidated and not entry.final_event_pending and not entry.remaining:
        ctx.cursors.kill([cursor_id])
        return {
            "cursor": {"nextBatch": [], "id": bson.Int64(0), "ns": ns},
            "ok": 1.0,
        }
    # Drain any already-buffered events first.
    if not entry.remaining and entry.producer is not None:
        new_events = entry.producer()
        if new_events:
            entry.remaining.extend(new_events)
    if not entry.remaining and entry.await_data and not entry.invalidated:
        # PyMongo does not always pass maxTimeMS on getMore for change streams;
        # real mongod treats that as "wait indefinitely". We bound the wait so
        # the connection thread can be reaped on shutdown.
        wait_seconds = max_time_ms / 1000.0 if max_time_ms > 0 else 1.0
        captured_tail = ctx.storage.oplog_tail_seq()
        with ctx.storage._oplog_cv:
            # Wake predicate must not acquire ``storage._lock`` — the
            # write path holds ``_lock`` then notifies under
            # ``_oplog_cv`` (lock order ``_lock -> _oplog_cv``), so a
            # waiter holding ``_oplog_cv`` and reaching for ``_lock``
            # would ABBA-deadlock against any concurrent insert /
            # update / delete. Use the lock-free tail peek; a stale
            # read self-corrects on the next iteration of wait_for.
            ctx.storage._oplog_cv.wait_for(
                lambda: ctx.storage.oplog_tail_seq_nolock() > captured_tail or entry.invalidated,
                timeout=wait_seconds,
            )
        if entry.producer is not None and not entry.remaining:
            new_events = entry.producer()
            if new_events:
                entry.remaining.extend(new_events)
    batch = entry.remaining[:batch_size]
    entry.remaining = entry.remaining[batch_size:]
    if not entry.remaining and entry.invalidated and entry.final_event_pending:
        # The invalidate event has now been delivered.
        entry.final_event_pending = False
    cursor_alive = not (entry.invalidated and not entry.remaining and not entry.final_event_pending)
    cursor_doc: dict[str, Any] = {
        "nextBatch": batch,
        # Cursor `id` MUST be int64 — Go driver hard-fails int32.
        "id": bson.Int64(cursor_id if cursor_alive else 0),
        "ns": ns,
    }
    if entry.last_token is not None:
        # `postBatchResumeToken` lets change-stream consumers advance
        # their resume position even when nextBatch is empty —
        # MongoDB 4.2+ feature, mongo-go-driver and pymongo expect it
        # on every change-stream getMore.
        cursor_doc["postBatchResumeToken"] = entry.last_token
    return {
        "cursor": cursor_doc,
        "ok": 1.0,
    }


def _aggregate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import IndexConflict

    from secantus import changestreams
    from secantus.storage import BadHint

    coll = doc["aggregate"]
    pipeline = doc.get("pipeline", [])
    hint = doc.get("hint")
    # ``let`` user-vars threaded into the pipeline context so
    # ``$expr`` clauses inside ``$match`` and the aggregation
    # expression language can resolve ``$$name`` references.
    let = _resolve_let_vars(doc.get("let"))
    cursor_opts = doc.get("cursor") or {}
    raw_agg_batch = cursor_opts.get("batchSize")
    batch_size = DEFAULT_BATCH_SIZE if raw_agg_batch is None else int(raw_agg_batch)
    coll_name = ""

    first_stage = pipeline[0] if pipeline else {}
    is_change_stream = isinstance(first_stage, Mapping) and "$changeStream" in first_stage

    if is_change_stream:
        return _aggregate_change_stream(doc, ctx, coll, pipeline, batch_size)

    if isinstance(coll, str):
        coll_name = coll
        if isinstance(first_stage, Mapping) and (
            "$collStats" in first_stage
            or "$indexStats" in first_stage
            or "$documents" in first_stage
        ):
            docs: list[dict[str, Any]] = []
        else:
            # If the first stage is $match, lift its filter into the initial
            # fetch so the index planner can use it. Skip the stage in the
            # pipeline so we don't apply the same filter twice.
            initial_filter: dict[str, Any] = {}
            if (
                isinstance(first_stage, Mapping)
                and "$match" in first_stage
                and isinstance(first_stage["$match"], Mapping)
            ):
                initial_filter = dict(first_stage["$match"])
                pipeline = list(pipeline[1:])
            try:
                docs = ctx.storage.find_matching(
                    ctx.db_name, coll, initial_filter, hint=hint, let=let
                )
            except BadHint as exc:
                return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
        ns = _ns(ctx.db_name, coll)
    else:
        docs = []
        ns = f"{ctx.db_name}.$cmd.aggregate"
    pipeline_ctx = PipelineContext(
        storage=ctx.storage,
        db_name=ctx.db_name,
        coll_name=coll_name,
        vars=dict(let) if let else {},
    )
    try:
        docs = apply_pipeline(docs, pipeline, pipeline_ctx)
    except IndexConflict as exc:
        # ``$merge whenMatched=fail`` raises this — surface mongod's
        # dup-key shape (code 11000 + keyPattern + keyValue) so the
        # driver's DuplicateKeyException path lights up.
        reply: dict[str, Any] = {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 11000,
            "codeName": "DuplicateKey",
        }
        if exc.key_pattern is not None:
            reply["keyPattern"] = exc.key_pattern
        if exc.key_value is not None:
            reply["keyValue"] = exc.key_value
        return reply
    first_batch, cursor_id = _split_into_cursor(docs, batch_size, ns, ctx.cursors)
    # Silence unused import in this branch.
    _ = changestreams
    return {
        # Cursor `id` MUST be int64 — the Go driver hard-fails int32 here.
        "cursor": {"firstBatch": first_batch, "id": bson.Int64(cursor_id), "ns": ns},
        "ok": 1.0,
    }


def _aggregate_change_stream(
    doc: dict[str, Any],
    ctx: CommandContext,
    coll: Any,
    pipeline: list[dict[str, Any]],
    batch_size: int,
) -> dict[str, Any]:
    from secantus import changestreams

    storage = ctx.storage
    spec = pipeline[0]["$changeStream"]
    if not isinstance(spec, Mapping):
        return {
            "ok": 0.0,
            "errmsg": "$changeStream spec must be a document",
            "code": 2,
            "codeName": "BadValue",
        }
    cs_spec = changestreams.parse_spec(spec)

    # Determine scope.
    if isinstance(coll, str):
        scope = {"kind": "coll", "db": ctx.db_name, "coll": coll}
        ns = _ns(ctx.db_name, coll)
        coll_name = coll
        coll_uuid = None
        if storage.collection_exists(ctx.db_name, coll):
            coll_uuid = storage.collection_uuid(ctx.db_name, coll)
    else:
        if ctx.db_name == "admin":
            scope = {"kind": "cluster"}
            ns = f"{ctx.db_name}.$cmd.aggregate"
        else:
            scope = {"kind": "db", "db": ctx.db_name}
            ns = f"{ctx.db_name}.$cmd.aggregate"
        coll_name = ""
        coll_uuid = None

    # Resolve start position.
    floor = storage.oplog_floor_seq()
    tail = storage.oplog_tail_seq()
    if cs_spec.resume_after is not None or cs_spec.start_after is not None:
        token = cs_spec.resume_after or cs_spec.start_after
        try:
            data = changestreams.parse_resume_token(token)
        except (ValueError, KeyError) as exc:
            return {
                "ok": 0.0,
                "errmsg": f"invalid resume token: {exc}",
                "code": 9,
                "codeName": "FailedToParse",
            }
        # Scope-bind the resume token. Without this check, a client
        # watching `db.collA` can craft a token with a different `ns`
        # (e.g. `db.collB`, or `secrets.users`) and read the oplog of
        # that other namespace from the embedded `seq`. mongod itself
        # leaves tokens unsigned (they're opaque transports of position),
        # but it gates the read at the resource ACL — SecantusDB doesn't
        # have full per-resource ACL plumbing, so the watch scope is
        # the boundary we must enforce here.
        if not changestreams._scope_matches(data.ns, scope):
            return {
                "ok": 0.0,
                "errmsg": (
                    "Resume token namespace does not match the change-stream "
                    "watch scope; refusing cross-namespace resume."
                ),
                "code": 13,  # Unauthorized
                "codeName": "Unauthorized",
            }
        history_lost = False
        if floor > 0 and data.seq < floor:
            history_lost = True
        # Empty oplog after pruning: any token referencing a past-emitted seq
        # is unresumable.
        if floor == 0 and tail > 0 and data.seq <= tail:
            history_lost = True
        if history_lost:
            return {
                "ok": 0.0,
                "errmsg": (
                    "Resume of change stream was not possible, as the resume "
                    "point may no longer be in the oplog."
                ),
                "code": changestreams.ChangeStreamHistoryLost.code,
                "codeName": changestreams.ChangeStreamHistoryLost.codeName,
            }
        start_seq = data.seq + 1
    elif cs_spec.start_at_operation_time is not None:
        start_seq = storage.find_seq_for_ts(cs_spec.start_at_operation_time)
    else:
        start_seq = storage.oplog_tail_seq() + 1

    # Build the namespace filter once for cheap reuse.
    def _ns_filter(entry_ns: str) -> bool:
        kind = scope.get("kind")
        if kind == "cluster":
            return True
        if kind == "db":
            db_target = scope.get("db")
            return entry_ns.startswith(f"{db_target}.")
        if kind == "coll":
            db_target = scope.get("db")
            coll_target = scope.get("coll")
            return entry_ns == f"{db_target}.{coll_target}" or entry_ns == f"{db_target}.$cmd"
        return False

    pipeline_after_cs = list(pipeline[1:])
    pipeline_ctx = PipelineContext(
        storage=storage, db_name=ctx.db_name, coll_name=coll_name, change_stream=cs_spec
    )

    # Bind the entry by reference; producer closes over it.
    entry_ref: dict[str, Any] = {"entry": None}

    def producer() -> list[dict[str, Any]]:
        entry = entry_ref["entry"]
        if entry is None:
            return []
        rows = storage.read_oplog(start_seq=entry.position_seq + 1, limit=200, ns_filter=_ns_filter)
        if not rows:
            # No new MATCHING oplog entries since last poll, but the
            # oplog as a whole may have moved on (writes on other
            # collections, periodic noop heartbeats). The
            # postBatchResumeToken should advance to reflect the
            # latest oplog position so consumers on quiet collections
            # can resume past unrelated activity — mongo-go-driver's
            # ``resume_token_updated_on_empty_batch`` test asserts
            # exactly that. Pin ``position_seq`` to the oplog tail so
            # the next read starts from there.
            tail_seq = storage.oplog_tail_seq()
            if tail_seq > entry.position_seq:
                entry.position_seq = tail_seq
            ts = storage.current_cluster_time()
            entry.last_token = changestreams.make_resume_token(
                changestreams.ResumeTokenData(entry.position_seq, ts, ns, {})
            )
            return []
        events: list[dict[str, Any]] = []
        last_seen = entry.position_seq
        last_seen_ts = None
        last_seen_ns = ns
        for seq, oplog_entry in rows:
            try:
                ev, invalidates = changestreams.project(
                    seq,
                    oplog_entry,
                    storage=storage,
                    full_document_mode=cs_spec.full_document_mode,
                    full_document_before_change_mode=cs_spec.full_document_before_change_mode,
                    scope=scope,
                )
            except changestreams.ChangeStreamFatalError as exc:
                # Best effort: surface as an empty batch and let the next
                # poll retry. A nicer surface would be a server-side error
                # cursor; defer.
                _ = exc
                last_seen = seq
                continue
            if ev is not None:
                if cs_spec.split_large_events:
                    changestreams.stamp_split_event(ev)
                events.append(ev)
            last_seen = seq
            ts_field = oplog_entry.get("ts")
            if ts_field is not None:
                last_seen_ts = ts_field
            ns_field = oplog_entry.get("ns")
            if isinstance(ns_field, str) and ns_field:
                last_seen_ns = ns_field
            if invalidates:
                inv = changestreams.invalidate_event(seq, oplog_entry)
                if cs_spec.split_large_events:
                    changestreams.stamp_split_event(inv)
                events.append(inv)
                entry.invalidated = True
                entry.final_event_pending = True
                break
        entry.position_seq = last_seen
        if last_seen_ts is None:
            last_seen_ts = storage.current_cluster_time()
        entry.last_token = changestreams.make_resume_token(
            changestreams.ResumeTokenData(last_seen, last_seen_ts, last_seen_ns, {})
        )
        if pipeline_after_cs:
            events = apply_pipeline(events, pipeline_after_cs, pipeline_ctx)
        return events

    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=True,
        position_seq=start_seq - 1,
        collection_uuid=coll_uuid,
    )
    entry_ref["entry"] = ctx.cursors.get(cursor_id)
    _ = batch_size  # firstBatch is empty by design for change streams
    initial_ts = ctx.storage.current_cluster_time()
    initial_token = changestreams.make_resume_token(
        changestreams.ResumeTokenData(start_seq - 1, initial_ts, ns, {})
    )
    entry_ref["entry"].last_token = initial_token
    return {
        "cursor": {
            "firstBatch": [],
            "id": bson.Int64(cursor_id),
            "ns": ns,
            "postBatchResumeToken": initial_token,
        },
        "operationTime": initial_ts,
        "ok": 1.0,
    }


# --- Authentication (SCRAM-SHA-256) ---

# MongoDB error codes used in this section.
_AUTHENTICATION_FAILED = 18
_USER_NOT_FOUND = 11
_USER_ALREADY_EXISTS = 51003
_NO_SUCH_USER_GENERIC = 11
_AUTH_NOT_REQUIRED = "Auth not required by server configuration"


def _auth_failure(msg: str = "Authentication failed.") -> dict[str, Any]:
    return {
        "ok": 0.0,
        "errmsg": msg,
        "code": _AUTHENTICATION_FAILED,
        "codeName": "AuthenticationFailed",
    }


def _payload_bytes(value: Any) -> bytes:
    """SCRAM payload arrives as BSON Binary; getter handles both Binary and bytes."""
    if isinstance(value, bson.Binary):
        return bytes(value)
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    return b""


def _payload_binary(b: bytes) -> bson.Binary:
    return bson.Binary(b, subtype=0)


def _ensure_conn_auth(ctx: CommandContext) -> ConnectionAuth:
    if ctx.connection_auth is None:
        # Should never happen in production paths — server.py always
        # constructs one. Fail closed.
        raise RuntimeError("connection auth state missing")
    return ctx.connection_auth


def _mechs_for_principal(ctx: CommandContext, principal: str) -> list[str]:
    """Resolve `saslSupportedMechs` for ``"<db>.<user>"``.

    Looks up the user record and returns the mechanisms its
    ``credentials`` doc carries entries for, falling back to
    ``[SCRAM_SHA_256]`` when the principal is unknown so the driver
    still tries a real mechanism.
    """
    db, _, username = principal.partition(".")
    if not username:
        return [SCRAM_SHA_256]
    record = ctx.storage.get_user(db, username)
    if record is None:
        return [SCRAM_SHA_256]
    creds_doc = record.get("credentials")
    if not isinstance(creds_doc, dict):
        return [SCRAM_SHA_256]
    # Order: modern first when both are present.
    mechs = [m for m in (SCRAM_SHA_256, SCRAM_SHA_1) if m in creds_doc]
    return mechs or [SCRAM_SHA_256]


def _lookup_creds(
    storage: Storage, db: str, username: str, *, mechanism: str = SCRAM_SHA_256
) -> StoredCredentials | None:
    record = storage.get_user(db, username)
    if record is None:
        return None
    creds_doc = record.get("credentials")
    if not isinstance(creds_doc, dict) or mechanism not in creds_doc:
        return None
    return StoredCredentials.from_doc(creds_doc, mechanism=mechanism)


def _sasl_start(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    conn = _ensure_conn_auth(ctx)
    mechanism = doc.get("mechanism", "")
    if mechanism not in (SCRAM_SHA_256, SCRAM_SHA_1):
        return _auth_failure(
            f"Unsupported SASL mechanism: {mechanism!r} (supported: {SCRAM_SHA_256}, {SCRAM_SHA_1})"
        )
    payload = _payload_bytes(doc.get("payload"))
    db_name = ctx.db_name or "admin"
    creds = _lookup_creds(ctx.storage, db_name, _peek_scram_username(payload), mechanism=mechanism)
    try:
        server_first, state = begin_scram(
            conversation_id=conn.new_conversation_id(),
            db_name=db_name,
            payload=payload,
            creds=creds,
            mechanism=mechanism,
        )
    except AuthError as exc:
        return _auth_failure(str(exc))
    conn.scram = state
    return {
        "conversationId": state.conversation_id,
        "done": False,
        "payload": _payload_binary(server_first),
        "ok": 1.0,
    }


def _peek_scram_username(payload: bytes) -> str:
    """Best-effort SCRAM `n=user` extraction for credential lookup.

    A malformed payload returns "" here, which makes ``_lookup_creds``
    return None, which makes ``begin_scram`` fabricate credentials and
    surface AuthError on the proof step — same shape as a wrong-password
    attempt. Reusing the parser keeps the lookup path single-source.
    """
    if not payload.startswith(b"n,"):
        return ""
    gs2_end = payload.find(b",", 2)
    if gs2_end < 0:
        return ""
    bare = payload[gs2_end + 1 :]
    for chunk in bare.split(b","):
        if chunk.startswith(b"n="):
            return chunk[2:].decode("utf-8", errors="replace")
    return ""


def _sasl_continue(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    conn = _ensure_conn_auth(ctx)
    state = conn.scram
    if state is None:
        return _auth_failure("No SCRAM conversation in progress")
    incoming_id = doc.get("conversationId")
    if incoming_id != state.conversation_id:
        return _auth_failure("SCRAM conversation id mismatch")
    payload = _payload_bytes(doc.get("payload"))
    try:
        server_final = continue_scram(state, payload)
    except AuthError as exc:
        conn.scram = None
        return _auth_failure(str(exc))
    # Successful proof. Mark the principal authenticated, capture their
    # role bindings for RBAC, and clear the in-flight conversation.
    # MongoDB returns done=True from the second server message
    # (skipping the spec's optional 3rd round-trip).
    principal = (state.db_name, state.username)
    if principal not in conn.authenticated_principals:
        conn.authenticated_principals.append(principal)
    record = ctx.storage.get_user(state.db_name, state.username)
    if record is not None:
        roles = record.get("roles") or []
        if isinstance(roles, list):
            conn.add_principal_roles(roles)
    if ctx.connections is not None:
        ctx.connections.authenticate(ctx.connection_id, f"{state.username}@{state.db_name}")
    conn.scram = None
    return {
        "conversationId": state.conversation_id,
        "done": True,
        "payload": _payload_binary(server_final),
        "ok": 1.0,
    }


def _create_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("createUser")
    pwd = doc.get("pwd")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "createUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    if not isinstance(pwd, str) or not pwd:
        return {
            "ok": 0.0,
            "errmsg": "createUser: pwd (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    roles_arg = doc.get("roles", []) or []
    normalised = _normalise_roles_arg(roles_arg, db_name, storage=ctx.storage)
    if normalised is None:
        return {
            "ok": 0.0,
            "errmsg": "createUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    # `mechanisms` selects which SCRAM hashes to derive credentials
    # for. Default to SCRAM-SHA-256 alone, matching mongod's modern
    # default (post-3.6 the SHA-1 hashing isn't computed unless the
    # request asks for it).
    mechanisms_arg = doc.get("mechanisms")
    if isinstance(mechanisms_arg, list) and mechanisms_arg:
        requested = [m for m in mechanisms_arg if m in (SCRAM_SHA_256, SCRAM_SHA_1)]
    else:
        requested = [SCRAM_SHA_256]
    if not requested:
        return {
            "ok": 0.0,
            "errmsg": (
                "createUser: mechanisms must contain at least one of "
                f"{SCRAM_SHA_256!r}, {SCRAM_SHA_1!r}"
            ),
            "code": 2,
            "codeName": "BadValue",
        }
    creds_doc: dict[str, object] = {}
    for mech in requested:
        creds_doc.update(derive_credentials(pwd, mechanism=mech, username=username).to_doc())
    record = {
        "_id": f"{db_name}.{username}",
        "user": username,
        "db": db_name,
        "credentials": creds_doc,
        "roles": normalised,
        "mechanisms": requested,
    }
    added = ctx.storage.add_user(db_name, username, record, replace=False)
    if not added:
        return {
            "ok": 0.0,
            "errmsg": f'User "{username}@{db_name}" already exists',
            "code": _USER_ALREADY_EXISTS,
            "codeName": "Location51003",
        }
    return {"ok": 1.0}


def _update_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Rotate a password and/or replace a user's role bindings in place.

    Real ``mongod``'s ``updateUser`` updates the existing record without
    invalidating other connections — drop+recreate would force every
    authenticated client off. We do the same: re-derive credentials when
    ``pwd`` is supplied, replace ``roles`` when supplied, leave the rest
    alone. The calling connection's effective_roles refresh live so a
    role change takes effect on the next command.
    """
    username = doc.get("updateUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "updateUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    pwd = doc.get("pwd")
    roles_arg = doc.get("roles")
    if pwd is None and roles_arg is None:
        return {
            "ok": 0.0,
            "errmsg": "updateUser: nothing to update (supply pwd and/or roles)",
            "code": 2,
            "codeName": "BadValue",
        }
    if pwd is not None:
        if not isinstance(pwd, str) or not pwd:
            return {
                "ok": 0.0,
                "errmsg": "updateUser: pwd must be a non-empty string",
                "code": 2,
                "codeName": "BadValue",
            }
        # Re-derive credentials for whichever mechanisms the existing
        # record was provisioned with, so updateUser preserves a
        # SHA-1+SHA-256 user as SHA-1+SHA-256.
        existing_mechs = record.get("mechanisms")
        if isinstance(existing_mechs, list) and existing_mechs:
            mechs = [m for m in existing_mechs if m in (SCRAM_SHA_256, SCRAM_SHA_1)]
        else:
            mechs = [SCRAM_SHA_256]
        new_creds: dict[str, object] = {}
        for mech in mechs:
            new_creds.update(derive_credentials(pwd, mechanism=mech, username=username).to_doc())
        record["credentials"] = new_creds
    if roles_arg is not None:
        normalised = _normalise_roles_arg(roles_arg, db_name, storage=ctx.storage)
        if normalised is None:
            return {
                "ok": 0.0,
                "errmsg": "updateUser: roles must be a list of known roles",
                "code": 31,
                "codeName": "RoleNotFound",
            }
        record["roles"] = normalised
    ctx.storage.add_user(db_name, username, record, replace=True)
    if roles_arg is not None:
        _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _drop_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("dropUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "dropUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    removed = ctx.storage.drop_user(db_name, username)
    if not removed:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    # Drop any active auth state for this principal on the calling
    # connection — both the principal entry and the role bindings the
    # connection inherited from this user. Other connections keep
    # theirs until they reconnect, matching mongod.
    conn = ctx.connection_auth
    if conn is not None:
        conn.authenticated_principals = [
            p for p in conn.authenticated_principals if p != (db_name, username)
        ]
        # Recompute effective roles by re-fetching the remaining
        # principals' role bindings. Cheap (small list, infrequent op).
        conn.effective_roles = []
        for p_db, p_user in conn.authenticated_principals:
            other = ctx.storage.get_user(p_db, p_user)
            if other is not None:
                roles = other.get("roles") or []
                if isinstance(roles, list):
                    conn.add_principal_roles(roles)
    return {"ok": 1.0}


def _drop_all_users_from_database(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop every user record bound to the calling database.

    Mirrors ``mongod`` semantics: returns ``{ok: 1, n: <count>}`` with
    ``n`` set to the number of records removed (0 is fine — empty db).
    Active auth state on the calling connection is invalidated for any
    principals scoped to this db; other connections keep theirs until
    they reconnect.
    """
    db_name = ctx.db_name or "admin"
    # Use a generous limit + paginate manually so we don't load every
    # user across the whole connection at once on a megacorp deploy.
    removed = 0
    while True:
        batch = ctx.storage.list_users(db_name, skip=0, limit=1000)
        if not batch:
            break
        for record in batch:
            username = record.get("user")
            if isinstance(username, str) and ctx.storage.drop_user(db_name, username):
                removed += 1
        if len(batch) < 1000:
            break

    conn = ctx.connection_auth
    if conn is not None:
        conn.authenticated_principals = [
            p for p in conn.authenticated_principals if p[0] != db_name
        ]
        conn.effective_roles = []
        for p_db, p_user in conn.authenticated_principals:
            other = ctx.storage.get_user(p_db, p_user)
            if other is not None:
                roles = other.get("roles") or []
                if isinstance(roles, list):
                    conn.add_principal_roles(roles)
    return {"ok": 1.0, "n": removed}


def _users_info(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    db_name = ctx.db_name or "admin"
    arg = doc.get("usersInfo")
    show_credentials = bool(doc.get("showCredentials", False))

    def _public(record: dict[str, Any]) -> dict[str, Any]:
        out = dict(record)
        if not show_credentials:
            out.pop("credentials", None)
        return out

    users: list[dict[str, Any]]
    if arg == 1 or arg is True:
        # All users in this database.
        users = [_public(r) for r in ctx.storage.list_users(db_name)]
    elif isinstance(arg, str):
        record = ctx.storage.get_user(db_name, arg)
        users = [_public(record)] if record else []
    elif isinstance(arg, dict):
        # `{user: "name", db: "other"}` — single specific principal.
        u = arg.get("user")
        d = arg.get("db", db_name)
        if isinstance(u, str) and isinstance(d, str):
            record = ctx.storage.get_user(d, u)
            users = [_public(record)] if record else []
        else:
            users = []
    elif isinstance(arg, list):
        users = []
        for entry in arg:
            if isinstance(entry, str):
                record = ctx.storage.get_user(db_name, entry)
            elif isinstance(entry, dict):
                u = entry.get("user")
                d = entry.get("db", db_name)
                if isinstance(u, str) and isinstance(d, str):
                    record = ctx.storage.get_user(d, u)
                else:
                    record = None
            else:
                record = None
            if record is not None:
                users.append(_public(record))
    else:
        users = []
    return {"users": users, "ok": 1.0}


def _normalise_roles_arg(
    arg: Any,
    default_db: str,
    *,
    storage: Storage | None = None,
) -> list[dict[str, str]] | None:
    """Coerce a ``roles`` argument into the canonical ``[{role, db}]`` shape.

    Accepts the list-of-strings shorthand (``["read", "readWrite"]`` —
    each implicitly bound to ``default_db``) and the list-of-dicts form.
    Role names validate against :data:`secantus.rbac.BUILT_IN_ROLES`
    first, then fall through to ``storage.get_role(db, name)`` so
    custom roles are accepted alongside built-ins. Returns ``None``
    if any entry is unrecognised — caller surfaces a ``RoleNotFound``
    error.
    """

    def _resolves(role: str, db: str) -> bool:
        if is_known_role(role):
            return True
        if storage is None:
            return False
        return storage.get_role(db, role) is not None

    if not isinstance(arg, list):
        return None
    out: list[dict[str, str]] = []
    for entry in arg:
        if isinstance(entry, str):
            if not _resolves(entry, default_db):
                return None
            out.append({"role": entry, "db": default_db})
            continue
        if isinstance(entry, dict):
            role = entry.get("role")
            db = entry.get("db", default_db)
            if not isinstance(role, str) or not isinstance(db, str):
                return None
            if not _resolves(role, db):
                return None
            out.append({"role": role, "db": db})
            continue
        return None
    return out


def _grant_roles_to_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("grantRolesToUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    roles_arg = doc.get("roles")
    new_roles = _normalise_roles_arg(roles_arg, db_name, storage=ctx.storage)
    if new_roles is None:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    existing = record.get("roles") or []
    seen = {(r.get("role"), r.get("db")) for r in existing if isinstance(r, dict)}
    for r in new_roles:
        key = (r["role"], r["db"])
        if key not in seen:
            existing.append(r)
            seen.add(key)
    record["roles"] = existing
    ctx.storage.add_user(db_name, username, record, replace=True)
    # Refresh effective roles on this connection if the calling user is
    # the one that just got granted — the new privileges take effect
    # immediately, not on the next connection.
    _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _revoke_roles_from_user(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    username = doc.get("revokeRolesFromUser")
    if not isinstance(username, str) or not username:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromUser: username (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    roles_arg = doc.get("roles")
    to_revoke = _normalise_roles_arg(roles_arg, db_name, storage=ctx.storage)
    if to_revoke is None:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromUser: roles must be a list of known roles",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    record = ctx.storage.get_user(db_name, username)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"User '{username}@{db_name}' not found",
            "code": _USER_NOT_FOUND,
            "codeName": "UserNotFound",
        }
    revoke_keys = {(r["role"], r["db"]) for r in to_revoke}
    record["roles"] = [
        r
        for r in (record.get("roles") or [])
        if isinstance(r, dict) and (r.get("role"), r.get("db")) not in revoke_keys
    ]
    ctx.storage.add_user(db_name, username, record, replace=True)
    _refresh_effective_roles(ctx)
    return {"ok": 1.0}


def _refresh_effective_roles(ctx: CommandContext) -> None:
    """Rebuild the connection's effective_roles from current user records.

    Called after `grantRolesToUser` / `revokeRolesFromUser` so the
    privilege check reflects the change immediately on this connection.
    Other connections refresh on their next reconnect.
    """
    conn = ctx.connection_auth
    if conn is None:
        return
    conn.effective_roles = []
    for p_db, p_user in conn.authenticated_principals:
        record = ctx.storage.get_user(p_db, p_user)
        if record is None:
            continue
        roles = record.get("roles") or []
        if isinstance(roles, list):
            conn.add_principal_roles(roles)


def _normalise_privileges(arg: Any) -> list[dict[str, Any]] | None:
    """Validate / normalise a ``privileges`` array. Returns the cleaned
    list, or ``None`` if any entry is malformed (caller maps to
    BadValue). An empty list is fine — a role can be a pure inheritor.
    """
    if not isinstance(arg, list):
        return None
    out: list[dict[str, Any]] = []
    for priv in arg:
        if not isinstance(priv, Mapping):
            return None
        resource = priv.get("resource")
        actions = priv.get("actions")
        if not isinstance(resource, Mapping) or not isinstance(actions, list):
            return None
        if not all(isinstance(a, str) for a in actions):
            return None
        out.append({"resource": dict(resource), "actions": list(actions)})
    return out


def _normalise_inherited_roles(arg: Any, default_db: str) -> list[dict[str, str]] | None:
    """Validate / normalise an inherited ``roles`` array. Each entry is
    ``"<name>"`` (uses default_db) or ``{"role": <name>, "db": <db>}``.
    Returns ``None`` on malformed input.
    """
    if not isinstance(arg, list):
        return None
    out: list[dict[str, str]] = []
    for entry in arg:
        if isinstance(entry, str) and entry:
            out.append({"role": entry, "db": default_db})
        elif isinstance(entry, Mapping):
            name = entry.get("role")
            db = entry.get("db", default_db)
            if not isinstance(name, str) or not name:
                return None
            if not isinstance(db, str) or not db:
                return None
            out.append({"role": name, "db": db})
        else:
            return None
    return out


def _create_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Define a custom role with privileges and (optionally) inherited
    roles. Mongod-shaped: ``createRole`` ``privileges`` ``roles``.
    Rejects names that collide with built-ins (matches mongod, which
    refuses ``createRole: "read"`` etc.).
    """
    name = doc.get("createRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "createRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    if name in BUILT_IN_ROLES:
        return {
            "ok": 0.0,
            "errmsg": f"Cannot create role with name {name!r}: name is reserved for a built-in",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    privileges = _normalise_privileges(doc.get("privileges"))
    if privileges is None:
        return {
            "ok": 0.0,
            "errmsg": "createRole: privileges must be an array of {resource, actions}",
            "code": 2,
            "codeName": "BadValue",
        }
    inherited = _normalise_inherited_roles(doc.get("roles"), db_name)
    if inherited is None:
        return {
            "ok": 0.0,
            "errmsg": "createRole: roles must be a list of names or {role, db} dicts",
            "code": 2,
            "codeName": "BadValue",
        }
    record = {
        "_id": f"{db_name}.{name}",
        "role": name,
        "db": db_name,
        "privileges": privileges,
        "roles": inherited,
    }
    if not ctx.storage.add_role(db_name, name, record, replace=False):
        return {
            "ok": 0.0,
            "errmsg": f'Role "{name}@{db_name}" already exists',
            "code": 51002,
            "codeName": "Location51002",
        }
    return {"ok": 1.0}


def _update_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Replace a custom role's privileges / inherited roles in place.
    Either ``privileges`` or ``roles`` (or both) may be supplied;
    omitted fields stay as-is. Mongod-shaped.
    """
    name = doc.get("updateRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "updateRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_role(db_name, name)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    if "privileges" in doc:
        privileges = _normalise_privileges(doc["privileges"])
        if privileges is None:
            return {
                "ok": 0.0,
                "errmsg": "updateRole: privileges must be an array of {resource, actions}",
                "code": 2,
                "codeName": "BadValue",
            }
        record["privileges"] = privileges
    if "roles" in doc:
        inherited = _normalise_inherited_roles(doc["roles"], db_name)
        if inherited is None:
            return {
                "ok": 0.0,
                "errmsg": "updateRole: roles must be a list of names or {role, db} dicts",
                "code": 2,
                "codeName": "BadValue",
            }
        record["roles"] = inherited
    ctx.storage.add_role(db_name, name, record, replace=True)
    return {"ok": 1.0}


def _drop_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = doc.get("dropRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "dropRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    if not ctx.storage.drop_role(db_name, name):
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    return {"ok": 1.0}


def _drop_all_roles_from_database(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop every custom role bound to the calling db. Returns ``n`` =
    removed count. Built-in roles are not affected (they're never
    persisted)."""
    db_name = ctx.db_name or "admin"
    removed = 0
    while True:
        batch = ctx.storage.list_roles(db_name, skip=0, limit=1000)
        if not batch:
            break
        for record in batch:
            role = record.get("role")
            if isinstance(role, str) and ctx.storage.drop_role(db_name, role):
                removed += 1
        if len(batch) < 1000:
            break
    return {"ok": 1.0, "n": removed}


def _grant_privileges_to_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = doc.get("grantPrivilegesToRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "grantPrivilegesToRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_role(db_name, name)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    additions = _normalise_privileges(doc.get("privileges"))
    if additions is None:
        return {
            "ok": 0.0,
            "errmsg": ("grantPrivilegesToRole: privileges must be an array of {resource, actions}"),
            "code": 2,
            "codeName": "BadValue",
        }
    privs = list(record.get("privileges") or [])
    for add in additions:
        merged = False
        for existing in privs:
            if existing.get("resource") == add["resource"]:
                # Merge actions, dedupe.
                existing_actions = list(existing.get("actions") or [])
                for a in add["actions"]:
                    if a not in existing_actions:
                        existing_actions.append(a)
                existing["actions"] = existing_actions
                merged = True
                break
        if not merged:
            privs.append(add)
    record["privileges"] = privs
    ctx.storage.add_role(db_name, name, record, replace=True)
    return {"ok": 1.0}


def _revoke_privileges_from_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = doc.get("revokePrivilegesFromRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "revokePrivilegesFromRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_role(db_name, name)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    revocations = _normalise_privileges(doc.get("privileges"))
    if revocations is None:
        return {
            "ok": 0.0,
            "errmsg": (
                "revokePrivilegesFromRole: privileges must be an array of {resource, actions}"
            ),
            "code": 2,
            "codeName": "BadValue",
        }
    privs = list(record.get("privileges") or [])
    for rev in revocations:
        for existing in privs:
            if existing.get("resource") != rev["resource"]:
                continue
            actions = [a for a in (existing.get("actions") or []) if a not in rev["actions"]]
            existing["actions"] = actions
    # Drop privileges that have no actions left.
    record["privileges"] = [p for p in privs if p.get("actions")]
    ctx.storage.add_role(db_name, name, record, replace=True)
    return {"ok": 1.0}


def _grant_roles_to_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = doc.get("grantRolesToRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_role(db_name, name)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    additions = _normalise_inherited_roles(doc.get("roles"), db_name)
    if additions is None:
        return {
            "ok": 0.0,
            "errmsg": "grantRolesToRole: roles must be a list of names or {role, db} dicts",
            "code": 2,
            "codeName": "BadValue",
        }
    seen = {(r["role"], r["db"]) for r in record.get("roles") or [] if isinstance(r, Mapping)}
    inherited = list(record.get("roles") or [])
    for add in additions:
        if (add["role"], add["db"]) not in seen:
            inherited.append(add)
            seen.add((add["role"], add["db"]))
    record["roles"] = inherited
    ctx.storage.add_role(db_name, name, record, replace=True)
    return {"ok": 1.0}


def _revoke_roles_from_role(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = doc.get("revokeRolesFromRole")
    if not isinstance(name, str) or not name:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromRole: role name (string) required",
            "code": 2,
            "codeName": "BadValue",
        }
    db_name = ctx.db_name or "admin"
    record = ctx.storage.get_role(db_name, name)
    if record is None:
        return {
            "ok": 0.0,
            "errmsg": f"Role {name!r} not found on database {db_name!r}",
            "code": 31,
            "codeName": "RoleNotFound",
        }
    revocations = _normalise_inherited_roles(doc.get("roles"), db_name)
    if revocations is None:
        return {
            "ok": 0.0,
            "errmsg": "revokeRolesFromRole: roles must be a list of names or {role, db} dicts",
            "code": 2,
            "codeName": "BadValue",
        }
    drop_set = {(r["role"], r["db"]) for r in revocations}
    record["roles"] = [
        r
        for r in (record.get("roles") or [])
        if isinstance(r, Mapping) and (r.get("role"), r.get("db")) not in drop_set
    ]
    ctx.storage.add_role(db_name, name, record, replace=True)
    return {"ok": 1.0}


def _roles_info(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Return information about built-in and custom roles.

    Built-in roles surface as ``isBuiltin: true`` with empty
    ``inheritedRoles``. Custom roles are looked up from storage and
    their ``privileges`` / ``roles`` arrays surface as-is. The
    ``rolesInfo`` argument matches mongod:

    * ``1`` / ``true`` — every custom role on the calling db (plus
      built-ins when ``showBuiltinRoles: true``).
    * ``"<name>"`` or ``{role, db}`` — single role lookup.
    * ``[ ... ]`` — multi-role lookup.
    """
    arg = doc.get("rolesInfo")
    db_name = ctx.db_name or "admin"
    show_builtin = bool(doc.get("showBuiltinRoles", False))

    def _builtin_entry(role_name: str, role_db: str) -> dict[str, Any]:
        return {
            "role": role_name,
            "db": role_db,
            "isBuiltin": True,
            "roles": [],
            "inheritedRoles": [],
        }

    def _custom_entry(record: Mapping[str, Any]) -> dict[str, Any]:
        return {
            "role": record.get("role"),
            "db": record.get("db"),
            "isBuiltin": False,
            "privileges": list(record.get("privileges") or []),
            "roles": list(record.get("roles") or []),
            "inheritedRoles": list(record.get("roles") or []),
        }

    out: list[dict[str, Any]] = []
    if arg == 1 or arg is True:
        # Custom roles in the calling db.
        for record in ctx.storage.list_roles(db_name):
            out.append(_custom_entry(record))
        if show_builtin:
            for name in BUILT_IN_ROLES:
                out.append(_builtin_entry(name, db_name))
    else:
        targets: list[tuple[str, str]] = []
        if isinstance(arg, str):
            targets.append((arg, db_name))
        elif isinstance(arg, Mapping):
            role = arg.get("role")
            db = arg.get("db", db_name)
            if isinstance(role, str) and isinstance(db, str):
                targets.append((role, db))
        elif isinstance(arg, list):
            for entry in arg:
                if isinstance(entry, str):
                    targets.append((entry, db_name))
                elif isinstance(entry, Mapping):
                    role = entry.get("role")
                    db = entry.get("db", db_name)
                    if isinstance(role, str) and isinstance(db, str):
                        targets.append((role, db))
        for role_name, role_db in targets:
            if role_name in BUILT_IN_ROLES:
                out.append(_builtin_entry(role_name, role_db))
                continue
            record = ctx.storage.get_role(role_db, role_name)
            if record is not None:
                out.append(_custom_entry(record))
    return {"roles": out, "ok": 1.0}


_HANDLERS: dict[str, CommandHandler] = {
    "hello": _hello,
    "isMaster": _hello,
    "ismaster": _hello,
    "ping": _ping,
    "buildInfo": _build_info,
    "buildinfo": _build_info,
    "endSessions": _end_sessions,
    "startSession": _start_session,
    "refreshSessions": _refresh_sessions,
    "killSessions": _kill_sessions,
    "killAllSessions": _kill_all_sessions,
    "killAllSessionsByPattern": _kill_all_sessions_by_pattern,
    "abortTransaction": _abort_transaction,
    "commitTransaction": _commit_transaction,
    "getLog": _get_log,
    "whatsmyuri": _whatsmyuri,
    "hostInfo": _hostinfo,
    "currentOp": _current_op,
    "fsync": _fsync,
    "profile": _profile,
    "secantusAdmin.pruneOplog": _secantus_admin_prune_oplog,
    "secantusAdmin.pruneTtl": _secantus_admin_prune_ttl,
    "explain": _explain,
    "serverStatus": _server_status,
    "getCmdLineOpts": _get_cmd_line_opts,
    "getParameter": _get_parameter,
    "connectionStatus": _connection_status,
    "dbStats": _db_stats,
    "dbstats": _db_stats,
    "collStats": _coll_stats,
    "insert": _insert,
    "find": _find,
    "update": _update,
    "delete": _delete,
    "count": _count,
    "distinct": _distinct,
    "findAndModify": _find_and_modify,
    "findandmodify": _find_and_modify,
    "drop": _drop,
    "create": _create,
    "collMod": _coll_mod,
    "dropDatabase": _drop_database,
    "renameCollection": _rename_collection,
    "listCollections": _list_collections,
    "listDatabases": _list_databases,
    "listIndexes": _list_indexes,
    "createIndexes": _create_indexes,
    "dropIndexes": _drop_indexes,
    "killCursors": _kill_cursors,
    "getMore": _get_more,
    "aggregate": _aggregate,
    "saslStart": _sasl_start,
    "saslContinue": _sasl_continue,
    "configureFailPoint": _configure_fail_point,
    "createUser": _create_user,
    "updateUser": _update_user,
    "dropUser": _drop_user,
    "dropAllUsersFromDatabase": _drop_all_users_from_database,
    "usersInfo": _users_info,
    "grantRolesToUser": _grant_roles_to_user,
    "revokeRolesFromUser": _revoke_roles_from_user,
    "rolesInfo": _roles_info,
    "createRole": _create_role,
    "updateRole": _update_role,
    "dropRole": _drop_role,
    "dropAllRolesFromDatabase": _drop_all_roles_from_database,
    "grantPrivilegesToRole": _grant_privileges_to_role,
    "revokePrivilegesFromRole": _revoke_privileges_from_role,
    "grantRolesToRole": _grant_roles_to_role,
    "revokeRolesFromRole": _revoke_roles_from_role,
}

# Commands a connection may invoke before authenticating, when
# require_auth=True. The handshake plus the auth handshake itself.
# getLog and hostInfo were previously listed here, which let an
# unauthenticated peer dump the in-memory log buffer (which can contain
# operation parameters, error messages, and other operational details)
# and read the server host's OS/CPU info. Both leak server-internal
# state and have no role in the driver handshake — pymongo / mongo-go /
# mongo-node / mongo-java all complete connect+auth without calling
# them. Require auth like every other command of their privilege class.
_PRE_AUTH_COMMANDS = frozenset(
    {
        "hello",
        "isMaster",
        "ismaster",
        "ping",
        "buildInfo",
        "buildinfo",
        "saslStart",
        "saslContinue",
        "endSessions",
        "whatsmyuri",
    }
)


# Per-command (action, scope) for the RBAC privilege check. The dispatcher
# reads this; missing entries default to "no privilege required" so a
# user with even minimal roles can still invoke them. Adding a new
# command? Add it here (or to ``_NO_PRIVILEGE_COMMANDS`` for explicit
# bypasses) — silent permissive defaults are how mongod's auth surface
# tends to grow accidental privilege escalations, so be deliberate.
_COMMAND_ACTIONS: dict[str, tuple[str, str]] = {
    # CRUD
    "find": (A_FIND, SCOPE_COLLECTION),
    "count": (A_FIND, SCOPE_COLLECTION),
    "distinct": (A_FIND, SCOPE_COLLECTION),
    # $out/$merge stages do their own write-action checks at stage time.
    "aggregate": (A_FIND, SCOPE_COLLECTION),
    "explain": (A_FIND, SCOPE_COLLECTION),
    "insert": (A_INSERT, SCOPE_COLLECTION),
    "update": (A_UPDATE, SCOPE_COLLECTION),
    "delete": (A_REMOVE, SCOPE_COLLECTION),
    "findAndModify": (A_UPDATE, SCOPE_COLLECTION),
    "findandmodify": (A_UPDATE, SCOPE_COLLECTION),
    # Cursor lifecycle
    "killCursors": (A_KILL_CURSORS, SCOPE_COLLECTION),
    # Index management
    "listIndexes": (A_LIST_INDEXES, SCOPE_COLLECTION),
    "createIndexes": (A_CREATE_INDEX, SCOPE_COLLECTION),
    "dropIndexes": (A_DROP_INDEX, SCOPE_COLLECTION),
    # DDL
    "create": (A_CREATE_COLLECTION, SCOPE_DATABASE),
    "drop": (A_DROP_COLLECTION, SCOPE_COLLECTION),
    "dropDatabase": (A_DROP_DATABASE, SCOPE_DATABASE),
    "renameCollection": (A_RENAME_COLL_SAME_DB, SCOPE_COLLECTION),
    "collMod": (A_COLL_MOD, SCOPE_COLLECTION),
    # Listings / stats
    "listCollections": (A_LIST_COLLECTIONS, SCOPE_DATABASE),
    "listDatabases": (A_LIST_DATABASES, SCOPE_CLUSTER),
    "dbStats": (A_DB_STATS, SCOPE_DATABASE),
    # MongoDB clients accept either case; the handler table aliases them
    # but the RBAC table previously only listed ``dbStats``, so a
    # lowercase request slipped past the privilege check (the dispatcher
    # silently exempts commands missing from this table).
    "dbstats": (A_DB_STATS, SCOPE_DATABASE),
    "collStats": (A_COLL_STATS, SCOPE_COLLECTION),
    # User management
    "createUser": (A_CREATE_USER, SCOPE_DATABASE),
    "dropUser": (A_DROP_USER, SCOPE_DATABASE),
    "dropAllUsersFromDatabase": (A_DROP_USER, SCOPE_DATABASE),
    "usersInfo": (A_VIEW_USER, SCOPE_DATABASE),
    "grantRolesToUser": (A_GRANT_ROLE, SCOPE_DATABASE),
    "revokeRolesFromUser": (A_REVOKE_ROLE, SCOPE_DATABASE),
    "rolesInfo": (A_VIEW_ROLE, SCOPE_DATABASE),
    "updateUser": (A_CHANGE_PASSWORD, SCOPE_DATABASE),
    # Custom roles — same database scope as user mgmt.
    "createRole": (A_CREATE_ROLE, SCOPE_DATABASE),
    "updateRole": (A_GRANT_ROLE, SCOPE_DATABASE),
    "dropRole": (A_DROP_ROLE, SCOPE_DATABASE),
    "dropAllRolesFromDatabase": (A_DROP_ROLE, SCOPE_DATABASE),
    "grantPrivilegesToRole": (A_GRANT_ROLE, SCOPE_DATABASE),
    "revokePrivilegesFromRole": (A_REVOKE_ROLE, SCOPE_DATABASE),
    "grantRolesToRole": (A_GRANT_ROLE, SCOPE_DATABASE),
    "revokeRolesFromRole": (A_REVOKE_ROLE, SCOPE_DATABASE),
    # Cluster / introspection
    "serverStatus": (A_SERVER_STATUS, SCOPE_CLUSTER),
    "hostInfo": (A_HOST_INFO, SCOPE_CLUSTER),
    "getCmdLineOpts": (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
    # ``getParameter`` exposes server-internal config (featureFlag state,
    # tunables) and was missing from this table — a cluster-info command
    # an unprivileged authenticated user could call. Reuse the same
    # cluster-monitor action as the other introspection commands.
    "getParameter": (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
    "getLog": (A_GET_LOG, SCOPE_CLUSTER),
    "currentOp": (A_INPROG, SCOPE_CLUSTER),
    "fsync": (A_FSYNC, SCOPE_CLUSTER),
    "profile": (A_ENABLE_PROFILER, SCOPE_DATABASE),
    # SecantusDB-extension prune commands reuse fsync's cluster-wide
    # privilege — both are admin-only operations against shared state.
    "secantusAdmin.pruneOplog": (A_FSYNC, SCOPE_CLUSTER),
    "secantusAdmin.pruneTtl": (A_FSYNC, SCOPE_CLUSTER),
}


# Commands that intentionally skip the RBAC check even when auth is on:
# cursor continuation (the cursor was already authorized at find/aggregate
# time), session-id administrivia (no real session state), and metadata
# commands the driver depends on at every connection.
_NO_PRIVILEGE_COMMANDS = frozenset(
    {
        "getMore",
        "endSessions",
        "startSession",
        "refreshSessions",
        "ping",
        "ismaster",
        "isMaster",
        "hello",
        "buildInfo",
        "buildinfo",
        "whatsmyuri",
        "saslStart",
        "saslContinue",
        "logout",
        "connectionStatus",
        # Aborting/committing transactions are no-ops in our surrogate;
        # gating them by a privilege would only confuse drivers that do
        # protocol negotiation.
        "abortTransaction",
        "commitTransaction",
    }
)


def _resource_for_command(
    name: str, doc: dict[str, Any], default_db: str
) -> tuple[str | None, bool]:
    """Resolve the target (db, cluster_flag) the action operates on.

    For collection-level commands, ``default_db`` is the connection's
    current ``$db`` and that's the resource. For ``listCollections`` /
    ``dropDatabase`` etc. the same applies. Cluster-level commands
    (``serverStatus``, ``listDatabases``) return ``(None, True)``.

    Some commands have their target db expressed in the request rather
    than in the connection's ``$db`` — for instance ``renameCollection``
    on the admin db with a ``renameCollection: "src.coll"`` namespace
    string. We pull those out specifically to avoid mis-attributing
    privileges to the admin db.
    """
    info = _COMMAND_ACTIONS.get(name)
    if info is None:
        return default_db, False
    _action, scope = info
    if scope == SCOPE_CLUSTER:
        return None, True
    # ``renameCollection: "src.coll"`` — the source db is the namespace
    # prefix, not the connection's $db.
    if name == "renameCollection":
        ns = doc.get("renameCollection")
        if isinstance(ns, str) and "." in ns:
            return ns.split(".", 1)[0], False
    return default_db, False


def command_name(doc: dict[str, Any]) -> str:
    return next(iter(doc), "")


# Commands the profiler skips. Handshake / session admin / cursor
# continuation / profile-itself produce noise out of proportion to their
# value. ``getLog`` would otherwise be self-amplifying as the dashboard
# polls it. Mongod skips a similar set.
_PROFILE_SKIP_COMMANDS = frozenset(
    {
        "hello",
        "isMaster",
        "ismaster",
        "ping",
        "buildInfo",
        "buildinfo",
        "whatsmyuri",
        "saslStart",
        "saslContinue",
        "logout",
        "connectionStatus",
        "startSession",
        "endSessions",
        "refreshSessions",
        "getMore",
        "killCursors",
        "profile",
    }
)


_PROFILE_OP_BUCKET: dict[str, str] = {
    "find": "query",
    "count": "query",
    "distinct": "query",
    "aggregate": "command",
    "insert": "insert",
    "update": "update",
    "delete": "remove",
    "findAndModify": "command",
    "findandmodify": "command",
}


def _profile_eligible_command(name: str, doc: dict[str, Any]) -> bool:
    """Return ``True`` if dispatch should time + maybe record this command."""
    if name in _PROFILE_SKIP_COMMANDS:
        return False
    # Recursion guard: any op against ``system.profile`` (write or read)
    # would itself produce a profile entry and recurse. Reads of the
    # profile collection in particular would otherwise cause writes that
    # show up on the next read, growing without bound until the user
    # spots it.
    coll = doc.get(name)
    return not (isinstance(coll, str) and coll == "system.profile")


def _profile_op_label(name: str) -> str:
    return _PROFILE_OP_BUCKET.get(name, "command")


def _profile_command_namespace(name: str, doc: dict[str, Any], db: str) -> str:
    """Best-effort ``ns`` field for a profile entry. ``db.coll`` or just ``db``."""
    coll_arg = doc.get(name)
    if isinstance(coll_arg, str) and coll_arg:
        return f"{db}.{coll_arg}"
    return db


def _profile_sanitize_command(doc: dict[str, Any]) -> dict[str, Any]:
    """Drop framing fields that aren't useful in a profile entry."""
    out = dict(doc)
    for key in ("$db", "$clusterTime", "lsid", "$readPreference"):
        out.pop(key, None)
    return out


def _maybe_record_profile(
    ctx: CommandContext,
    name: str,
    doc: dict[str, Any],
    result: dict[str, Any],
    start_ns: int,
) -> None:
    """Persist a ``system.profile`` entry if the per-DB level requires it.

    Errors during profile-write are swallowed: the user's command has
    already produced its result, and a profile-write failure shouldn't
    bleed into the response. Logged at WARNING for diagnosability.
    """
    db = ctx.db_name or "admin"
    try:
        settings = ctx.storage.get_profile(db)
    except Exception:
        return
    level = int(settings.get("level") or 0)
    if level == 0:
        return
    duration_ms = max(0, (_time.monotonic_ns() - start_ns) // 1_000_000)
    # ``settings.get(...) or default`` would replace legitimate zero
    # values with the default. Use the explicit-default form instead.
    slowms_raw = settings.get("slowms", 100)
    slowms = int(slowms_raw) if slowms_raw is not None else 100
    if level == 1 and duration_ms < slowms:
        return
    rate_raw = settings.get("sampleRate", 1.0)
    sample_rate = float(rate_raw) if rate_raw is not None else 1.0
    if sample_rate < 1.0 and _random.random() > sample_rate:
        return
    try:
        ctx.storage.ensure_profile_collection(db)
        client_addr = ""
        if ctx.connections is not None:
            for info in ctx.connections.snapshot():
                if info.conn_id == ctx.connection_id:
                    client_addr = f"{info.peer_addr[0]}:{info.peer_addr[1]}"
                    break
        user = None
        if ctx.connection_auth is not None and ctx.connection_auth.is_authenticated:
            principals = ctx.connection_auth.authenticated_principals
            if principals:
                u, d = principals[0]
                user = f"{u}@{d}"
        entry = {
            "ts": _dt.datetime.now(_dt.timezone.utc),
            "op": _profile_op_label(name),
            "ns": _profile_command_namespace(name, doc, db),
            "command": _profile_sanitize_command(doc),
            "millis": int(duration_ms),
            "ok": 1.0 if result.get("ok") == 1.0 else 0.0,
            "client": client_addr,
        }
        if user is not None:
            entry["user"] = user
        if result.get("ok") != 1.0 and "errmsg" in result:
            entry["errMsg"] = str(result["errmsg"])
            if "code" in result:
                entry["errCode"] = int(result["code"])
        ctx.storage.insert(db, "system.profile", [entry])
    except Exception as exc:  # pragma: no cover — defensive
        logger.warning("profile-write failed for %s: %s", db, exc)


# Read-concern levels mongod accepts. Anything else is a parse error
# (code 9, FailedToParse). Real mongod's enum: ``local``, ``available``,
# ``majority``, ``linearizable``, ``snapshot``. SecantusDB doesn't
# implement a per-level read path — every read sees the latest committed
# state — but we still validate the level so drivers that probe with
# garbage values (e.g. mongo-ruby-driver's
# ``read concern is not valid raises an exception`` test) get the same
# error shape they'd see against real mongod.
_VALID_READ_CONCERN_LEVELS = frozenset(
    {"local", "available", "majority", "linearizable", "snapshot"}
)

# MongoDB stable API: only version "1" exists. Anything else surfaces
# as ``APIVersionError`` (code 322), per the upstream
# ``mongo-ruby-driver`` test ``database_spec.rb:874`` which asserts
# exactly this code on ``apiVersion: 'does-not-exist'``.
_VALID_API_VERSIONS = frozenset({"1"})


def dispatch(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    name = command_name(doc)
    # Read-concern + apiVersion validation runs before every command
    # — they're cross-cutting concerns the wire layer should reject
    # uniformly, so invalid shapes don't silently pass through to
    # handlers that don't read those fields.
    rc = doc.get("readConcern")
    if isinstance(rc, Mapping) and "level" in rc:
        level = rc["level"]
        if not isinstance(level, str) or level not in _VALID_READ_CONCERN_LEVELS:
            return {
                "ok": 0.0,
                "errmsg": (f"Specified readConcern level {level!r} is not valid"),
                "code": 9,
                "codeName": "FailedToParse",
            }
    api_version = doc.get("apiVersion")
    if api_version is not None and api_version not in _VALID_API_VERSIONS:
        return {
            "ok": 0.0,
            "errmsg": (
                f"Provided apiVersion {api_version!r} is not supported. "
                f"Supported versions: {sorted(_VALID_API_VERSIONS)}"
            ),
            "code": 322,
            "codeName": "APIVersionError",
        }
    # Count every dispatched command — even unknown / unauth-rejected
    # ones — so serverStatus.network.numRequests reflects raw wire
    # traffic, not just the successful subset. Mongod's accounting is
    # the same.
    if ctx.metrics is not None:
        ctx.metrics.record_command(name)
    if ctx.connections is not None:
        ctx.connections.record_command(ctx.connection_id, name)
    # Implicit session registration: drivers that don't call
    # ``startSession`` explicitly still attach an ``lsid`` to every
    # command. Touching the registry on each lsid'd command keeps
    # the idle-TTL clock aligned with actual activity, mirroring
    # mongod's "session is alive while it's being used" rule.
    if ctx.sessions is not None:
        lsid = _lsid_bytes_from_arg(doc.get("lsid"))
        if lsid is not None:
            ctx.sessions.register(lsid)
    handler = _HANDLERS.get(name)
    if handler is None:
        return {
            "ok": 0.0,
            "errmsg": f"no such command: '{name}'",
            "code": 59,
            "codeName": "CommandNotFound",
        }
    if (
        ctx.require_auth
        and name not in _PRE_AUTH_COMMANDS
        and (ctx.connection_auth is None or not ctx.connection_auth.is_authenticated)
    ):
        return {
            "ok": 0.0,
            "errmsg": (f"command {name} requires authentication"),
            "code": 13,
            "codeName": "Unauthorized",
        }
    # RBAC privilege check. Active only when ``--auth`` is on; without
    # auth, any client can do anything (the legacy default-allow mode).
    # Pre-auth commands (handshake, SCRAM round-trip) and explicitly
    # exempt commands (cursor continuation, session admin) bypass the
    # check. Everything else needs an action grant from the principal's
    # role bindings.
    if (
        ctx.require_auth
        and ctx.connection_auth is not None
        and ctx.connection_auth.is_authenticated
        and name not in _NO_PRIVILEGE_COMMANDS
        and name not in _PRE_AUTH_COMMANDS
    ):
        info = _COMMAND_ACTIONS.get(name)
        if info is not None:
            action, scope = info
            target_db, cluster = _resource_for_command(name, doc, ctx.db_name or "admin")
            if not check_privilege(
                ctx.connection_auth.effective_roles,
                action,
                target_db=target_db,
                cluster=cluster,
                role_resolver=ctx.storage.get_role,
            ):
                return {
                    "ok": 0.0,
                    "errmsg": (
                        f"not authorized on {target_db or 'cluster'} to "
                        f"execute command (action: {action})"
                    ),
                    "code": 13,
                    "codeName": "Unauthorized",
                }
    # Failpoint match — short-circuit with ``errorCode`` before the
    # handler runs, or fall through and remember a ``writeConcernError``
    # to attach to the successful response. ``configureFailPoint``
    # itself is exempt: the test setup that installs the failpoint
    # would otherwise loop on its own configuration.
    failpoint_wce: dict[str, Any] | None = None
    failpoint_labels: tuple[str, ...] = ()
    if ctx.failpoints is not None and name != "configureFailPoint":
        match = ctx.failpoints.match(name)
        if match is not None:
            if match.error_code is not None:
                result: dict[str, Any] = {
                    "ok": 0.0,
                    "errmsg": "Failing command via 'failCommand' failpoint",
                    "code": match.error_code,
                    "codeName": _code_name_for(match.error_code),
                }
                if match.error_labels:
                    result["errorLabels"] = list(match.error_labels)
                return result
            if match.write_concern_error is not None:
                wce = dict(match.write_concern_error)
                wce.setdefault("errmsg", "failCommand failpoint")
                wce.setdefault("codeName", _code_name_for(int(wce.get("code", 0))))
                failpoint_wce = wce
                failpoint_labels = match.error_labels

    profile_eligible = _profile_eligible_command(name, doc)
    start_ns = _time.monotonic_ns() if profile_eligible else 0
    try:
        result = handler(doc, ctx)
    except _USER_FACING_EXCEPTIONS as exc:
        # Validation-class errors: messages are deliberately shaped to
        # match mongod, drivers parse them. Surface verbatim.
        result = {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": 14,
            "codeName": "TypeMismatch",
        }
    except Exception:
        # Anything else is an internal bug. Logging the full traceback
        # server-side preserves debuggability; returning a generic
        # message avoids leaking field paths, file paths, document
        # contents, or stack frames over the wire to an unauthenticated
        # peer.
        logger.exception(
            "internal error handling command %s (conn=%s, db=%s)",
            name,
            ctx.connection_id,
            ctx.db_name,
        )
        result = {
            "ok": 0.0,
            "errmsg": "internal server error",
            "code": 1,
            "codeName": "InternalError",
        }
    if profile_eligible:
        _maybe_record_profile(ctx, name, doc, result, start_ns)
    if failpoint_wce is not None and result.get("ok", 0.0):
        result["writeConcernError"] = failpoint_wce
        if failpoint_labels:
            result["errorLabels"] = list(failpoint_labels)
    return result
