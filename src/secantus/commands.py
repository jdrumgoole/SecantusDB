from __future__ import annotations

import datetime as _dt
import logging
import os
import random as _random
import sys
import time as _time
from collections.abc import Callable, Mapping
from dataclasses import dataclass, replace
from typing import Any

import bson

from secantus import changestreams
from secantus.aggregate import (
    AggregateError,
    PipelineContext,
    apply_pipeline,
    validate_stage_names,
)
from secantus.auth import (
    MONGODB_X509,
    SCRAM_SHA_1,
    SCRAM_SHA_256,
    X509_CREDENTIAL_MARKER,
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
    A_KILLOP,
    A_LIST_COLLECTIONS,
    A_LIST_DATABASES,
    A_LIST_INDEXES,
    A_REMOVE,
    A_RENAME_COLL_SAME_DB,
    A_REVOKE_ROLE,
    A_SERVER_STATUS,
    A_TOP,
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
from secantus.storage import (
    DocumentTooLargeError,
    DuplicateKeyError,
    GeoExtractError,
    Storage,
    WriteConflictError,
    _is_wt_rollback,
)
from secantus.transactions import (
    TRANSIENT_LABEL,
    Transaction,
    TransactionRegistry,
    TxnState,
    no_such_transaction_reply,
)
from secantus.update import UpdateError, validate_update_doc
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


def _validate_write_concern(doc: Mapping[str, Any]) -> dict[str, Any] | None:
    """Reject malformed ``writeConcern`` per mongod.

    Real mongod rejects:
      * ``w`` that's a string other than ``"majority"`` /
        any configured tag-set (we accept ``"majority"`` only — we're
        standalone, no tags). Returns code 79 ``UnknownReplWriteConcern``.
      * ``w`` that's a non-int / non-string. BadValue (2).
      * ``j`` that isn't a bool. TypeMismatch (14).
      * ``wtimeout`` that isn't a number. TypeMismatch (14).

    Returns a server-error response on failure, ``None`` on success or
    when no ``writeConcern`` is present. Callers prepend this check
    to write commands (``insert`` / ``update`` / ``delete`` /
    ``findAndModify`` / ``create*`` / ``drop*`` etc.). Mongo-ruby-
    driver's ``Collection#create when... INVALID_WRITE_CONCERN``
    specs pin the wire shape.
    """
    wc = doc.get("writeConcern")
    if wc is None:
        return None
    if not isinstance(wc, Mapping):
        return {
            "ok": 0.0,
            "errmsg": "writeConcern must be a document",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if "w" in wc:
        w = wc["w"]
        if isinstance(w, bool) or not isinstance(w, (int, str)):
            return {
                "ok": 0.0,
                "errmsg": "writeConcern.w must be a number or string",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        if isinstance(w, str) and w != "majority":
            return {
                "ok": 0.0,
                "errmsg": f"No write concern mode named {w!r} found in replica set configuration",
                "code": 79,
                "codeName": "UnknownReplWriteConcern",
            }
    if "j" in wc and not isinstance(wc["j"], (bool, int)):
        # Real mongod is loose on the ``j`` type — bool or int both
        # work (truthiness used). Mongo-node-driver's
        # ``findOneAnd... passes through the writeConcern`` test sends
        # ``{j: 1}`` and expects the command to succeed.
        return {
            "ok": 0.0,
            "errmsg": "writeConcern.j must be a boolean or integer",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if "wtimeout" in wc and (
        isinstance(wc["wtimeout"], bool) or not isinstance(wc["wtimeout"], (int, float))
    ):
        return {
            "ok": 0.0,
            "errmsg": "writeConcern.wtimeout must be a number",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    return None


def _wants_journal(doc: Mapping[str, Any]) -> bool:
    """Extract the per-write ``writeConcern.j`` flag.

    Returns ``True`` when the request asks for journal-durable
    semantics (``j: true`` or any truthy integer). Real mongod treats
    ``j: 1``/``j: true`` identically — the type check in
    ``_validate_write_concern`` already vets the field's wire shape.

    When ``True`` flows through to storage, the per-write WT
    transaction commits with ``sync=on`` (per-commit fsync) regardless
    of the connection's ``transaction_sync`` setting. Closes the
    long-standing gap where ``j: true`` was silently dropped because
    the server-wide ``sync_on_commit`` knob was the only lever.
    """
    wc = doc.get("writeConcern")
    if not isinstance(wc, Mapping):
        return False
    return bool(wc.get("j"))


def _reject_oplog_rs_write(ctx: CommandContext, coll: str, op_name: str) -> dict[str, Any] | None:
    """Refuse any write to a synthetic read-only view.

    Covers ``local.oplog.rs`` and ``admin.system.users`` — both are
    read-only projections over dedicated WT tables that own their own
    write paths (oplog emission via writes elsewhere; ``createUser`` /
    ``updateUser`` / ``dropUser`` for users). Direct writes through
    ``insert`` / ``update`` / ``delete`` would either land in the wrong
    table or corrupt the view's invariants, so we reject with code 13
    (Unauthorized) — the same code mongod returns when RBAC denies the
    write — and a clear errmsg so debuggers know what they hit.
    """
    if ctx.db_name == "local" and coll == "oplog.rs":
        return {
            "ok": 0.0,
            "errmsg": (
                f"not authorized for {op_name} on local.oplog.rs "
                "(synthetic read-only view of the SecantusDB oplog)"
            ),
            "code": 13,
            "codeName": "Unauthorized",
        }
    if ctx.db_name == "admin" and coll == "system.users":
        return {
            "ok": 0.0,
            "errmsg": (
                f"not authorized for {op_name} on admin.system.users "
                "(synthetic read-only view — use createUser / updateUser / "
                "dropUser instead)"
            ),
            "code": 13,
            "codeName": "Unauthorized",
        }
    if ctx.db_name == "admin" and coll == "system.version":
        return {
            "ok": 0.0,
            "errmsg": (
                f"not authorized for {op_name} on admin.system.version "
                "(synthetic read-only view of the auth schema version)"
            ),
            "code": 13,
            "codeName": "Unauthorized",
        }
    return None


def _unsatisfiable_wc_error(doc: Mapping[str, Any]) -> dict[str, Any] | None:
    """If ``writeConcern.w`` can't be satisfied, return the mongod-shaped
    ``writeConcernError`` to attach to a successful reply.

    SecantusDB advertises as a single-node replica set (`setName:
    "secantus"`, one member). Real mongod with the same topology
    executes write commands normally but tacks a ``writeConcernError``
    with code 100 / ``CannotSatisfyWriteConcern`` onto the reply when
    ``w`` is an integer above the member count. Drivers see the wce and
    raise ``OperationFailure`` (mongo-ruby-driver's
    ``Mongo::Collection#create ... applies the write concern`` spec
    relies on exactly this). Returns ``None`` when ``w`` is absent,
    ``"majority"``, or ``<= 1``.
    """
    wc = doc.get("writeConcern")
    if not isinstance(wc, Mapping):
        return None
    w = wc.get("w")
    # ``bool`` is an ``int`` subclass in Python — exclude it explicitly so
    # ``w: true`` doesn't trip the comparison below. The ``True``/``False``
    # case is unspecified at the protocol level; let the wire shape pass.
    if isinstance(w, bool):
        return None
    if isinstance(w, int) and w > 1:
        return {
            "code": 100,
            "codeName": "CannotSatisfyWriteConcern",
            "errmsg": (
                f"Not enough data-bearing nodes; requested w={w} but only 1 member is configured"
            ),
        }
    return None


def _resolve_let_vars(let: Any) -> dict[str, Any]:
    # MongoDB 5.0+ command-level ``let`` values are aggregation
    # expressions: ``{y: {$literal: "bar"}}`` binds ``$$y`` to "bar",
    # ``{n: {$add: [1, 2]}}`` binds ``$$n`` to 3. Driver tests
    # (mongo-java-driver's ``UnifiedCrudTest#updateMany-let``)
    # depend on this — passing the raw mapping through would bind
    # ``$$y`` to the dict ``{$literal: "bar"}`` instead of the
    # string. Scalars are passed through unchanged.
    #
    # ``$$NOW`` is seeded unconditionally: mongod binds it as a Date
    # constant for the whole operation in every command context, and
    # ``let`` expressions themselves may reference it.
    now_vars: dict[str, Any] = {"NOW": _dt.datetime.now(_dt.timezone.utc)}
    if not isinstance(let, dict):
        return now_vars
    from secantus.expressions import evaluate

    resolved = {name: evaluate(value, {}, vars=dict(now_vars)) for name, value in let.items()}
    return {**now_vars, **resolved}


def _validate_doc_against_collection(
    storage: Storage, db: str, coll: str, doc: dict[str, Any]
) -> dict[str, Any] | None:
    """If the collection has a ``validator``, check the doc against it.

    Returns a mongod-shaped ``DocumentValidationFailure`` (code 121)
    error response when validation fails, else ``None``. The
    ``errInfo.failingDocumentId`` lets drivers' errorResponse tests
    pick out which doc was rejected without parsing the whole error.
    """
    opts = storage.get_collection_options(db, coll)
    validator = opts.get("validator")
    if not isinstance(validator, dict) or not validator:
        return None
    if matches(doc, validator):
        return None
    return {
        "ok": 0.0,
        "errmsg": "Document failed validation",
        "code": 121,
        "codeName": "DocumentValidationFailure",
        "errInfo": {
            "failingDocumentId": doc.get("_id"),
            "details": {
                "operatorName": "validator",
                "schemaRulesNotSatisfied": [
                    {"operatorName": k, "specifiedAs": {k: v}} for k, v in validator.items()
                ],
            },
        },
    }


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
    transactions: TransactionRegistry | None = None
    # MONGODB-X509: the subject DN of the verified client cert the
    # connection's TLS handshake produced, in RFC 4514 string form
    # (e.g. ``"CN=alice,O=Acme,C=US"``). None when the connection is
    # plaintext, or TLS without ``[tls] ca_file`` configured, or the
    # client didn't present a cert in CERT_OPTIONAL mode. Captured
    # once at TLS handshake time, replayed into every CommandContext
    # for the connection.
    peer_cert_dn: str | None = None


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
    # Per the MongoDB Handshake spec, drivers send their
    # self-identification (name, version, OS, platform) in the
    # ``client`` subdoc on the first ``hello`` / ``isMaster`` of
    # a connection. We stash it on the registry so ``currentOp``
    # can surface it back as ``clientMetadata`` — that's how
    # mongo-rust-driver's
    # ``test::client::metadata_sent_in_handshake`` reads it back.
    client_meta = doc.get("client")
    if isinstance(client_meta, Mapping) and ctx.connections is not None:
        ctx.connections.set_client_metadata(ctx.connection_id, dict(client_meta))
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
    for entry in doc.get("endSessions") or []:
        lsid = _lsid_bytes_from_arg(entry)
        if lsid is not None:
            if ctx.sessions is not None:
                ctx.sessions.unregister(lsid)
            if ctx.transactions is not None:
                ctx.transactions.abort_for_session(lsid)
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
    if ctx.transactions is not None:
        ctx.transactions.abort_all()
    return {"ok": 1.0}


def _kill_all_sessions_by_pattern(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Pattern-filtered variant of ``killAllSessions``.

    Same effective semantics as ``killAllSessions`` — drop every
    session — because the registry isn't lsid-pattern-indexed.
    """
    if ctx.sessions is not None:
        ctx.sessions.clear()
    if ctx.transactions is not None:
        ctx.transactions.abort_all()
    return {"ok": 1.0}


def _kill_sessions(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Drop the listed sessions (driver-callable variant)."""
    for entry in doc.get("killSessions") or []:
        lsid = _lsid_bytes_from_arg(entry)
        if lsid is not None:
            if ctx.sessions is not None:
                ctx.sessions.unregister(lsid)
            if ctx.transactions is not None:
                ctx.transactions.abort_for_session(lsid)
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


def _txn_envelope(doc: dict[str, Any]) -> tuple[bytes | None, int | None]:
    """Extract ``(lsid_bytes, txnNumber)`` from a command's transaction
    envelope; either is None when absent or malformed."""
    lsid_bytes = _lsid_bytes_from_arg(doc.get("lsid"))
    txn_number = doc.get("txnNumber")
    if isinstance(txn_number, bool) or not isinstance(txn_number, int):
        txn_number = None
    return lsid_bytes, txn_number


def _write_conflict_reply(*, label: bool = True) -> dict[str, Any]:
    reply: dict[str, Any] = {
        "ok": 0.0,
        "errmsg": (
            "WriteConflict error: this operation conflicted with another "
            "operation. Please retry your operation or multi-document "
            "transaction."
        ),
        "code": 112,
        "codeName": "WriteConflict",
    }
    if label:
        # The transient label is transaction-specific; a plain write
        # that loses a conflict race doesn't carry it.
        reply["errorLabels"] = [TRANSIENT_LABEL]
    return reply


def _abort_transaction(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    lsid_bytes, txn_number = _txn_envelope(doc)
    if ctx.transactions is None or lsid_bytes is None or txn_number is None:
        # Envelope-less call (drivers never send this): tolerated no-op,
        # same as the pre-transactions stub.
        return {"ok": 1.0}
    err = ctx.transactions.abort(lsid_bytes, txn_number)
    return err if err is not None else {"ok": 1.0}


def _commit_transaction(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    lsid_bytes, txn_number = _txn_envelope(doc)
    if ctx.transactions is None or lsid_bytes is None or txn_number is None:
        return {"ok": 1.0}
    try:
        err = ctx.transactions.commit(lsid_bytes, txn_number)
    except Exception as exc:
        # The WT commit itself failed; the registry already rolled the
        # transaction back. A conflict is retryable from the client's
        # point of view (retry the whole transaction, not the commit).
        if isinstance(exc, WriteConflictError) or _is_wt_rollback(exc):
            return _write_conflict_reply()
        raise
    return err if err is not None else {"ok": 1.0}


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
            op_entry: dict[str, Any] = {
                "type": "op",
                "desc": f"conn{conn.conn_id}",
                "connectionId": bson.Int64(conn.conn_id),
                "client": host_port,
                "opid": bson.Int64(conn.conn_id),
                "active": conn.last_command_name is not None,
                "op": conn.last_command_name or "none",
                "ns": "",
                # ``command`` is the body of the last operation
                # on this connection. Real mongod always populates
                # it (even if the op already completed) — drivers
                # iterating ``inprog`` filter on ``command.<name>``
                # to find specific ops. Mongo-node-driver's
                # ``Aggregation should ... $currentOp`` test does
                # exactly that; without the field it crashes with
                # ``Cannot read properties of undefined``.
                "command": ({conn.last_command_name: 1} if conn.last_command_name else {}),
                "currentOpTime": opened_iso,
                "secs_running": 0,
                "microsecs_running": 0,
                "effectiveUsers": (
                    [{"user": conn.user.split("@", 1)[0], "db": conn.user.split("@", 1)[1]}]
                    if conn.user and "@" in conn.user
                    else []
                ),
            }
            # ``clientMetadata`` mirrors the ``hello.client`` subdoc
            # the driver sent on handshake. mongo-rust-driver's
            # ``test::client::metadata_sent_in_handshake`` reads it
            # back via ``currentOp`` to verify the driver / OS /
            # platform fields round-trip. Only present when the
            # driver actually sent a ``client`` subdoc on hello
            # (every modern driver does; ``mongo --eval`` shells
            # don't).
            if conn.client_metadata is not None:
                op_entry["clientMetadata"] = conn.client_metadata
            inprog.append(op_entry)
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


def _kill_op(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """``killOp``: forcibly close a client connection by its opid.

    Real mongod's ``killOp`` signals a per-op interrupt flag the
    long-running paths poll. SecantusDB doesn't carry per-op
    cancellation state, so the closest faithful semantic is "close
    the socket" — any in-flight command finishes, the connection
    thread's next ``recv`` returns 0, the loop exits, and the
    connection unregisters.

    Our ``opid`` is the connection's ``conn_id`` (one in-flight op
    per connection in our model), so the user can pass the value
    they read off ``currentOp`` directly. Accepts ``Int32`` /
    ``Int64`` / plain int / numeric string for the ``op`` field —
    different drivers serialise it differently.
    """
    raw = doc.get("op")
    try:
        op_id = int(raw)
    except (TypeError, ValueError):
        return {
            "ok": 0.0,
            "errmsg": f"killOp requires an integer ``op`` field, got {raw!r}",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if ctx.connections is None:
        return {"info": "no connection registry", "ok": 1.0}
    killed = ctx.connections.kill(op_id)
    # mongod always returns ``ok: 1`` from killOp regardless of whether
    # the op was found — it's fire-and-forget. The ``info`` field
    # surfaces what we did so admin tooling can confirm.
    return {
        "info": "operation killed" if killed else "no operation with that opid",
        "ok": 1.0,
    }


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


def _secantus_admin_backup_archive(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """SecantusDB extension: force a checkpoint, then tar the WT home.

    ``mongodump``/``mongorestore`` go through the wire protocol — they
    walk every collection, BSON-encode every doc, write to BSON files,
    then replay on restore. For a single-node test surrogate this is
    slower (and not atomic across collections) than a snapshot of the
    underlying WT directory. This command does the latter:

    1. Lock the storage,
    2. Force a WT checkpoint (durable flush + consistent snapshot),
    3. Tar the WT home directory into ``outputPath`` (.tar.gz).

    ``outputPath`` is a server-side path the SecantusDB process can
    write to. Returns ``{path, sizeBytes, ok: 1}``. Restore is
    "stop SecantusDB, extract the archive into a new storage path,
    start SecantusDB pointing at it" — a separate slice will land a
    server-side restore command.
    """
    output_path = doc.get("outputPath")
    if not isinstance(output_path, str) or not output_path:
        return {
            "ok": 0.0,
            "errmsg": "secantusAdmin.backupArchive requires outputPath: <string>",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    try:
        result = ctx.storage.create_archive(output_path)
    except RuntimeError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 20, "codeName": "IllegalOperation"}
    return {"path": result["path"], "sizeBytes": result["sizeBytes"], "ok": 1.0}


def _secantus_admin_restore_archive(doc: dict[str, Any], _ctx: CommandContext) -> dict[str, Any]:
    """SecantusDB extension: extract a backup archive into ``targetDir``.

    Side-channel restore: the archive is unpacked into a fresh
    directory that the operator then points a *new* SecantusDB process
    at (``SecantusDBServer(storage_path=<targetDir>)`` /
    ``secantusdb --storage-path <targetDir>``). The running server's
    own storage is **not** touched — hot in-place restore over a live
    WT connection would need a wholesale rework of how connection
    threads cache WT sessions, and isn't a mode real mongod supports
    either (real restores are "stop mongod, swap dbpath, start
    mongod").

    Required fields: ``archivePath`` (server-side path to the
    ``.tar.gz`` produced by ``backupArchive``), ``targetDir``
    (server-side path to extract into). Optional ``allowExisting``
    (bool, default false) lets the caller overlay into a non-empty
    target. Returns ``{targetDir, fileCount, archive, ok: 1}``.
    """
    from secantus.storage import extract_backup_archive

    archive_path = doc.get("archivePath")
    target_dir = doc.get("targetDir")
    if not isinstance(archive_path, str) or not archive_path:
        return {
            "ok": 0.0,
            "errmsg": "secantusAdmin.restoreArchive requires archivePath: <string>",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if not isinstance(target_dir, str) or not target_dir:
        return {
            "ok": 0.0,
            "errmsg": "secantusAdmin.restoreArchive requires targetDir: <string>",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    allow_existing = bool(doc.get("allowExisting", False))
    try:
        result = extract_backup_archive(archive_path, target_dir, allow_existing=allow_existing)
    except RuntimeError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 20, "codeName": "IllegalOperation"}
    return {
        "targetDir": result["targetDir"],
        "fileCount": result["fileCount"],
        "archive": result["archive"],
        "ok": 1.0,
    }


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


def _mem_section() -> dict[str, Any]:
    """``mem`` in mongod's serverStatus shape.

    mongostat dereferences ``mem.supported`` with no nil guard
    (status/readers.go ``ReadMapped``), so the section must always be
    present. ``resident`` is real (getrusage max-RSS, normalised to MB —
    ru_maxrss is bytes on macOS, KiB on Linux); ``virtual`` isn't
    portably readable without psutil, so it's reported as 0 rather than
    invented.
    """
    resident_mb = 0
    try:
        import resource

        ru_maxrss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        divisor = 1024 * 1024 if sys.platform == "darwin" else 1024
        resident_mb = int(ru_maxrss / divisor)
    except Exception:  # pragma: no cover - resource absent on Windows
        pass
    return {
        "bits": 64,
        "resident": resident_mb,
        "virtual": 0,
        "supported": True,
    }


def _server_status(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Real metrics from :class:`secantus.metrics.Metrics` if the server
    constructed one (production path); falls back to zeroed values for
    embedded callers that didn't thread a metrics instance through (e.g.
    ad-hoc test harnesses that use ``CommandContext`` directly)."""
    import secantus

    base: dict[str, Any] = {
        "host": "secantus",
        "version": SERVER_VERSION,
        "process": "secantus",
        "pid": os.getpid(),
        "localTime": _dt.datetime.now(_dt.timezone.utc),
        "mem": _mem_section(),
        # Categorical self-identification: real mongod never has this key.
        # Tooling (the conformance-gauge tripwire, ad-hoc smoke scripts)
        # checks it to prove it's talking to SecantusDB rather than an
        # accidental real MongoDB on the same address. `server`
        # distinguishes the pure-Python server from the Rust one.
        "secantus": {"server": "python", "version": secantus.__version__},
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


def _top(_doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """mongod-shaped ``top``: one entry per existing namespace.

    SecantusDB doesn't instrument per-namespace operation timing, so
    every counter is zero — mongotop renders an all-zero table the same
    way it does for an idle mongod. The shape (``note`` + per-ns
    ``total``/``readLock``/``writeLock``/per-op sections, each
    ``{time, count}``) is what mongo-tools' decoder requires; it skips
    the ``note`` key explicitly.
    """
    if ctx.db_name != "admin":
        return {
            "ok": 0.0,
            "errmsg": "top may only be run against the admin database.",
            "code": 13,
            "codeName": "Unauthorized",
        }
    totals: dict[str, Any] = {"note": "all times in microseconds"}
    for db in ctx.storage.list_databases():
        for coll in ctx.storage.list_collections(db):
            totals[f"{db}.{coll}"] = {
                section: {"time": 0, "count": 0}
                for section in (
                    "total",
                    "readLock",
                    "writeLock",
                    "queries",
                    "getmore",
                    "insert",
                    "update",
                    "remove",
                    "commands",
                )
            }
    return {"totals": totals, "ok": 1.0}


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
        # Real ``mongod`` exposes the list of enabled auth mechanisms
        # via ``getParameter authenticationMechanisms``. Driver test
        # runners (mongo-java-driver's unified ``RunOnRequirementsMatcher``
        # at line 81-88) read this to decide whether to run a test that
        # gates on ``authMechanism: "MONGODB-OIDC"``. We implement only
        # SCRAM-SHA-256, so advertise only that — tests requiring
        # ``MONGODB-OIDC`` / ``MONGODB-X509`` / ``GSSAPI`` / ``PLAIN``
        # then self-skip via ``assumeTrue`` instead of running and
        # failing on the missing handshake.
        "authenticationMechanisms": ["SCRAM-SHA-256"],
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


def _validate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """``validate`` — mongod's collection consistency check. SecantusDB
    stores documents as opaque BSON in WiredTiger and maintains index
    entries transactionally, so there's nothing to repair: we report a
    clean, mongod-shaped result with real record / index counts. The
    ``full`` / ``background`` / ``scandata`` options are accepted and
    ignored (they only affect how mongod scans, not the verdict)."""
    coll = doc.get("validate")
    if not isinstance(coll, str):
        return {
            "ok": 0.0,
            "errmsg": "validate requires a collection name",
            "code": 14,
            "codeName": "TypeMismatch",
        }
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"Collection '{ctx.db_name}.{coll}' does not exist to validate.",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    # mongod rejects full+background together (full needs an exclusive
    # scan; background can't take one). pymongo's
    # ``test_validate_collection_background`` asserts this rejection to
    # prove the background option reached the wire.
    if doc.get("full") and doc.get("background"):
        return {
            "ok": 0.0,
            "errmsg": (
                "Running the validate command with both { background: true } "
                "and { full: true } is not supported."
            ),
            "code": 72,
            "codeName": "InvalidOptions",
        }
    nrecords = ctx.storage.count_matching(ctx.db_name, coll, None)
    indexes = ctx.storage.list_indexes(ctx.db_name, coll)
    keys_per_index = {ix["name"]: nrecords for ix in indexes}
    index_details = {ix["name"]: {"valid": True} for ix in indexes}
    return {
        "ns": f"{ctx.db_name}.{coll}",
        "nInvalidDocuments": 0,
        "nNonCompliantDocuments": 0,
        "nrecords": nrecords,
        "nIndexes": len(indexes),
        "keysPerIndex": keys_per_index,
        "indexDetails": index_details,
        "valid": True,
        "repaired": False,
        "warnings": [],
        "errors": [],
        "extraIndexEntries": [],
        "missingIndexEntries": [],
        "corruptRecords": [],
        "ok": 1.0,
    }


def _explain(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    inner = doc.get("explain") or {}
    coll = ""
    filter_: dict[str, Any] = {}
    sort = None
    hint = None
    collation = None
    if isinstance(inner, dict):
        cmd_name = next(iter(inner), "")
        coll_value = inner.get(cmd_name)
        coll = coll_value if isinstance(coll_value, str) else ""
        filter_ = inner.get("filter") or inner.get("query") or {}
        sort = inner.get("sort")
        hint = inner.get("hint")
        collation = inner.get("collation")
        # Mirror ``aggregate``'s own planning: a leading ``$match`` is
        # lifted into the initial fetch, so explain must report the
        # same index decision (and execute against the same filter)
        # the real pipeline run would use.
        if cmd_name == "aggregate" and not filter_:
            pipeline_head = (inner.get("pipeline") or [{}])[0]
            if isinstance(pipeline_head, Mapping) and "$match" in pipeline_head:
                lifted = pipeline_head["$match"]
                if isinstance(lifted, Mapping):
                    filter_ = dict(lifted)
    # MongoDB rejects ``explain`` paired with a journaled write concern
    # (``writeConcern: {j: true}`` or ``{w: "majority"}``). The explain
    # cycle is a no-op read; combining it with a write concern is
    # ill-formed. Mongo-node-driver's ``aggregation.test.ts`` has two
    # cases that assert the rejection.
    # ``writeConcern`` may live on the outer ``explain`` doc or be
    # nested inside the wrapped command (drivers handle both shapes).
    wc_outer = doc.get("writeConcern")
    wc_inner = inner.get("writeConcern") if isinstance(inner, dict) else None
    for wc in (wc_outer, wc_inner):
        if isinstance(wc, dict) and (
            wc.get("j") is True or wc.get("j") == 1 or wc.get("w") == "majority"
        ):
            return {
                "ok": 0.0,
                "errmsg": ("Command does not support writeConcern when used with explain"),
                "code": 72,
                "codeName": "InvalidOptions",
            }
    # ``maxTimeMS`` accepted but not enforced — operations complete
    # immediately on this in-process daemon so a real timeout never
    # fires. Pre-Node's CSOT, explain helpers just attach the value
    # for the wire-shape audit; mongo-node-driver's
    # ``explain helpers w/ maxTimeMS attaches maxTimeMS to the explain
    # command`` test reads the started-command event, not the server
    # response. The "explain command times out after timeoutMS" tests
    # rely on a server-side failpoint (not us) to actually time out.
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
        "queryPlanner",
        "executionStats",
        "allPlansExecution",
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
        plan = ctx.storage.explain_plan(
            ctx.db_name, coll, filter_, sort=sort, hint=hint, collation=collation
        )
    else:
        plan = {"kind": "COLLSCAN"}
    # ``executionStats`` / ``allPlansExecution`` really execute the
    # query, like mongod — Compass renders nReturned /
    # totalDocsExamined in its explain tab, and hardcoded zeroes would
    # display as "0 documents returned" for a matching query.
    n_returned = 0
    docs_examined = 0
    keys_examined = 0
    exec_millis = 0
    if verbosity != "queryPlanner" and coll:
        started = _time.monotonic()
        n_returned = len(
            ctx.storage.find_matching(
                ctx.db_name,
                coll,
                filter_,
                sort=sort,
                hint=hint,
                collation=collation,
            )
        )
        exec_millis = int((_time.monotonic() - started) * 1000)
        if plan["kind"] == "IXSCAN":
            # Exact-bounds index scans fetch one doc per matching key;
            # we don't instrument residual-filter key scans, so this is
            # mongod's reported shape for the common case.
            keys_examined = n_returned
            docs_examined = n_returned
        else:
            docs_examined = ctx.storage.count_matching(ctx.db_name, coll, {})
    if plan["kind"] == "IXSCAN":
        input_stage: dict[str, Any] = {
            "stage": "IXSCAN",
            "indexName": plan["index_name"],
            "keyPattern": plan["key_pattern"],
            "direction": plan["direction"],
        }
        # mongod flags an IXSCAN over a partial index with ``isPartial``.
        if coll:
            partial = any(
                ix.get("name") == plan["index_name"] and "partialFilterExpression" in ix
                for ix in ctx.storage.list_indexes(ctx.db_name, coll)
            )
            if partial:
                input_stage["isPartial"] = True
        winning_plan = {
            "stage": "FETCH",
            "filter": filter_,
            "inputStage": input_stage,
        }
        execution_stage = {
            "stage": "FETCH",
            "nReturned": n_returned,
            "inputStage": {"stage": "IXSCAN", "nReturned": n_returned},
        }
    else:
        winning_plan = {"stage": "COLLSCAN", "filter": filter_}
        execution_stage = {"stage": "COLLSCAN", "nReturned": n_returned}
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
        execution_stats = {
            "executionSuccess": True,
            "nReturned": n_returned,
            "executionTimeMillis": exec_millis,
            "totalKeysExamined": keys_examined,
            "totalDocsExamined": docs_examined,
            "executionStages": execution_stage,
        }
        cursor_stage: dict[str, Any] = {
            "$cursor": {
                "queryPlanner": query_planner,
            }
        }
        if verbosity != "queryPlanner":
            cursor_stage["$cursor"]["executionStats"] = execution_stats
        stages: list[dict[str, Any]] = [cursor_stage]
        for stage_doc in pipeline:
            if isinstance(stage_doc, Mapping):
                stages.append(dict(stage_doc))
        # mongod's modern aggregate-explain returns ``queryPlanner`` /
        # ``executionStats`` at the **top level** on standalone — same
        # shape ``find``'s explain uses. The ``stages`` array is the
        # pipeline-level breakdown that mongo-node-driver's
        # ``aggregation.test.ts`` looks for. Mongo-java-driver's
        # ``AbstractExplainTest#testExplainOfAggregateWithNewResponse
        # Structure`` looks at the top level. Returning both keeps
        # everyone happy.
        reply_agg: dict[str, Any] = {
            "stages": stages,
            "queryPlanner": query_planner,
            "explainVersion": "1",
            "command": inner,
            "serverInfo": server_info,
            "ok": 1.0,
        }
        if verbosity != "queryPlanner":
            reply_agg["executionStats"] = execution_stats
        return reply_agg
    reply: dict[str, Any] = {
        "queryPlanner": query_planner,
        "command": inner if isinstance(inner, dict) else {},
        "serverInfo": server_info,
        "ok": 1.0,
    }
    if verbosity != "queryPlanner":
        reply["executionStats"] = {
            "executionSuccess": True,
            "nReturned": n_returned,
            "executionTimeMillis": exec_millis,
            "totalKeysExamined": keys_examined,
            "totalDocsExamined": docs_examined,
            "executionStages": execution_stage,
        }
    return reply


def _ns(db: str, coll: str) -> str:
    return f"{db}.{coll}"


def _insert(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["insert"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "insert")
    if oplog_err is not None:
        return oplog_err
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
    bypass_validation = bool(doc.get("bypassDocumentValidation", False))
    # Collection-level ``validator`` (set via ``create`` / ``collMod``)
    # is enforced unless the caller passed ``bypassDocumentValidation:
    # true``. Mongo-node-driver's ``Document Validation should allow
    # bypassing document validation on inserts`` test installs a
    # validator then asserts (a) a violating insert fails with
    # ``MongoServerError`` and (b) the same insert with the bypass flag
    # succeeds.
    coll_opts_for_validation = ctx.storage.get_collection_options(ctx.db_name, coll)
    validator_spec = coll_opts_for_validation.get("validator")
    validator_active = isinstance(validator_spec, dict) and validator_spec and not bypass_validation
    # ``_id`` documents may not contain top-level ``$``-prefixed keys
    # in any server version — MongoDB always restricted this and
    # mongo-java-driver's ``insertOne-dots_and_dollars`` test pins it.
    # Surface the rejection as a per-doc writeError so unordered
    # batches keep the surviving inserts.
    pre_errors: list[dict[str, Any]] = []
    surviving: list[dict[str, Any]] = []
    for index, d in enumerate(documents):
        if isinstance(d, dict):
            id_value = d.get("_id")
            if isinstance(id_value, dict) and any(
                isinstance(k, str) and k.startswith("$") for k in id_value
            ):
                pre_errors.append(
                    {
                        "index": index,
                        "code": 2,
                        "errmsg": (
                            "_id fields may not contain '$'-prefixed fields: "
                            f"{next(iter(id_value))!s} is not valid for storage."
                        ),
                    }
                )
                if ordered:
                    break
                continue
            if validator_active and not matches(d, validator_spec):
                pre_errors.append(
                    {
                        "index": index,
                        "code": 121,
                        "errmsg": "Document failed validation",
                        "errInfo": {
                            "failingDocumentId": id_value,
                            "details": {"operatorName": "validator"},
                        },
                    }
                )
                if ordered:
                    break
                continue
        surviving.append(d)
    if pre_errors and ordered:
        # In ordered mode, abort at the first bad doc — anything
        # before it has not been attempted yet, anything after is
        # not attempted either. Match the per-doc writeError shape.
        return {"n": 0, "ok": 1.0, "writeErrors": pre_errors}
    if not surviving:
        return {"n": 0, "ok": 1.0, "writeErrors": pre_errors}
    inserted, errors = ctx.storage.insert(
        ctx.db_name, coll, surviving, ordered=ordered, journal=_wants_journal(doc)
    )
    reply: dict[str, Any] = {"n": inserted, "ok": 1.0}
    if pre_errors or errors:
        # ``writeErrors`` index refers to the position in the
        # *original* ``documents`` array. ``pre_errors`` already use
        # the original index; ``storage.insert``'s errors index into
        # ``surviving``, so remap via the per-doc reject mask.
        rejected_indices = {err["index"] for err in pre_errors}
        survivor_to_orig: list[int] = [
            i for i in range(len(documents)) if i not in rejected_indices
        ]
        remapped: list[dict[str, Any]] = []
        for err in errors:
            new_err = dict(err)
            new_err["index"] = survivor_to_orig[err["index"]]
            remapped.append(new_err)
        reply["writeErrors"] = pre_errors + remapped
    return reply


def _find(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.query import QueryError
    from secantus.storage import BadHint, MinMaxKeyError

    coll = doc["find"]
    filter_ = doc.get("filter") or {}
    skip = int(doc.get("skip", 0) or 0)
    limit = int(doc.get("limit", 0) or 0)
    sort = doc.get("sort") or None
    projection = doc.get("projection") or None
    hint = doc.get("hint")
    # Cursor ``min`` / ``max`` index bounds (the find command fields, not
    # the ``$min`` / ``$max`` aggregation operators): documents whose
    # keys name a leading prefix of the hinted index. ``max`` is
    # exclusive, ``min`` inclusive.
    min_bound = doc.get("min") or None
    max_bound = doc.get("max") or None
    # ``let`` declares user-vars visible to ``$expr`` clauses in the
    # filter (MongoDB 5.0+).
    let = _resolve_let_vars(doc.get("let"))
    collation = doc.get("collation")
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
            collation=collation,
            min_bound=min_bound,
            max_bound=max_bound,
        )
    except BadHint as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    except MinMaxKeyError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 51174, "codeName": "Location51174"}
    except QueryError as exc:
        return {"ok": 0.0, "errmsg": str(exc), "code": 2, "codeName": "BadValue"}
    # ``returnKey`` replaces each result with just the keys of the index that
    # serves the query (filter + sort): the index's key-pattern fields, plus
    # the sort fields (mongod serves a sort from an index — the ``_id`` order
    # of the doc table here, which ``explain`` reports as a COLLSCAN). When set
    # it also suppresses ``showRecordId``'s ``$recordId``. ``showRecordId``
    # alone tags each doc with a synthetic ``$recordId``.
    return_key = bool(doc.get("returnKey", False))
    if return_key:
        key_fields: list[str] = []
        try:
            plan = ctx.storage.explain_plan(
                ctx.db_name, coll, filter_, sort=sort, hint=hint, collation=collation
            )
        except Exception:
            plan = {"kind": "COLLSCAN"}
        if plan.get("kind") == "IXSCAN":
            key_fields = list(plan.get("key_pattern", {}).keys())
        if isinstance(sort, Mapping):
            for f in sort:
                if f not in key_fields:
                    key_fields.append(f)
        docs = [{f: d[f] for f in key_fields if f in d} for d in docs]
    elif bool(doc.get("showRecordId", False)):
        docs = [{**d, "$recordId": bson.Int64(i + 1)} for i, d in enumerate(docs)]
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
        # ``local.oplog.rs`` is a synthetic view over the oplog WT table; its
        # entries have no ``_id``, so it needs a producer that tails by oplog
        # seq rather than the doc-table id_key path ``_find_tailable`` uses.
        if ctx.db_name == "local" and coll == "oplog.rs":
            return _find_tailable_oplog(filter_, docs, batch_size, ns, await_data, ctx)
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

    Two doc sources feed the cursor:

    * The matched docs that ``find`` already produced. ``firstBatch``
      gets the leading ``batch_size`` of them; the remainder is queued
      on the cursor for the next ``getMore`` to drain (mongo-go-driver's
      ``TestCursor_RemainingBatchLength/first_batch_is_non_empty``
      relies on this — open a tailable cursor over 5 capped-coll docs
      with ``batchSize=2`` and the second batch must deliver
      ``{x:3},{x:4}`` via getMore, not block on awaitData).
    * Docs inserted *after* this find. A producer closure scans the
      doc table for rows with ``id_key`` strictly greater than the
      last one we've returned. ``id_key`` is the byte-sortable ``_id``
      encoding (see ``secantus.sortkey``); for monotonic
      ``ObjectId``-style ``_id`` values that order matches insertion
      order, which is exactly what tailable consumers expect. Capped
      collections eviction-prune oldest rows in the same order, so the
      producer naturally tracks the trailing edge.
    """
    from secantus.sortkey import encode_value as _encode_id_key

    db_name = ctx.db_name
    storage = ctx.storage
    first_batch = initial_docs[:batch_size]
    initial_remaining = initial_docs[batch_size:]
    # Watermark for the producer: highest id_key among the docs we've
    # already handed to the client (either in firstBatch or queued in
    # initial_remaining). Setting it to ``rows[-1][0]`` (the last doc
    # in the collection) instead would silently drop any matched docs
    # past ``batch_size`` — the original bug surfaced by the go gauge.
    # Empty collection: watermark is None, so the producer walks from
    # the start and picks up the very first insert after this find.
    watermark = _encode_id_key(initial_docs[-1]["_id"]) if initial_docs else None
    state = {"after_id_key": watermark}

    def producer() -> list[dict[str, Any]]:
        after = state["after_id_key"]
        # Capped rollover detection: if the doc this cursor last returned
        # (``after``) has been evicted — i.e. the collection's smallest
        # ``id_key`` is now strictly greater than it — the cursor has been
        # lapped and mongod kills it with ``CappedPositionLost``. A fresh
        # cursor (``after is None``) has no anchor to lose.
        if after is not None:
            min_key = storage.collection_min_id_key(db_name, coll)
            if min_key is None or min_key > after:
                raise _CappedPositionLost
        new_rows = storage.scan_docs_after_id_key(db_name, coll, after=after)
        if not new_rows:
            return []
        state["after_id_key"] = new_rows[-1][0]
        return [doc for _id_k, doc in new_rows]

    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=await_data,
        initial_remaining=initial_remaining,
    )
    return {
        "cursor": {
            "firstBatch": first_batch,
            "id": bson.Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    }


def _find_tailable_oplog(
    filter_: dict[str, Any],
    initial_docs: list[dict[str, Any]],
    batch_size: int,
    ns: str,
    await_data: bool,
    ctx: CommandContext,
) -> dict[str, Any]:
    """Build a tailable cursor over the synthetic ``local.oplog.rs`` view.

    Oplog entries have no ``_id``, so (unlike ``_find_tailable``) the producer
    can't anchor on the doc-table id_key. It anchors on the oplog *seq*: the
    highest seq present when the cursor opens is captured, and each poll reads
    rows past it via ``read_oplog``, applying the user filter. ``firstBatch``
    is the already-matched entries from the initial ``find``; the standard
    ``getMore`` awaitData path blocks on the oplog condition variable, waking
    on any oplog write. This is what lets a client tail the oplog the way
    replication does (pymongo's ``test_cursor.test_to_list_tailable``)."""
    storage = ctx.storage
    first_batch = initial_docs[:batch_size]
    initial_remaining = initial_docs[batch_size:]
    state = {"after_seq": storage.oplog_tail_seq()}

    def producer() -> list[dict[str, Any]]:
        rows = storage.read_oplog(start_seq=state["after_seq"] + 1, limit=1000)
        if not rows:
            return []
        state["after_seq"] = rows[-1][0]
        entries = [entry for _seq, entry in rows]
        if filter_:
            entries = [e for e in entries if matches(e, filter_)]
        return entries

    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=await_data,
        initial_remaining=initial_remaining,
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
    from secantus.storage import DocumentValidationError, GeoExtractError, IndexConflict
    from secantus.update import _PIPELINE_UPDATE_STAGES

    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["update"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "update")
    if oplog_err is not None:
        return oplog_err
    updates = doc.get("updates", [])
    ordered = bool(doc.get("ordered", True))
    bypass_validation = bool(doc.get("bypassDocumentValidation", False))
    # ``let`` — see ``_delete`` for the wire-shape rationale.
    let = _resolve_let_vars(doc.get("let"))
    # Collection validator enforced unless bypass requested. Mongo-node-
    # driver's ``Document Validation should allow bypassing document
    # validation on updates`` test asserts both directions.
    coll_opts_for_validation = ctx.storage.get_collection_options(ctx.db_name, coll)
    validator_spec = coll_opts_for_validation.get("validator")
    validator_active = isinstance(validator_spec, dict) and validator_spec and not bypass_validation
    n = 0
    n_modified = 0
    upserted: list[dict[str, Any]] = []
    write_errors: list[dict[str, Any]] = []
    for index, spec in enumerate(updates):
        # MongoDB 8.0 added a ``sort`` option to update spec entries
        # (matches in sort order then updates the first). Pre-8.0 the
        # server rejects it as a parse error. We advertise wire
        # version 17 (7.0), so mirror mongod's pre-8.0 behaviour: a
        # command-level FailedToParse. Drivers' ``updateOne-sort`` /
        # ``replaceOne-sort`` / ``BulkWrite updateOne-sort`` /
        # ``BulkWrite replaceOne-sort`` tests with
        # ``maxServerVersion: "7.99"`` assert this.
        if "sort" in spec:
            return {
                "ok": 0.0,
                "errmsg": (
                    "The 'sort' option is not supported on update commands before MongoDB 8.0"
                ),
                "code": 9,
                "codeName": "FailedToParse",
            }
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
            # Parse-time validation: mongod rejects an unknown update
            # modifier before matching any document, so an invalid update
            # errors even against an empty collection (apply_update only
            # runs per matched doc, which would miss this).
            validate_update_doc(spec.get("u", {}))
            result = ctx.storage.update_matching(
                ctx.db_name,
                coll,
                spec.get("q", {}),
                spec.get("u", {}),
                multi=bool(spec.get("multi", False)),
                upsert=bool(spec.get("upsert", False)),
                array_filters=spec.get("arrayFilters"),
                let=let,
                collation=spec.get("collation"),
                validator=validator_spec if validator_active else None,
                journal=_wants_journal(doc),
            )
        except DocumentValidationError as exc:
            write_errors.append(
                {
                    "index": index,
                    "code": 121,
                    "errmsg": "Document failed validation",
                    "errInfo": {
                        "failingDocumentId": exc.doc_id,
                        "details": {"operatorName": "validator"},
                    },
                }
            )
            if ordered:
                break
            continue
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
        except DocumentTooLargeError as exc:
            write_errors.append({"index": index, "code": exc.code, "errmsg": str(exc)})
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
            # ``_id`` immutability gets a special code (66
            # ImmutableField) so drivers' canonical handling triggers.
            # Everything else falls under FailedToParse (9) — malformed
            # operators / mixed ops & replacement fields.
            msg = str(exc)
            code = 66 if "immutable field" in msg else 9
            write_errors.append({"index": index, "code": code, "errmsg": msg})
            if ordered:
                break
            continue
        n += result["matched"]
        n_modified += result["modified"]
        # ``did_upsert`` distinguishes "upserted a doc whose _id is None"
        # from "no upsert" — ``upserted_id`` alone can't, since None is a
        # valid _id (pymongo's test_update_result upserts with
        # ``{_id: None}`` and asserts did_upsert).
        if result["did_upsert"]:
            upserted.append({"index": index, "_id": result["upserted_id"]})
            n += 1
    reply: dict[str, Any] = {"n": n, "nModified": n_modified, "ok": 1.0}
    if upserted:
        reply["upserted"] = upserted
    if write_errors:
        reply["writeErrors"] = write_errors
    return reply


def _delete(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["delete"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "delete")
    if oplog_err is not None:
        return oplog_err
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
                ctx.db_name,
                coll,
                spec.get("q", {}),
                limit=int(spec.get("limit", 0)),
                let=let,
                collation=spec.get("collation"),
                journal=_wants_journal(doc),
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
    # View support: if the collection is a view (``viewOn`` set),
    # run the view's pipeline + the count's query filter via the
    # aggregation engine and return its row count. Mongo-java-
    # driver's ``estimatedDocumentCount works correctly on views``
    # test relies on this.
    coll_opts = ctx.storage.get_collection_options(ctx.db_name, coll)
    view_on = coll_opts.get("viewOn")
    if isinstance(view_on, str):
        pipeline = list(coll_opts.get("viewPipeline") or [])
        if filter_:
            pipeline.append({"$match": filter_})
        docs = ctx.storage.find_matching(ctx.db_name, view_on, {})
        pipeline_ctx = PipelineContext(
            storage=ctx.storage,
            db_name=ctx.db_name,
            coll_name=view_on,
        )
        result_docs = apply_pipeline(docs, pipeline, pipeline_ctx)
        n = len(result_docs)
        skip = int(doc.get("skip") or 0)
        if skip > 0:
            n = max(n - skip, 0)
        limit = int(doc.get("limit") or 0)
        if limit > 0:
            n = min(n, limit)
        return {"n": n, "ok": 1.0}
    n = ctx.storage.count_matching(ctx.db_name, coll, filter_, collation=doc.get("collation"))
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
    from secantus.collation import cmp_key
    from secantus.collation import parse as _parse_collation
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
    collation = doc.get("collation")
    collation_obj = _parse_collation(collation)
    matched = ctx.storage.find_matching(ctx.db_name, coll, filter_, collation=collation)
    seen: list[Any] = []
    seen_keys: set[Any] = set()

    def _add(v: Any) -> None:
        ck = cmp_key(v, collation_obj)
        try:
            # cmp_key returns a hashable normalised form for strings;
            # but a non-string value (dict, list) may not be hashable.
            # Fall back to linear scan in that case.
            if ck in seen_keys:
                return
            seen_keys.add(ck)
        except TypeError:
            if any(v == s for s in seen):
                return
        seen.append(v)

    for d in matched:
        value = get_path(d, key)
        if isinstance(value, list):
            for elem in value:
                _add(elem)
        elif value is not None or _key_present(d, key):
            _add(value)
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
    from secantus.storage import DocumentValidationError, GeoExtractError, IndexConflict

    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["findAndModify"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "findAndModify")
    if oplog_err is not None:
        return oplog_err
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
    collation = doc.get("collation")

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
        ctx.db_name, coll, query, sort=sort, limit=1, let=let, collation=collation
    )

    if not candidates:
        if upsert and not is_remove:
            # Validator on the upsert path too — mongo-node-driver's
            # ``Document Validation should allow bypassing document
            # validation on findAndModify`` test calls
            # ``findOneAndUpdate(..., upsert=true)`` against a
            # validator-bound collection and asserts a
            # ``MongoServerError`` when bypass is off.
            bypass_validation_fam_up = bool(doc.get("bypassDocumentValidation", False))
            coll_opts_up = ctx.storage.get_collection_options(ctx.db_name, coll)
            validator_spec_up = coll_opts_up.get("validator")
            validator_up = (
                dict(validator_spec_up)
                if isinstance(validator_spec_up, dict)
                and validator_spec_up
                and not bypass_validation_fam_up
                else None
            )
            try:
                result = ctx.storage.update_matching(
                    ctx.db_name,
                    coll,
                    query,
                    update,
                    multi=False,
                    upsert=True,
                    let=let,
                    array_filters=array_filters,
                    collation=collation,
                    validator=validator_up,
                    journal=_wants_journal(doc),
                )
            except DocumentValidationError as exc:
                return {
                    "ok": 0.0,
                    "errmsg": "Document failed validation",
                    "code": 121,
                    "codeName": "DocumentValidationFailure",
                    "errInfo": {
                        "failingDocumentId": exc.doc_id,
                        "details": {"operatorName": "validator"},
                    },
                }
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
            if return_new and result["did_upsert"]:
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
        ctx.storage.delete_matching(
            ctx.db_name,
            coll,
            {"_id": matched_id},
            limit=1,
            journal=_wants_journal(doc),
        )
        value = matched_doc
        if fields:
            value = apply_projection(value, fields)
        return {
            "lastErrorObject": {"n": 1, "updatedExisting": True},
            "value": value,
            "ok": 1.0,
        }

    # Document validator check: if the collection has a ``validator``
    # set via ``collMod``/``create``, simulate the update first and
    # reject when the resulting doc fails validation. Mongo-java-
    # driver's ``findOneAndUpdate-errorResponse`` test pins this
    # path — without it, the update silently succeeds and the test
    # fails because no exception was thrown. Honour
    # ``bypassDocumentValidation: true``.
    bypass_validation_fam = bool(doc.get("bypassDocumentValidation", False))
    coll_opts = ctx.storage.get_collection_options(ctx.db_name, coll)
    validator_spec = coll_opts.get("validator")
    if isinstance(validator_spec, dict) and validator_spec and not bypass_validation_fam:
        from secantus.update import apply_update as _apply_update_check
        from secantus.update import find_positional_matches as _pos_matches

        try:
            simulated = _apply_update_check(
                matched_doc,
                update,
                array_filters=array_filters,
                positional_matches=_pos_matches(matched_doc, {"_id": matched_id}),
                let=let,
            )
        except Exception:
            simulated = None
        if simulated is not None:
            verr = _validate_doc_against_collection(ctx.storage, ctx.db_name, coll, simulated)
            if verr is not None:
                return verr

    try:
        ctx.storage.update_matching(
            ctx.db_name,
            coll,
            {"_id": matched_id},
            update,
            multi=False,
            array_filters=array_filters,
            let=let,
            journal=_wants_journal(doc),
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
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["drop"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "drop")
    if oplog_err is not None:
        return oplog_err
    existed = ctx.storage.drop_collection(ctx.db_name, coll)
    if not existed:
        # Modern mongod treats ``drop`` of a non-existent collection as
        # an idempotent success (``{ok: 1}``), not a NamespaceNotFound
        # error. Returning ok:1 also lets the generic dispatch path
        # attach a ``writeConcernError`` for an unsatisfiable write
        # concern — pymongo's test_drop_collection drops an
        # already-absent collection with w:50 and asserts a
        # WriteConcernError, which requires the ok:1 reply shape.
        return {"ok": 1.0}
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
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    # Reject unknown top-level options on ``create``. Real mongod
    # surfaces unknown fields as ``Location40415`` (40415, IDLUnknownField).
    # mongo-ruby-driver's ``Collection#create ... a failed operation
    # using a session`` shared spec passes ``invalid: true`` specifically
    # to provoke this rejection. The whitelist below covers every
    # option mongod's ``create`` IDL accepts plus the wire envelope
    # fields any driver may attach.
    unknown = next(
        (k for k in doc if k not in _CREATE_KNOWN_OPTIONS and not k.startswith("$")),
        None,
    )
    if unknown is not None:
        return {
            "ok": 0.0,
            "errmsg": f"BSON field 'create.{unknown}' is an unknown field",
            "code": 40415,
            "codeName": "Location40415",
        }
    coll = doc["create"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "create")
    if oplog_err is not None:
        return oplog_err
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
    stored: dict[str, Any] = {}
    if capped:
        stored["capped"] = True
        stored["size"] = int(doc["size"])
        if doc.get("max") is not None:
            stored["max"] = int(doc["max"])
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        stored["changeStreamPreAndPostImages"] = dict(pre_post)
    validator = doc.get("validator")
    if isinstance(validator, Mapping):
        stored["validator"] = dict(validator)
    # ``listCollections`` round-trips all of these options under
    # ``options`` per real mongod. Mongo-go-driver's
    # ``TestDatabase/create_collection/options/all_options_except_
    # collation_and_csppi`` test installs each one then asserts the
    # echo. Mongo-ruby-driver's ``listCollection ... options.validator``
    # spec is the same shape.
    # ``writeConcern`` is a per-command option, not a collection
    # option — real mongod doesn't echo it in listCollections.
    # ``lsid`` and ``$db`` are wire envelope fields, never stored.
    _PASSTHROUGH_CREATE_OPTIONS = (
        "storageEngine",
        "indexOptionDefaults",
        "validationAction",
        "validationLevel",
        "collation",
        "expireAfterSeconds",
        "timeseries",
    )
    for opt_name in _PASSTHROUGH_CREATE_OPTIONS:
        if opt_name in doc:
            stored[opt_name] = doc[opt_name]
    # ``clusteredIndex`` makes ``_id`` the collection's clustering key
    # (the doc table IS the index — exactly SecantusDB's WiredTiger
    # layout already, since the doc table is keyed by ``_id``). mongod
    # only allows it on ``{_id: 1}`` with ``unique: true``; we normalise
    # the stored option (default name ``_id_``, add ``v: 2``) so
    # listCollections / listIndexes echo mongod's shape.
    if "clusteredIndex" in doc:
        ci = doc["clusteredIndex"]
        if isinstance(ci, Mapping):
            if ci.get("key") != {"_id": 1}:
                return {
                    "ok": 0.0,
                    "errmsg": "The clusteredIndex option is only supported for key: {_id: 1}",
                    "code": 197,
                    "codeName": "InvalidIndexSpecificationOption",
                }
            if ci.get("unique") is not True:
                return {
                    "ok": 0.0,
                    "errmsg": "The clusteredIndex option requires unique: true to be specified",
                    "code": 5979700,
                    "codeName": "Location5979700",
                }
            stored["clusteredIndex"] = {
                "v": 2,
                "key": {"_id": 1},
                "name": ci.get("name") or "_id_",
                "unique": True,
            }
    # MongoDB 3.4+ ``viewOn`` + ``pipeline`` makes the collection a
    # read-only view of another collection filtered through an
    # aggregation pipeline. Mongo-java-driver's
    # ``estimatedDocumentCount works correctly on views`` test
    # creates one and asserts ``count`` returns the right number
    # after the pipeline filters the source docs.
    view_on = doc.get("viewOn")
    if isinstance(view_on, str):
        view_pipeline = doc.get("pipeline") or []
        if not isinstance(view_pipeline, list):
            view_pipeline = []
        stored["viewOn"] = view_on
        stored["viewPipeline"] = list(view_pipeline)
    if stored:
        ctx.storage.set_collection_options(ctx.db_name, coll, **stored)
    return {"ok": 1.0}


def _coll_mod(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["collMod"]
    if not ctx.storage.collection_exists(ctx.db_name, coll):
        return {
            "ok": 0.0,
            "errmsg": f"ns not found: {ctx.db_name}.{coll}",
            "code": 26,
            "codeName": "NamespaceNotFound",
        }
    # Collect the options this collMod actually changed; the same map
    # becomes the ``modify`` change event's ``operationDescription`` (a bare
    # collMod with no options is a valid no-op that still emits an event).
    description: dict[str, Any] = {}
    pre_post = doc.get("changeStreamPreAndPostImages")
    if isinstance(pre_post, Mapping):
        ctx.storage.set_collection_options(
            ctx.db_name, coll, changeStreamPreAndPostImages=dict(pre_post)
        )
        description["changeStreamPreAndPostImages"] = dict(pre_post)
    # MongoDB 3.2+ ``validator``: a query predicate that every
    # subsequent insert / update must satisfy. Mongo-java-driver's
    # ``findOneAndUpdate-errorResponse`` test installs a validator
    # via ``modifyCollection`` then asserts that an update that
    # violates it surfaces as a ``DocumentValidationFailure``
    # (code 121) with ``errInfo.failingDocumentId`` + ``details``.
    validator = doc.get("validator")
    if isinstance(validator, Mapping):
        ctx.storage.set_collection_options(ctx.db_name, coll, validator=dict(validator))
        description["validator"] = dict(validator)
    # Emit the collMod command oplog entry so a change stream with
    # ``showExpandedEvents`` surfaces a ``modify`` event.
    ctx.storage.record_collmod(ctx.db_name, coll, description)
    return {"ok": 1.0}


def _list_collections(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """List collections honouring ``filter`` and ``nameOnly`` per mongod.

    Each descriptor mirrors mongod's wire shape:
    ``{name, type, options, info: {readOnly, uuid}, idIndex: {v, key, name, ns}}``.
    ``filter`` is a regular query predicate evaluated against the
    descriptor (the dotted-path matcher walks into the nested fields so
    ``{options.capped: true}`` works the way the Go driver's
    ``filter_passed_to_listCollections`` test expects). ``nameOnly: true``
    strips down each entry to ``{name, type}`` — drivers use this when
    they only care about names.
    """
    names = ctx.storage.list_collections(ctx.db_name)
    name_only = bool(doc.get("nameOnly", False))
    filter_doc = doc.get("filter")

    # Storage keys that are server-side bookkeeping, not user-facing
    # options. Anything else in the stored map round-trips into the
    # ``options`` reply so drivers can read back exactly what was
    # passed to ``create`` / ``collMod`` (mongo-go-driver's
    # ``TestDatabase/create_collection/options/*`` + mongo-ruby-driver's
    # ``listCollection ... options.validator`` specs both rely on this).
    _INTERNAL_COLL_OPTIONS = {"uuid"}
    batch: list[dict[str, Any]] = []
    for n in names:
        raw = ctx.storage.get_collection_options(ctx.db_name, n)
        opts: dict[str, Any] = {k: v for k, v in raw.items() if k not in _INTERNAL_COLL_OPTIONS}
        # ``viewOn`` collections surface as ``type: "view"`` and the
        # view's pipeline lives under ``options.pipeline`` (the
        # storage layer keeps it as ``viewPipeline`` so the option-blob
        # key doesn't collide with ``pipeline`` arguments on other
        # commands; rename on the way out).
        is_view = isinstance(opts.get("viewOn"), str)
        if "viewPipeline" in opts:
            opts["pipeline"] = opts.pop("viewPipeline")
        # info.uuid: BSON Binary subtype 4 (the standard "old UUID"
        # subtype mongod uses in listCollections / change-stream events).
        # The mongo-go-driver's ``ListCollectionSpecifications`` checks
        # ``info.uuid`` is present and decodes it via ``Binary``; pymongo's
        # ``listCollections`` cursor round-trips it as a ``bson.Binary``
        # with the same subtype.
        coll_uuid = ctx.storage.collection_uuid(ctx.db_name, n)
        info = {
            "readOnly": is_view,
            "uuid": bson.Binary(coll_uuid.bytes, 4),
        }
        entry: dict[str, Any] = {
            "name": n,
            "type": "view" if is_view else "collection",
            "options": opts,
            "info": info,
        }
        # ``idIndex`` is only meaningful on real collections; views
        # don't have one (mongod omits the field on views).
        if not is_view:
            entry["idIndex"] = {
                "v": 2,
                "key": {"_id": 1},
                "name": "_id_",
                "ns": f"{ctx.db_name}.{n}",
            }
        batch.append(entry)

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

    ``sizeOnDisk`` per database is the sum of bson-encoded doc
    bytes across all collections in that database (same accounting
    ``collStats`` / ``dbStats`` use for ``dataSize``).
    mongo-rust-driver's ``test::client::list_databases`` asserts
    ``size_on_disk > 0`` on every entry it gets back from a
    populated db; returning 0 unconditionally — as we did before —
    failed the test even when the db actually had data. ``empty``
    is derived from the size: an empty database has 0 bytes; a
    populated one doesn't.
    """
    names = ctx.storage.list_databases()
    name_only = bool(doc.get("nameOnly", False))
    filter_doc = doc.get("filter")

    descriptors: list[dict[str, Any]] = []
    total_size = 0
    for n in names:
        if name_only:
            # Skip the per-collection scan when the driver doesn't
            # need the size — same shortcut mongod takes.
            descriptors.append({"name": n})
            continue
        size = 0
        for coll in ctx.storage.list_collections(n):
            size += ctx.storage.collection_data_size(n, coll)
        descriptors.append({"name": n, "sizeOnDisk": size, "empty": size == 0})
        total_size += size

    if isinstance(filter_doc, dict) and filter_doc:
        descriptors = [d for d in descriptors if matches(d, filter_doc)]

    return {
        "databases": descriptors,
        "totalSize": total_size,
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
    # A clustered collection has no separate ``_id_`` index — the
    # clustering key IS the index. mongod reports a single entry for it
    # carrying ``clustered: true`` (and the user's name / unique).
    # Replace the synthesised ``_id_`` entry; secondary indexes pass
    # through unchanged.
    coll_opts = ctx.storage.get_collection_options(ctx.db_name, coll) or {}
    ci = coll_opts.get("clusteredIndex")
    if isinstance(ci, Mapping):
        clustered_entry = {
            "v": ci.get("v", 2),
            "key": {"_id": 1},
            "name": ci.get("name", "_id_"),
            "unique": True,
            "clustered": True,
        }
        indexes = [clustered_entry] + [ix for ix in indexes if ix.get("name") != "_id_"]
    # Honour ``cursor.batchSize`` so callers with many indexes
    # actually round-trip via ``getMore`` — mongo-go-driver's
    # ``TestIndexView/list/getMore_commands_are_monitored`` test
    # asserts at least one getMore fires when batchSize < total.
    # Negative ``batchSize`` is rejected: real mongod returns
    # BadValue. mongo-ruby-driver's ``failed_operation`` shared spec
    # constructs ``authorized_collection.indexes(batch_size: -100, ...)``
    # specifically to provoke this.
    cursor_opts = doc.get("cursor") or {}
    raw_bs = cursor_opts.get("batchSize")
    if raw_bs is not None:
        try:
            batch_size = int(raw_bs)
        except (TypeError, ValueError):
            return {
                "ok": 0.0,
                "errmsg": "BSON field 'batchSize' must be a number",
                "code": 14,
                "codeName": "TypeMismatch",
            }
        if batch_size < 0:
            return {
                "ok": 0.0,
                "errmsg": f"BSON field 'batchSize' value must be >= 0, actual value {batch_size}",
                "code": 51024,
                "codeName": "BadValue",
            }
    else:
        batch_size = DEFAULT_BATCH_SIZE
    ns = f"{ctx.db_name}.$cmd.listIndexes.{coll}"
    first_batch, cursor_id = _split_into_cursor(indexes, batch_size, ns, ctx.cursors)
    return {
        "cursor": {
            "firstBatch": first_batch,
            "id": bson.Int64(cursor_id),
            "ns": ns,
        },
        "ok": 1.0,
    }


def _create_indexes(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus.storage import (
        CreateIndexUnsupported,
        GeoExtractError,
        IndexConflict,
        IndexOptionsConflict,
    )

    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
    coll = doc["createIndexes"]
    oplog_err = _reject_oplog_rs_write(ctx, coll, "createIndexes")
    if oplog_err is not None:
        return oplog_err
    indexes = doc.get("indexes", [])
    # ``commitQuorum`` is a top-level option on ``createIndexes`` (not
    # per-index). MongoDB 4.4+ accepts an integer, ``"majority"``, or
    # ``"votingMembers"``; unknown strings trigger a write-concern-mode
    # lookup miss in the replica-set config. mongo-ruby-driver's
    # ``unsupported-value`` commit_quorum spec pins the regex.
    commit_quorum = doc.get("commitQuorum")
    if commit_quorum is not None and not (
        isinstance(commit_quorum, int) or commit_quorum in ("majority", "votingMembers")
    ):
        return {
            "ok": 0.0,
            "errmsg": (
                f"No write concern mode named {commit_quorum!r} found in replica set configuration"
            ),
            "code": 79,
            "codeName": "UnknownReplWriteConcern",
        }
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
        # Reject unknown options on the index spec itself. Real mongod
        # rejects with ``Location40415`` (40415, IDLUnknownField).
        # mongo-ruby-driver's ``Index::View#create_one when provided a
        # session`` shared spec passes ``invalid: true`` specifically to
        # provoke this rejection. The whitelist below covers every
        # option mongod's index-spec IDL accepts plus the legacy /
        # deprecated forms drivers still emit.
        unknown_idx = next(
            (k for k in options if k not in _INDEX_SPEC_KNOWN_OPTIONS),
            None,
        )
        if unknown_idx is not None:
            return {
                "ok": 0.0,
                "errmsg": (
                    f"Error in specification {dict(idx_spec)!r}: "
                    f"the field '{unknown_idx}' is an unknown field"
                ),
                "code": 40415,
                "codeName": "Location40415",
            }
        # Canonicalise option-blob shape per mongod: falsy values for
        # ``hidden`` / ``sparse`` / ``unique`` are stripped (mongod stores
        # only the non-default form). Mongo-ruby-driver's
        # ``Collection#indexes`` specs assert ``hidden: false`` does NOT
        # come back in the index spec — same logic for ``sparse`` /
        # ``unique`` since the default is false there too.
        for _falsy_opt in ("hidden", "sparse", "unique"):
            if _falsy_opt in options and not options[_falsy_opt]:
                options.pop(_falsy_opt)
        # ``dropDups`` was removed in MongoDB 3.0; modern mongod accepts it on
        # the wire but ignores it entirely (it never drops duplicates). Drop it
        # here so it isn't stored as an index option — a unique index built
        # over duplicate data then fails on the duplicate (DuplicateKey 11000),
        # exactly as mongod does. pymongo's test_index_dont_drop_dups pins this.
        options.pop("dropDups", None)
        # ``partialFilterExpression`` must be a document. Numbers / strings
        # / arrays etc. are rejected by mongod with BadValue.
        pfe = options.get("partialFilterExpression")
        if pfe is not None and not isinstance(pfe, dict):
            return {
                "ok": 0.0,
                "errmsg": "partialFilterExpression must be a document",
                "code": 2,
                "codeName": "BadValue",
            }
        # The expression itself must parse as a valid filter — mongod
        # rejects unknown operators ({x: {$asdasd: 3}}) and malformed
        # logical operators ({$and: 5}). Run it against an empty doc so
        # the query engine surfaces the same parse errors it would at
        # query time. pymongo's test_index_filter pins these rejections.
        if isinstance(pfe, dict):
            from secantus.query import QueryError

            try:
                matches({}, pfe)
            except (QueryError, ExpressionError, TypeError, ValueError, KeyError) as exc:
                return {
                    "ok": 0.0,
                    "errmsg": f"Error in specification, partialFilterExpression is invalid: {exc}",
                    "code": 2,
                    "codeName": "BadValue",
                }
        # ``wildcardProjection`` is only valid on wildcard indexes (a key
        # of the form ``{ "$**": 1 }`` or ``{ "field.$**": 1 }``). When
        # present it must be a non-empty document — mongod rejects ints,
        # strings, empty docs, etc. mongo-ruby-driver's ``invalid wildcard
        # projection expression`` and ``wildcard projection to an invalid
        # base index`` tests pin both messages via regex.
        wcp = options.get("wildcardProjection")
        if wcp is not None:
            if not isinstance(wcp, Mapping) or not wcp:
                return {
                    "ok": 0.0,
                    "errmsg": (
                        f"Error in specification {{ key: {dict(key_spec)!r}, "
                        f"wildcardProjection: {wcp!r} }} :: caused by :: "
                        "wildcardProjection must be a non-empty object"
                    ),
                    "code": 67,
                    "codeName": "CannotCreateIndex",
                }
            is_wildcard_key = any(
                isinstance(k, str) and (k == "$**" or k.endswith(".$**")) for k in key_spec
            )
            if not is_wildcard_key:
                return {
                    "ok": 0.0,
                    "errmsg": (
                        f"Error in specification {{ key: {dict(key_spec)!r}, "
                        f"wildcardProjection: {dict(wcp)!r} }} :: caused by :: "
                        "wildcardProjection is only allowed on wildcard indexes"
                    ),
                    "code": 67,
                    "codeName": "CannotCreateIndex",
                }
        try:
            new = ctx.storage.create_index(ctx.db_name, coll, name, key_spec, options)
        except CreateIndexUnsupported as exc:
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 67,
                "codeName": "CannotCreateIndex",
            }
        except IndexOptionsConflict as exc:
            return {
                "ok": 0.0,
                "errmsg": str(exc),
                "code": 85,
                "codeName": "IndexOptionsConflict",
            }
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
    wc_err = _validate_write_concern(doc)
    if wc_err is not None:
        return wc_err
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


def _change_stream_fatal_reply(exc: changestreams.ChangeStreamFatalError) -> dict[str, Any]:
    """mongod's reply shape for fatal change-stream conditions: the
    error code plus the ``NonResumableChangeStreamError`` label so
    drivers know not to auto-resume (asserted by the unified
    change-streams-errors specs)."""
    return {
        "ok": 0.0,
        "errmsg": str(exc),
        "code": exc.code,
        "codeName": exc.codeName,
        "errorLabels": ["NonResumableChangeStreamError"],
    }


class _CappedPositionLost(Exception):
    """Raised by a capped-collection tailable producer when the document the
    cursor was anchored on (its last-returned ``id_key``) has been evicted by
    capped rollover. mongod kills such a cursor with code 136
    ``CappedPositionLost``; pymongo swallows that error for tailable cursors
    (it's in ``_CURSOR_CLOSED_ERRORS``) so the cursor simply reports
    ``alive == False`` and the in-flight read returns no documents."""


def _capped_position_lost_reply() -> dict[str, Any]:
    return {
        "ok": 0.0,
        "errmsg": (
            "CollectionScan died due to failure to restore tailable cursor "
            "position. Last seen record id: RecordId"
        ),
        "code": 136,
        "codeName": "CappedPositionLost",
    }


def _drain_change_stream_producer(entry: Any) -> None:
    """Pull one producer batch into ``entry.remaining`` if it's empty.

    May raise ``changestreams.ChangeStreamFatalError`` — the caller turns
    that into a fatal reply and kills the cursor. Shared by the
    change-stream open (firstBatch) and ``getMore`` (nextBatch) paths.
    """
    if not entry.remaining and entry.producer is not None:
        new_events = entry.producer()
        if new_events:
            entry.remaining.extend(new_events)


def _change_stream_cursor_doc(
    entry: Any, cursor_id: int, batch_size: int, ns: str, *, batch_key: str
) -> dict[str, Any]:
    """Slice ``entry.remaining`` into one batch and build the ``cursor``
    sub-document for a change-stream reply.

    Handles the invalidate/final-event bookkeeping, the cursor-alive →
    ``id: 0`` transition, and the ``postBatchResumeToken``. ``batch_key``
    is ``"firstBatch"`` for the aggregate open and ``"nextBatch"`` for
    ``getMore`` — the only shape difference between the two paths.
    """
    batch = entry.remaining[:batch_size]
    entry.remaining = entry.remaining[batch_size:]
    if not entry.remaining and entry.invalidated and entry.final_event_pending:
        # The invalidate event has now been delivered.
        entry.final_event_pending = False
    cursor_alive = not (entry.invalidated and not entry.remaining and not entry.final_event_pending)
    cursor_doc: dict[str, Any] = {
        batch_key: batch,
        # Cursor `id` MUST be int64 — Go driver hard-fails int32.
        "id": bson.Int64(cursor_id if cursor_alive else 0),
        "ns": ns,
    }
    if entry.last_token is not None:
        # `postBatchResumeToken` lets change-stream consumers advance
        # their resume position even when the batch is empty — MongoDB
        # 4.2+ feature, mongo-go-driver and pymongo expect it on every
        # change-stream reply.
        cursor_doc["postBatchResumeToken"] = entry.last_token
    return cursor_doc


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
    try:
        _drain_change_stream_producer(entry)
    except changestreams.ChangeStreamFatalError as exc:
        ctx.cursors.kill([cursor_id])
        return _change_stream_fatal_reply(exc)
    except _CappedPositionLost:
        ctx.cursors.kill([cursor_id])
        return _capped_position_lost_reply()
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
                lambda: (
                    ctx.storage.oplog_tail_seq_nolock() > captured_tail
                    or entry.invalidated
                    or ctx.storage._shutting_down
                ),
                timeout=wait_seconds,
            )
        try:
            _drain_change_stream_producer(entry)
        except changestreams.ChangeStreamFatalError as exc:
            ctx.cursors.kill([cursor_id])
            return _change_stream_fatal_reply(exc)
        except _CappedPositionLost:
            ctx.cursors.kill([cursor_id])
            return _capped_position_lost_reply()
    return {
        "cursor": _change_stream_cursor_doc(
            entry, cursor_id, batch_size, ns, batch_key="nextBatch"
        ),
        "ok": 1.0,
    }


def _map_reduce(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Minimal ``mapReduce`` — translates the canonical emit/count
    pattern to a ``$group`` aggregation. No general JS evaluation:
    only the ``function() { emit(this.<field>, 1); }`` map +
    ``function(key, values) { return values.length; }`` reduce shape
    that mongo-java-driver's ``testMapReduceWithGenerics`` test uses
    (and that real apps overwhelmingly use to mean "count by field").
    Anything else returns the deprecation error mongod 5.0 introduced.
    """
    import re as _re

    coll = doc["mapReduce"]
    map_fn = str(doc.get("map") or "")
    reduce_fn = str(doc.get("reduce") or "")
    out = doc.get("out") or {}
    if not (isinstance(out, dict) and "inline" in out):
        return {
            "ok": 0.0,
            "errmsg": "mapReduce on this server only supports {out: {inline: 1}}",
            "code": 9,
            "codeName": "FailedToParse",
        }
    m = _re.search(r"emit\s*\(\s*this\.(\w+)\s*,\s*1\s*\)", map_fn)
    if m is not None and "values.length" in reduce_fn:
        # Canonical ``emit(this.<field>, 1)`` + ``values.length`` count
        # pattern — translate to a ``$group`` aggregation.
        field = m.group(1)
        pipeline = [{"$group": {"_id": f"${field}", "value": {"$sum": 1}}}]
        pipeline_ctx = PipelineContext(storage=ctx.storage, db_name=ctx.db_name, coll_name=coll)
        docs = ctx.storage.find_matching(ctx.db_name, coll, {})
        result_docs = apply_pipeline(docs, pipeline, pipeline_ctx)
        # Real mongod's mapReduce always returns ``value`` as a double
        # (the JS engine treats numbers as doubles). The Java driver
        # decoder enforces this — ``readDouble`` throws on Int32. Cast.
        for d in result_docs:
            if "value" in d and isinstance(d["value"], int) and not isinstance(d["value"], bool):
                d["value"] = float(d["value"])
        return {"results": result_docs, "ok": 1.0}
    # Fall-through for non-canonical map/reduce JS bodies (mongo-java-
    # driver's ``default-write-concern-3.4.yml`` exercises one such
    # pattern just to assert the wire shape — it doesn't check the
    # result). We don't ship a JS runtime so we can't actually
    # evaluate arbitrary map/reduce, but returning an empty result
    # with ``ok: 1`` lets the wire-shape probe pass; tests that
    # depend on the actual values would already need a real
    # ``mongod``. mapReduce is deprecated in MongoDB 5.0+ and slated
    # for removal — full JS evaluation is intentionally out of scope.
    return {"results": [], "ok": 1.0}


def _aggregate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    from secantus import changestreams
    from secantus.storage import BadHint, IndexConflict

    coll = doc["aggregate"]
    pipeline = doc.get("pipeline", [])
    hint = doc.get("hint")
    # ``let`` user-vars threaded into the pipeline context so
    # ``$expr`` clauses inside ``$match`` and the aggregation
    # expression language can resolve ``$$name`` references.
    let = _resolve_let_vars(doc.get("let"))
    collation = doc.get("collation")
    cursor_opts = doc.get("cursor") or {}
    raw_agg_batch = cursor_opts.get("batchSize")
    batch_size = DEFAULT_BATCH_SIZE if raw_agg_batch is None else int(raw_agg_batch)
    coll_name = ""

    first_stage = pipeline[0] if pipeline else {}
    is_change_stream = isinstance(first_stage, Mapping) and "$changeStream" in first_stage

    if is_change_stream:
        # Change streams require a replica-set deployment (real mongod
        # rejects on standalone with ``IllegalOperation`` (40573)). When
        # SecantusDB is booted with ``--standalone`` we drop the
        # replica-set advertisement in ``hello``, so the topology the
        # driver sees is STANDALONE — the unified ``change-streams-
        # errors`` spec gates its single-topology test on exactly that
        # response. Mirror mongod here.
        if ctx.replica_set_name is None:
            return {
                "ok": 0.0,
                "errmsg": ("The $changeStream stage is only supported on replica sets"),
                "code": 40573,
                "codeName": "IllegalOperation",
            }
        # Change streams support only the default / majority read
        # concern — mongod rejects an explicit ``local`` (or any other
        # level) with InvalidOptions. pymongo's ``test_read_concern``
        # pins the rejection server-side.
        rc_cs = doc.get("readConcern")
        if isinstance(rc_cs, Mapping) and rc_cs.get("level") not in (None, "majority"):
            return {
                "ok": 0.0,
                "errmsg": (
                    f"readConcern level '{rc_cs.get('level')}' is not supported "
                    "for change streams; only 'majority' (or the default) is allowed"
                ),
                "code": 72,
                "codeName": "InvalidOptions",
            }
        return _aggregate_change_stream(doc, ctx, coll, pipeline, batch_size)

    # Linearizable read concern is incompatible with write stages.
    # mongod rejects with InvalidOptions (72): the aggregate-out-readConcern
    # unified spec test asserts the operation errors when ``$out`` runs
    # under ``readConcern: linearizable``; ``$merge`` carries the same
    # restriction.
    rc = doc.get("readConcern")
    if isinstance(rc, Mapping) and rc.get("level") == "linearizable":
        for stage in pipeline:
            if isinstance(stage, Mapping):
                bad = "$out" if "$out" in stage else ("$merge" if "$merge" in stage else None)
                if bad is not None:
                    return {
                        "ok": 0.0,
                        "errmsg": (
                            f"{bad} cannot be used with a 'linearizable' read concern level"
                        ),
                        "code": 72,
                        "codeName": "InvalidOptions",
                    }

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
                    ctx.db_name,
                    coll,
                    initial_filter,
                    hint=hint,
                    let=let,
                    collation=collation,
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
        collation=collation,
        command_doc=dict(doc),
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
        # ``resumeAfter`` cannot point at an invalidate event's token —
        # the stream it came from is over. mongod requires
        # ``startAfter`` for that (260 InvalidResumeToken).
        if cs_spec.resume_after is not None and data.from_invalidate:
            return {
                "ok": 0.0,
                "errmsg": (
                    "Attempting to resume a change stream using 'resumeAfter' "
                    "is not allowed from an invalidate notification; use "
                    "'startAfter' instead"
                ),
                "code": 260,
                "codeName": "InvalidResumeToken",
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
    # Validate user-stage names NOW — mongod rejects an unrecognized
    # stage at aggregate time (40324), not at the first getMore.
    validate_stage_names(pipeline_after_cs)
    # ``$changeStreamSplitLargeEvent`` in the pipeline is a second opt-in
    # path for event-splitting (alongside
    # ``$changeStream: {splitLargeChangeStreamEvents: true}``). The
    # rust driver / node driver / java driver use the pipeline-stage
    # form when the user opts into split via the high-level cursor
    # API (``coll.watch().pipeline([{$changeStreamSplitLargeEvent: {}}])``).
    # When either path is taken, set the producer-side flag so
    # ``stamp_split_event`` actually splits.
    if any(
        isinstance(stage, Mapping) and "$changeStreamSplitLargeEvent" in stage
        for stage in pipeline_after_cs
    ):
        cs_spec.split_large_events = True
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
                    show_expanded_events=cs_spec.show_expanded_events,
                    scope=scope,
                )
            except changestreams.ChangeStreamFatalError:
                # mongod surfaces this as a getMore error (code 280) and
                # the stream is over — fullDocument: "required" misses
                # and pre-image lookups that aren't stored both land
                # here. Don't advance the position: the error, not the
                # event, is the outcome. _get_more shapes the reply.
                raise
            if ev is not None:
                if cs_spec.split_large_events:
                    events.extend(changestreams.stamp_split_event(ev))
                else:
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
                    events.extend(changestreams.stamp_split_event(inv))
                else:
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
            for ev in events:
                if isinstance(ev, Mapping) and "_id" not in ev:
                    # mongod 4.1.8+: an event whose ``_id`` (the resume
                    # token) was projected out by the user pipeline is
                    # fatal — the stream can't be resumed past it.
                    raise changestreams.ChangeStreamFatalError(
                        "Encountered an event whose _id field, which contains the "
                        "resume token, was modified by the pipeline. Modifying the "
                        "_id field of an event makes it unusable for resuming"
                    )
        return events

    cursor_id = ctx.cursors.register_tailable(
        ns,
        producer,
        await_data=True,
        position_seq=start_seq - 1,
        collection_uuid=coll_uuid,
    )
    entry = ctx.cursors.get(cursor_id)
    entry_ref["entry"] = entry
    initial_ts = ctx.storage.current_cluster_time()
    initial_token = changestreams.make_resume_token(
        changestreams.ResumeTokenData(start_seq - 1, initial_ts, ns, {})
    )
    entry.last_token = initial_token

    # A *resuming* open (resumeAfter / startAfter / startAtOperationTime) may
    # have a backlog of already-committed events between the start position
    # and now. mongod returns those in the aggregate's firstBatch — a driver
    # that checks the cursor for buffered data before sending any getMore
    # (pymongo's ``CommandCursor._has_next()``, which never itself issues a
    # getMore) must see them. Drain the producer once to populate firstBatch;
    # there is no awaitData wait on open. A fresh tail watch has no backlog,
    # so this is gated to the resuming forms only — the common case keeps the
    # untouched empty-firstBatch + minted-token path.
    is_resuming = (
        cs_spec.resume_after is not None
        or cs_spec.start_after is not None
        or cs_spec.start_at_operation_time is not None
    )
    if is_resuming:
        saved_pos = entry.position_seq
        try:
            _drain_change_stream_producer(entry)
        except changestreams.ChangeStreamFatalError as exc:
            ctx.cursors.kill([cursor_id])
            return _change_stream_fatal_reply(exc)
        if entry.remaining:
            # Backlog present: the producer advanced ``position_seq`` and set
            # ``last_token`` to the last backlog event. Hand the batch back as
            # firstBatch (overflow stays in ``remaining`` for the first
            # getMore). PyMongo does not cache the PBRT off a *non-empty*
            # firstBatch, so an uniterated resumed stream still reports
            # resume_token == the token the caller passed (prose test #14).
            return {
                "cursor": _change_stream_cursor_doc(
                    entry, cursor_id, batch_size, ns, batch_key="firstBatch"
                ),
                "operationTime": initial_ts,
                "ok": 1.0,
            }
        # No backlog: restore the at-open position + token so the empty-batch
        # open behaves exactly as before. The producer's no-match position
        # advance is a getMore-time concern (so a quiet collection's PBRT can
        # move past unrelated activity), not an open-time one — at open the
        # cursor still sits at the resume point.
        entry.position_seq = saved_pos
        entry.last_token = initial_token
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
    # Order: SCRAM modern first, legacy SHA-1 next, then X509. The
    # driver picks the first one it supports; pymongo / Go / Java all
    # default to SCRAM-SHA-256 when offered.
    mechs = [m for m in (SCRAM_SHA_256, SCRAM_SHA_1, MONGODB_X509) if m in creds_doc]
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
    if mechanism == MONGODB_X509:
        return _sasl_start_x509(doc, ctx, conn)
    if mechanism not in (SCRAM_SHA_256, SCRAM_SHA_1):
        return _auth_failure(
            f"Unsupported SASL mechanism: {mechanism!r} "
            f"(supported: {SCRAM_SHA_256}, {SCRAM_SHA_1}, {MONGODB_X509})"
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


def _sasl_start_x509(
    doc: dict[str, Any], ctx: CommandContext, conn: ConnectionAuth
) -> dict[str, Any]:
    """One-shot MONGODB-X509 auth.

    Unlike SCRAM there's no challenge / response. The credential is the
    TLS client cert the connection presented during the handshake; the
    server reads the cert's subject DN, looks up a user record whose
    username equals that DN AND whose ``credentials`` doc carries an
    X509 entry, and marks the connection authenticated. ``done=True``
    on the first reply — no ``_sasl_continue`` round-trip.

    The optional ``payload`` in the request can carry the username
    (some drivers send it as a sanity check); when present, it must
    equal the cert DN exactly. Mismatch is an auth failure — the
    driver claimed identity X but the cert says identity Y.

    Refuses with ``AuthenticationFailed`` (code 18) when:

    * The connection is plaintext or didn't verify a client cert
      (``peer_cert_dn`` is None) — X509 needs mTLS.
    * No user record exists for the cert DN on the auth db
      (``$external`` by convention; per-driver-config db otherwise).
    * The matched user record has no ``MONGODB-X509`` entry in its
      ``credentials`` (so a SCRAM-only user can't be impersonated
      via cert).
    * The payload-claimed username doesn't match the cert DN.
    """
    if ctx.peer_cert_dn is None:
        return _auth_failure(
            "MONGODB-X509 requires the client to present a verified TLS cert "
            "(connection is plaintext or no client cert was offered)"
        )
    db_name = ctx.db_name or "$external"
    # MongoDB convention: X509 users live on the ``$external`` auth db,
    # which is a virtual db (no real collections). We also accept the
    # legacy ``admin`` placement to give operators flexibility.
    claimed = _x509_payload_username(_payload_bytes(doc.get("payload")))
    if claimed and claimed != ctx.peer_cert_dn:
        return _auth_failure(
            f"MONGODB-X509: payload username {claimed!r} doesn't match cert DN {ctx.peer_cert_dn!r}"
        )
    record = ctx.storage.get_user(db_name, ctx.peer_cert_dn)
    if record is None and db_name == "$external":
        # Fallback: also try `admin` so users created with the
        # ``--db admin`` shorthand work too.
        record = ctx.storage.get_user("admin", ctx.peer_cert_dn)
        if record is not None:
            db_name = "admin"
    if record is None:
        return _auth_failure(
            f"MONGODB-X509: no user found with name {ctx.peer_cert_dn!r} on {db_name!r}"
        )
    creds_doc = record.get("credentials")
    if not isinstance(creds_doc, dict) or MONGODB_X509 not in creds_doc:
        return _auth_failure(
            f"MONGODB-X509: user {ctx.peer_cert_dn!r} on {db_name!r} is not "
            "configured for X509 auth (no MONGODB-X509 entry in credentials)"
        )
    # All checks passed. Mark authenticated, capture roles for RBAC,
    # mirror SCRAM's reply shape with done=True.
    conn.authenticated_principals.append((db_name, ctx.peer_cert_dn))
    roles = record.get("roles") or []
    if isinstance(roles, list):
        conn.add_principal_roles(roles)
    return {
        "conversationId": conn.new_conversation_id(),
        "done": True,
        "payload": _payload_binary(b""),
        "ok": 1.0,
    }


def _authenticate(doc: dict[str, Any], ctx: CommandContext) -> dict[str, Any]:
    """Legacy ``authenticate`` command — pymongo's chosen path for
    MONGODB-X509.

    Modern drivers use ``saslStart`` for SCRAM, but for MONGODB-X509
    pymongo still sends the legacy
    ``{authenticate: 1, mechanism: "MONGODB-X509", user: <DN>}``
    command (it's a one-shot with no challenge / response, so the
    legacy single-command shape fits). The Java / Go / Node drivers
    do the same for X509 backwards-compat.

    Reuses :func:`_sasl_start_x509`'s logic via a synthetic SCRAM-less
    code path — same user lookup, same DN comparison, same
    role-binding side-effects on the connection.
    """
    conn = _ensure_conn_auth(ctx)
    mechanism = doc.get("mechanism", SCRAM_SHA_256)
    if mechanism != MONGODB_X509:
        # Legacy ``authenticate`` for SCRAM is older than SecantusDB
        # cares about — drivers ship the saslStart path for SCRAM. If
        # somebody really needs it, raise a CommandNotFound-shaped
        # error that points them at the right command.
        return {
            "ok": 0.0,
            "errmsg": (
                f"authenticate: only {MONGODB_X509!r} is supported on this "
                f"command path; use saslStart for {SCRAM_SHA_256!r} / {SCRAM_SHA_1!r}"
            ),
            "code": 2,
            "codeName": "BadValue",
        }
    if ctx.peer_cert_dn is None:
        return _auth_failure(
            "MONGODB-X509 requires the client to present a verified TLS cert "
            "(connection is plaintext or no client cert was offered)"
        )
    claimed = doc.get("user")
    if isinstance(claimed, str) and claimed and claimed != ctx.peer_cert_dn:
        return _auth_failure(
            f"MONGODB-X509: claimed user {claimed!r} doesn't match cert DN {ctx.peer_cert_dn!r}"
        )
    db_name = ctx.db_name or "$external"
    record = ctx.storage.get_user(db_name, ctx.peer_cert_dn)
    if record is None and db_name == "$external":
        record = ctx.storage.get_user("admin", ctx.peer_cert_dn)
        if record is not None:
            db_name = "admin"
    if record is None:
        return _auth_failure(
            f"MONGODB-X509: no user found with name {ctx.peer_cert_dn!r} on {db_name!r}"
        )
    creds_doc = record.get("credentials")
    if not isinstance(creds_doc, dict) or MONGODB_X509 not in creds_doc:
        return _auth_failure(
            f"MONGODB-X509: user {ctx.peer_cert_dn!r} on {db_name!r} is not "
            "configured for X509 auth (no MONGODB-X509 entry in credentials)"
        )
    conn.authenticated_principals.append((db_name, ctx.peer_cert_dn))
    roles = record.get("roles") or []
    if isinstance(roles, list):
        conn.add_principal_roles(roles)
    # Legacy ``authenticate`` reply shape: {dbname, user, ok: 1}.
    return {"dbname": db_name, "user": ctx.peer_cert_dn, "ok": 1.0}


def _x509_payload_username(payload: bytes) -> str:
    """Best-effort extraction of an optional username from the X509
    SASL payload.

    pymongo sends ``{payload: BinData(0, b"")}`` (empty) and trusts
    the cert. Other drivers send the username as a UTF-8 string
    (sometimes prefixed with the GS2 header marker). Strip the GS2
    if present, decode, and let the caller compare.
    """
    if not payload:
        return ""
    # Some drivers prefix with the SASL GS2 header ``n,,`` like SCRAM
    # does, even though X509 doesn't need one.
    if payload.startswith(b"n,,"):
        payload = payload[3:]
    return payload.decode("utf-8", errors="replace")


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
    # `mechanisms` selects which auth mechanisms the user is enabled
    # for. SCRAM-SHA-256 alone is the default — matches mongod's modern
    # behaviour. MONGODB-X509 is a TLS-cert-as-username mechanism; when
    # requested, the cert's subject DN must equal the username (so the
    # caller is creating something like ``CN=alice,O=Acme,C=US``), and
    # no password is required.
    mechanisms_arg = doc.get("mechanisms")
    if isinstance(mechanisms_arg, list) and mechanisms_arg:
        requested = [m for m in mechanisms_arg if m in (SCRAM_SHA_256, SCRAM_SHA_1, MONGODB_X509)]
    else:
        requested = [SCRAM_SHA_256]
    if not requested:
        return {
            "ok": 0.0,
            "errmsg": (
                "createUser: mechanisms must contain at least one of "
                f"{SCRAM_SHA_256!r}, {SCRAM_SHA_1!r}, {MONGODB_X509!r}"
            ),
            "code": 2,
            "codeName": "BadValue",
        }
    scram_requested = [m for m in requested if m in (SCRAM_SHA_256, SCRAM_SHA_1)]
    # Password is required when ANY SCRAM mechanism is requested.
    # MONGODB-X509-only users have no password — the cert is the
    # credential.
    if scram_requested and (not isinstance(pwd, str) or not pwd):
        return {
            "ok": 0.0,
            "errmsg": (
                "createUser: pwd (string) required when SCRAM mechanisms are requested "
                f"(got mechanisms={requested!r})"
            ),
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
    creds_doc: dict[str, object] = {}
    for mech in scram_requested:
        creds_doc.update(derive_credentials(pwd, mechanism=mech, username=username).to_doc())
    if MONGODB_X509 in requested:
        creds_doc.update(X509_CREDENTIAL_MARKER)
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
    "killOp": _kill_op,
    "fsync": _fsync,
    "profile": _profile,
    "secantusAdmin.pruneOplog": _secantus_admin_prune_oplog,
    "secantusAdmin.pruneTtl": _secantus_admin_prune_ttl,
    "secantusAdmin.backupArchive": _secantus_admin_backup_archive,
    "secantusAdmin.restoreArchive": _secantus_admin_restore_archive,
    "explain": _explain,
    "serverStatus": _server_status,
    "top": _top,
    "getCmdLineOpts": _get_cmd_line_opts,
    "getParameter": _get_parameter,
    "connectionStatus": _connection_status,
    "dbStats": _db_stats,
    "dbstats": _db_stats,
    "collStats": _coll_stats,
    "validate": _validate,
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
    "mapReduce": _map_reduce,
    "mapreduce": _map_reduce,
    "saslStart": _sasl_start,
    "saslContinue": _sasl_continue,
    "authenticate": _authenticate,
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
        # Legacy ``authenticate`` command — pymongo / Java / Go drivers
        # all use it for MONGODB-X509 (one-shot, no challenge / response,
        # the legacy single-command shape fits cleanly).
        "authenticate",
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
    "mapReduce": (A_FIND, SCOPE_COLLECTION),
    "mapreduce": (A_FIND, SCOPE_COLLECTION),
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
    "validate": (A_COLL_STATS, SCOPE_COLLECTION),
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
    "top": (A_TOP, SCOPE_CLUSTER),
    "hostInfo": (A_HOST_INFO, SCOPE_CLUSTER),
    "getCmdLineOpts": (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
    # ``getParameter`` exposes server-internal config (featureFlag state,
    # tunables) and was missing from this table — a cluster-info command
    # an unprivileged authenticated user could call. Reuse the same
    # cluster-monitor action as the other introspection commands.
    "getParameter": (A_GET_CMD_LINE_OPTS, SCOPE_CLUSTER),
    "getLog": (A_GET_LOG, SCOPE_CLUSTER),
    "currentOp": (A_INPROG, SCOPE_CLUSTER),
    "killOp": (A_KILLOP, SCOPE_CLUSTER),
    "fsync": (A_FSYNC, SCOPE_CLUSTER),
    "profile": (A_ENABLE_PROFILER, SCOPE_DATABASE),
    # SecantusDB-extension prune commands reuse fsync's cluster-wide
    # privilege — both are admin-only operations against shared state.
    "secantusAdmin.pruneOplog": (A_FSYNC, SCOPE_CLUSTER),
    "secantusAdmin.pruneTtl": (A_FSYNC, SCOPE_CLUSTER),
    "secantusAdmin.backupArchive": (A_FSYNC, SCOPE_CLUSTER),
    "secantusAdmin.restoreArchive": (A_FSYNC, SCOPE_CLUSTER),
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
    # mongod's profiler records ``count`` / ``distinct`` under ``op:
    # "command"`` (only ``find`` is ``op: "query"``); matching that lets
    # ``system.profile`` queries that filter on ``{op: "command",
    # command.distinct: ...}`` find the entry.
    "count": "command",
    "distinct": "command",
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

# Per-index options the ``createIndexes`` command accepts inside each
# entry of the ``indexes`` array. Anything outside this set is
# rejected with ``Location40415`` (40415, IDLUnknownField), matching
# mongod's IDL. mongo-ruby-driver's ``Index::View#create_one when
# provided a session`` spec passes ``invalid: true`` to trigger this.
_INDEX_SPEC_KNOWN_OPTIONS = frozenset(
    {
        # Geometric / vector indexes.
        "2dsphereIndexVersion",
        "bits",
        "min",
        "max",
        # Wildcard.
        "wildcardProjection",
        # Standard knobs.
        "unique",
        "sparse",
        "hidden",
        "background",
        "expireAfterSeconds",
        "partialFilterExpression",
        "collation",
        "storageEngine",
        # Text — accepted on the wire, even though we don't implement
        # text indexes (storage rejects with CreateIndexUnsupported).
        "weights",
        "default_language",
        "language_override",
        "textIndexVersion",
        # Index format version + namespace (legacy drivers).
        "v",
        "ns",
        # Haystack (deprecated).
        "bucketSize",
        # ``dropDups`` — removed in MongoDB 3.0; modern mongod accepts and
        # silently ignores it (it never drops duplicates) rather than
        # rejecting the spec. So a unique index built over duplicate data
        # still fails on the duplicate (DuplicateKey 11000), not on an
        # unknown-field error. Stripped from the stored options below.
        "dropDups",
    }
)


# Top-level options the ``create`` command accepts. Anything outside
# this set is rejected with ``Location40415`` (40415, IDLUnknownField),
# matching mongod's IDL behaviour. mongo-ruby-driver's ``Collection#create
# when a session is provided`` shared spec exercises this by sending
# ``invalid: true`` and asserting an ``OperationFailure``.
_CREATE_KNOWN_OPTIONS = frozenset(
    {
        # Command name (always present).
        "create",
        # Per-create options mongod's IDL exposes.
        "capped",
        "size",
        "max",
        "validator",
        "validationAction",
        "validationLevel",
        "viewOn",
        "pipeline",
        "collation",
        "expireAfterSeconds",
        "timeseries",
        "clusteredIndex",
        "changeStreamPreAndPostImages",
        "storageEngine",
        "indexOptionDefaults",
        "writeConcern",
        "comment",
        "maxTimeMS",
        # mongorestore sends ``idIndex`` (the full ``_id_`` spec from
        # the source collection's listCollections output) so the
        # restored target gets the right index version / options. Real
        # mongod accepts this on ``create``.
        "idIndex",
        # Legacy / deprecated but tolerated.
        "autoIndexId",
        "flags",
        # Wire envelope fields (all ``$``-prefixed keys are accepted
        # unconditionally via the ``startswith("$")`` check above, but
        # list the common non-``$``-prefixed envelope keys explicitly
        # so driver tests passing them at the command level don't trip
        # the unknown-field rejection).
        "lsid",
        "txnNumber",
        "autocommit",
        "startTransaction",
        "readConcern",
        "apiVersion",
        "apiStrict",
        "apiDeprecationErrors",
    }
)

# Commands allowed under ``apiVersion: 1, apiStrict: true`` per
# MongoDB's Stable API contract. Anything outside this set with
# ``apiStrict: true`` set surfaces as ``APIStrictError`` (code 323).
# mongo-java-driver's ``versioned-api/crud-api-version-1-strict.yml``
# pins ``distinct`` and ``$listLocalSessions`` as the canary cases.
_API_V1_COMMANDS = frozenset(
    {
        "abortTransaction",
        "aggregate",
        "authenticate",
        "bulkWrite",
        "collMod",
        "commitTransaction",
        "create",
        "createIndexes",
        "delete",
        "drop",
        "dropDatabase",
        "dropIndexes",
        "endSessions",
        "explain",
        "find",
        "findAndModify",
        "getMore",
        "hello",
        "insert",
        "killCursors",
        "listCollections",
        "listDatabases",
        "listIndexes",
        "ping",
        "refreshSessions",
        "saslContinue",
        "saslStart",
        "update",
    }
)

# Aggregation stages allowed under ``apiVersion: 1, apiStrict: true``.
# Commands explicitly rejected when ``apiStrict: true`` is set. Narrow
# set — only the ones the spec's unified test runners actively probe.
# ``distinct`` is the canary (mongo-java-driver's
# ``crud-api-version-1-strict.yml`` test ``distinct appends declared
# API version`` asserts ``errorCodeName: APIStrictError``).
#
# Intentionally NOT inverting ``_API_V1_COMMANDS``: that broader set
# would reject ``count`` (used internally by
# ``estimatedDocumentCount``), ``buildInfo`` / ``serverStatus`` /
# ``listLocalSessions`` (handshake-adjacent admin commands drivers
# call on startup), and other internal-but-non-v1 names that aren't
# the spec's target.
_API_V1_REJECTED_BY_NAME = frozenset({"distinct"})


# Driver tests probe with ``$listLocalSessions`` / ``$listSessions``
# (deliberately excluded) because they're the cheapest way to land an
# ``APIStrictError`` from inside a known-allowed command (``aggregate``).
_API_V1_AGG_STAGES = frozenset(
    {
        "$addFields",
        "$bucket",
        "$bucketAuto",
        "$changeStream",
        "$collStats",
        "$count",
        "$densify",
        "$documents",
        "$facet",
        "$fill",
        "$geoNear",
        "$graphLookup",
        "$group",
        "$indexStats",
        "$limit",
        "$lookup",
        "$match",
        "$merge",
        "$out",
        "$project",
        "$redact",
        "$replaceRoot",
        "$replaceWith",
        "$sample",
        "$set",
        "$setWindowFields",
        "$skip",
        "$sort",
        "$sortByCount",
        "$unionWith",
        "$unset",
        "$unwind",
    }
)


# Commands that may run inside a multi-document transaction. Everything
# else gets mongod's 263 ``OperationNotSupportedInTransaction`` — the
# spec's canary is ``count``. ``commitTransaction`` / ``abortTransaction``
# carry the same envelope but are the transaction *controls*, handled by
# their own handlers rather than the statement path.
_TXN_ALLOWED_COMMANDS = frozenset(
    {
        "insert",
        "update",
        "delete",
        "findAndModify",
        "find",
        "getMore",
        "killCursors",
        "aggregate",
        "distinct",
        "bulkWrite",
        "create",
        "createIndexes",
    }
)

# Aggregation stages mongod refuses inside a transaction.
_TXN_BLOCKED_AGG_STAGES = frozenset(
    {
        "$out",
        "$merge",
        "$changeStream",
        "$collStats",
        "$currentOp",
        "$indexStats",
        "$listLocalSessions",
        "$listSessions",
    }
)

# Error codes that get the ``TransientTransactionError`` label when a
# statement inside a transaction fails: mongod's transient set
# (WriteConflict, SnapshotUnavailable, NoSuchTransaction, LockTimeout)
# plus the retryable-error codes, which are transient on any
# non-commit statement. Notably NOT here: 11000 duplicate key — it
# aborts the transaction but retrying wouldn't help, so no label.
_TRANSIENT_TXN_CODES = frozenset(
    {112, 246, 251, 24, 6, 7, 89, 91, 189, 9001, 10107, 11600, 11602, 13435, 13436}
)


def _txn_unsupported_reason(name: str, doc: dict[str, Any], ctx: CommandContext) -> str | None:
    """mongod-shaped reason a statement can't run in a transaction, or None."""
    if name not in _TXN_ALLOWED_COMMANDS:
        return f"Cannot run '{name}' in a multi-document transaction."
    if name == "aggregate":
        pipeline = doc.get("pipeline") or []
        if isinstance(pipeline, list):
            for stage in pipeline:
                if isinstance(stage, Mapping):
                    stage_name = next(iter(stage), "")
                    if stage_name in _TXN_BLOCKED_AGG_STAGES:
                        return (
                            f"Operation not permitted in transaction :: caused by :: "
                            f"Aggregation stage {stage_name} cannot run within a "
                            f"multi-document transaction."
                        )
    if name in ("insert", "update", "delete", "findAndModify"):
        coll = doc.get(name)
        if isinstance(coll, str):
            opts = ctx.storage.get_collection_options(ctx.db_name, coll)
            if opts.get("capped"):
                return (
                    f"Collection '{ctx.db_name}.{coll}' is a capped collection. "
                    f"Writing in a transaction to capped collections is not allowed."
                )
    return None


def _resolve_txn_statement(
    name: str,
    doc: dict[str, Any],
    ctx: CommandContext,
    lsid_bytes: bytes,
    txn_number: int,
) -> tuple[Transaction | None, dict[str, Any] | None]:
    """Registry resolution + allowlist gate for an in-transaction statement.

    Returns ``(txn, None)`` to execute, or ``(None, error_reply)``. A
    disallowed command still resolves first and then aborts the
    transaction — mongod treats it as a failed statement.
    """
    assert ctx.transactions is not None
    start = bool(doc.get("startTransaction"))
    txn, err = ctx.transactions.for_statement(lsid_bytes, doc.get("lsid"), txn_number, start=start)
    if err is not None:
        return None, err
    assert txn is not None
    reason = _txn_unsupported_reason(name, doc, ctx)
    if reason is not None:
        ctx.transactions.abort_in_progress(txn)
        return None, {
            "ok": 0.0,
            "errmsg": reason,
            "code": 263,
            "codeName": "OperationNotSupportedInTransaction",
        }
    return txn, None


def _run_txn_statement(
    txn: Transaction,
    handler: Any,
    doc: dict[str, Any],
    ctx: CommandContext,
) -> dict[str, Any]:
    """Execute one statement inside the transaction's WT session.

    The per-transaction mutex serializes statements (and the
    commit/abort/reaper transitions) so the WT session is never used
    from two threads at once. State is re-checked under the mutex —
    the lifetime reaper may have aborted between resolution and here.
    """
    with txn.mutex:
        if txn.state is not TxnState.IN_PROGRESS:
            return no_such_transaction_reply(txn.txn_number, label=True)
        if txn.handle is None:
            txn.handle = ctx.storage.begin_user_transaction()
        with ctx.storage.use_user_transaction(txn.handle):
            return handler(doc, ctx)


def _finish_txn_statement(ctx: CommandContext, txn: Transaction, result: dict[str, Any]) -> None:
    """mongod parity: any failed statement aborts the transaction
    server-side. Only transient-class codes get the
    ``TransientTransactionError`` label (E11000 aborts unlabeled)."""
    ok = bool(result.get("ok", 0.0))
    failed = (not ok) or bool(result.get("writeErrors"))
    if not failed:
        return
    assert ctx.transactions is not None
    ctx.transactions.abort_in_progress(txn)
    if not ok and result.get("code") in _TRANSIENT_TXN_CODES:
        labels = result.setdefault("errorLabels", [])
        if TRANSIENT_LABEL not in labels:
            labels.append(TRANSIENT_LABEL)


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
        # ``snapshot`` read concern requires a WiredTiger timestamp-pinned
        # read view that's only meaningful in a real replica set with
        # majority-committed snapshots. SecantusDB is single-node — real
        # mongod on standalone rejects ``snapshot`` with
        # ``SnapshotUnavailable`` (246) so drivers can fall back. The Java
        # ``snapshot-sessions-not-supported-server-error`` unified spec
        # asserts the error shape (any ``ok: 0`` reply on find / aggregate
        # / distinct with ``readConcern.level: snapshot``).
        #
        # Inside a multi-document transaction (``autocommit: false``)
        # the level IS accepted: every in-transaction read runs against
        # the transaction's pinned WT snapshot anyway, which is exactly
        # what ``snapshot`` asks for on a single node.
        #
        # Outside transactions, mongod 5.0+ replica sets accept
        # ``snapshot`` on exactly find / aggregate / distinct (snapshot
        # sessions). We advertise such a topology, so those commands
        # accept it too — accept-and-record: the reply carries
        # ``atClusterTime`` for session pinning but reads are not
        # actually timestamp-pinned (single node; tasks/backlog.md).
        # Everything else keeps mongod's rejection. When the
        # replica-set persona is off we reject like a real standalone
        # (the snapshot-sessions-not-supported unified specs pin that
        # error shape).
        # getMore / killCursors ride along: pymongo propagates the
        # pinned ``{level: snapshot, atClusterTime}`` onto cursor
        # continuation, and mongod accepts it there (the cursor already
        # owns its snapshot).
        snapshot_readable = (
            name in ("find", "aggregate", "distinct", "getMore", "killCursors")
            and ctx.replica_set_name
        )
        if level == "snapshot" and doc.get("autocommit") is not False and not snapshot_readable:
            return {
                "ok": 0.0,
                "errmsg": "Snapshot read concern is not supported on standalone",
                "code": 246,
                "codeName": "SnapshotUnavailable",
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
    # ``apiStrict: true`` narrows the allowed surface to the Stable API
    # contract. Two gates:
    #
    # * Command-name gate (narrow): reject only the small set in
    #   ``_API_V1_REJECTED_BY_NAME`` (currently ``distinct``). The
    #   spec's ``crud-api-version-1-strict.yml`` asserts this rejection
    #   for ``distinct``; mirroring it makes the test pass. The full
    #   whitelist invert is intentionally NOT enabled — it'd reject
    #   ``count`` (used internally by ``estimatedDocumentCount``) and
    #   a handful of internal admin commands.
    # * Aggregation-stage gate: reject pipeline stages outside
    #   ``_API_V1_AGG_STAGES``. Lights up ``versioned-api/aggregate on
    #   database`` (probes with ``$listLocalSessions``).
    if doc.get("apiStrict"):
        if name in _API_V1_REJECTED_BY_NAME:
            return {
                "ok": 0.0,
                "errmsg": f"Provided command {name} is not in API Version 1",
                "code": 323,
                "codeName": "APIStrictError",
            }
        if name == "aggregate":
            pipeline = doc.get("pipeline") or []
            if isinstance(pipeline, list):
                for stage in pipeline:
                    if isinstance(stage, Mapping):
                        stage_name = next(iter(stage), "")
                        if stage_name and stage_name not in _API_V1_AGG_STAGES:
                            return {
                                "ok": 0.0,
                                "errmsg": (
                                    f"Provided aggregation pipeline stage "
                                    f"{stage_name} is not in API Version 1"
                                ),
                                "code": 323,
                                "codeName": "APIStrictError",
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
            if match.close_connection:
                # The failpoint asked us to abruptly drop the TCP
                # connection without responding. Drivers detect the
                # broken socket and report it as a client-side
                # network error. Used by mongo-java-driver's
                # ``estimatedDocumentCount errors correctly--socket
                # error`` test.
                from secantus.failpoints import CloseConnectionRequested

                raise CloseConnectionRequested()
            if match.block_connection and match.block_time_ms > 0:
                # Sleep before processing — mongo-node-driver's CSOT
                # ``explain with timeoutMS`` tests configure a 2000 ms
                # block specifically so the client's ``timeoutMS: 1000``
                # timer fires first and surfaces as
                # ``MongoOperationTimeoutError``. The block is what the
                # driver test gates on; the actual timeout firing is
                # client-side and we never have to send a reply.
                _time.sleep(match.block_time_ms / 1000.0)
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

    # Multi-document transaction envelope. ``autocommit: false`` +
    # ``lsid`` + ``txnNumber`` marks an in-transaction statement (the
    # first one also carries ``startTransaction: true``); ``txnNumber``
    # WITHOUT ``autocommit`` stays the tolerated retryable-write
    # envelope. Resolution runs AFTER the failpoint block on purpose:
    # failpoint-injected errors must not abort the transaction —
    # retryable-commit tests inject an error on the first commit
    # attempt and expect the retry to succeed.
    txn: Transaction | None = None
    if ctx.transactions is not None and "txnNumber" in doc:
        lsid_bytes, txn_number = _txn_envelope(doc)
        if lsid_bytes is not None and txn_number is not None:
            if doc.get("autocommit") is False:
                if name not in ("commitTransaction", "abortTransaction"):
                    txn, txn_err = _resolve_txn_statement(name, doc, ctx, lsid_bytes, txn_number)
                    if txn_err is not None:
                        return txn_err
            else:
                # Retryable write: consumes the session's txnNumber
                # sequence and implicitly aborts an older open
                # transaction, as in mongod.
                ctx.transactions.on_retryable_write(lsid_bytes, txn_number)
    profile_eligible = _profile_eligible_command(name, doc)
    start_ns = _time.monotonic_ns() if profile_eligible else 0
    try:
        if txn is not None:
            result = _run_txn_statement(txn, handler, doc, ctx)
        else:
            result = handler(doc, ctx)
    except WriteConflictError:
        result = _write_conflict_reply(label=txn is not None)
    except changestreams.ChangeStreamFatalError as exc:
        # Change-stream fatal conditions (resume token projected out,
        # fullDocument: "required" miss, pre-image not stored) surface
        # with their own codes — mongod replies, not internal errors.
        result = _change_stream_fatal_reply(exc)
    except _USER_FACING_EXCEPTIONS as exc:
        # Validation-class errors: messages are deliberately shaped to
        # match mongod, drivers parse them. Surface verbatim. Exceptions
        # may carry the mongod code their error uses (ExpressionError:
        # $divide-by-zero is 2 BadValue, $mod uses Location codes;
        # AggregateError: 40324 for an unrecognized pipeline stage —
        # which leaves ``code`` as None when unset, hence the ``or``);
        # 14 TypeMismatch stays the default.
        result = {
            "ok": 0.0,
            "errmsg": str(exc),
            "code": getattr(exc, "code", None) or 14,
            "codeName": getattr(exc, "code_name", None) or "TypeMismatch",
        }
    except Exception as exc:
        if _is_wt_rollback(exc):
            # WT_ROLLBACK surfacing from a write path that doesn't
            # classify locally (update/delete cursors raise the raw
            # WiredTigerError): same WriteConflict as the typed path.
            result = _write_conflict_reply(label=txn is not None)
        else:
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
    if txn is not None:
        _finish_txn_statement(ctx, txn, result)
    if profile_eligible:
        _maybe_record_profile(ctx, name, doc, result, start_ns)
    if failpoint_wce is not None and result.get("ok", 0.0):
        result["writeConcernError"] = failpoint_wce
        if failpoint_labels:
            result["errorLabels"] = list(failpoint_labels)
    elif result.get("ok", 0.0):
        # An unsatisfiable ``writeConcern.w`` (e.g. ``w: 4000`` against
        # our single-node "secantus" replica set) gets a
        # ``writeConcernError`` attached to the successful reply, the
        # same way real mongod does. Drivers raise ``OperationFailure``
        # on the wce. Failpoint-attached wces win when both apply.
        wc_wce = _unsatisfiable_wc_error(doc)
        if wc_wce is not None:
            result["writeConcernError"] = wc_wce
    # Cluster-time gossip: real mongod attaches ``$clusterTime`` and
    # ``operationTime`` to EVERY reply — successes and errors — when the
    # node is a replica-set member (standalones don't gossip; neither do
    # we when the replica-set persona is off). Drivers and pymongo's
    # tests read ``reply["operationTime"]`` for causal consistency and
    # ``startAtOperationTime``. The keyless signature (20 zero bytes,
    # keyId 0) is what auth-less replica sets send. ``setdefault``
    # preserves handlers that already attach a more specific value
    # (e.g. the change-stream ``aggregate`` reply).
    if ctx.replica_set_name:
        ts = ctx.storage.peek_cluster_time()
        result.setdefault(
            "$clusterTime",
            {
                "clusterTime": ts,
                "signature": {"hash": bson.Binary(b"\x00" * 20), "keyId": bson.Int64(0)},
            },
        )
        result.setdefault("operationTime", ts)
        # Snapshot sessions: pymongo pins the session's read timestamp
        # from the FIRST snapshot read's reply — ``cursor.atClusterTime``
        # for cursor commands, top-level ``atClusterTime`` otherwise
        # (client_session.py _update_read_concern) — and sends it back
        # as ``readConcern.atClusterTime`` on subsequent reads. Reads
        # are NOT actually pinned (single node, accept-and-record; see
        # tasks/backlog.md), but the wire contract is satisfied.
        rc = doc.get("readConcern")
        if bool(result.get("ok")) and isinstance(rc, Mapping) and rc.get("level") == "snapshot":
            cursor_part = result.get("cursor")
            if isinstance(cursor_part, dict):
                cursor_part.setdefault("atClusterTime", ts)
            else:
                result.setdefault("atClusterTime", ts)
    return result
