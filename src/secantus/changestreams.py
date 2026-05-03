"""Change-stream projection: oplog entries → MongoDB change events.

The shape of a change event matches what real ``mongod`` emits over a
change stream — ``_id`` is an opaque resume token, plus ``operationType``,
``clusterTime``, ``wallTime``, ``ns``, ``documentKey``, and (per-op)
``fullDocument`` / ``fullDocumentBeforeChange`` / ``updateDescription``.

Resume tokens are ``{"_data": "<hex>"}`` where the hex bytes are a BSON
document carrying ``{s: seq, t: ts, n: ns, k: documentKey._id}``. PyMongo
treats the token as opaque and round-trips ``_data`` verbatim, so the
internal layout is up to us — we use it on resume to seek the oplog past
``seq``, validate the namespace for ``startAfter`` semantics, and detect
the invalidate boundary.

Pre-image / post-image lookup is the only I/O: ``project`` takes a
``Storage`` and may call ``find_matching`` for ``fullDocument:
"updateLookup"`` or ``read_preimage`` for ``fullDocumentBeforeChange``.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import bson
from bson.timestamp import Timestamp

if TYPE_CHECKING:
    from secantus.storage import Storage


# fullDocument / fullDocumentBeforeChange modes. Default = no full doc.
FULL_DOC_DEFAULT = "default"
FULL_DOC_UPDATE_LOOKUP = "updateLookup"
FULL_DOC_REQUIRED = "required"
FULL_DOC_WHEN_AVAILABLE = "whenAvailable"
FULL_DOC_OFF = "off"


class ChangeStreamHistoryLost(Exception):
    """Resume token references oplog history that has been pruned (code 286)."""

    code = 286
    codeName = "ChangeStreamHistoryLost"


class ChangeStreamFatalError(Exception):
    """``fullDocument: "required"`` lookup missed (code 280)."""

    code = 280
    codeName = "ChangeStreamFatalError"


@dataclass
class ResumeTokenData:
    seq: int
    ts: Timestamp
    ns: str
    document_key: dict[str, Any]


def make_resume_token(data: ResumeTokenData) -> dict[str, str]:
    inner = bson.encode(
        {
            "s": data.seq,
            "t": data.ts,
            "n": data.ns,
            "k": data.document_key,
        }
    )
    return {"_data": inner.hex()}


def parse_resume_token(token: Mapping[str, Any]) -> ResumeTokenData:
    if not isinstance(token, Mapping) or "_data" not in token:
        raise ValueError("resume token missing _data")
    raw = token["_data"]
    if not isinstance(raw, str):
        raise ValueError("resume token _data must be a hex string")
    inner = bson.decode(bytes.fromhex(raw))
    ts = inner.get("t")
    if not isinstance(ts, Timestamp):
        raise ValueError("resume token has invalid timestamp")
    return ResumeTokenData(
        seq=int(inner["s"]),
        ts=ts,
        ns=str(inner["n"]),
        document_key=dict(inner.get("k", {})),
    )


def _split_ns(ns: str) -> tuple[str, str]:
    if "." in ns:
        db, coll = ns.split(".", 1)
        return db, coll
    return ns, ""


def _scope_matches(ns: str, scope: Mapping[str, Any]) -> bool:
    """Whether the oplog ns falls inside the watch scope.

    ``scope`` is one of: ``{"kind": "cluster"}``, ``{"kind": "db", "db": ...}``,
    ``{"kind": "coll", "db": ..., "coll": ...}``.
    """
    kind = scope.get("kind")
    if kind == "cluster":
        return True
    db, coll = _split_ns(ns)
    if kind == "db":
        return db == scope.get("db")
    if kind == "coll":
        return db == scope.get("db") and coll == scope.get("coll")
    return False


def _ns_doc(ns: str) -> dict[str, str]:
    db, coll = _split_ns(ns)
    out: dict[str, str] = {"db": db}
    if coll and coll != "$cmd":
        out["coll"] = coll
    return out


def _do_lookup(storage: Storage, db: str, coll: str, doc_id: Any) -> dict[str, Any] | None:
    docs = storage.find_matching(db, coll, {"_id": doc_id}, limit=1)
    return docs[0] if docs else None


def _attach_full_document(
    event: dict[str, Any],
    op: str,
    oplog_entry: Mapping[str, Any],
    *,
    storage: Storage,
    full_document_mode: str,
) -> None:
    if op == "i":
        # Inserts already carry the full doc.
        if full_document_mode != FULL_DOC_OFF:
            event["fullDocument"] = dict(oplog_entry["o"])
        return
    if op == "u" and full_document_mode in (
        FULL_DOC_UPDATE_LOOKUP,
        FULL_DOC_REQUIRED,
        FULL_DOC_WHEN_AVAILABLE,
    ):
        ns = str(oplog_entry.get("ns", ""))
        db, coll = _split_ns(ns)
        doc_id = oplog_entry.get("o2", {}).get("_id")
        looked_up = _do_lookup(storage, db, coll, doc_id) if doc_id is not None else None
        if looked_up is not None:
            event["fullDocument"] = looked_up
        elif full_document_mode == FULL_DOC_REQUIRED:
            raise ChangeStreamFatalError(
                f"fullDocument required but document with _id={doc_id!r} not found"
            )
        else:
            event["fullDocument"] = None


def _attach_full_document_before_change(
    event: dict[str, Any],
    seq: int,
    *,
    storage: Storage,
    mode: str,
) -> None:
    if mode in (FULL_DOC_DEFAULT, FULL_DOC_OFF, ""):
        return
    pre = storage.read_preimage(seq)
    if pre is not None:
        event["fullDocumentBeforeChange"] = pre
    elif mode == FULL_DOC_REQUIRED:
        raise ChangeStreamFatalError("fullDocumentBeforeChange required but pre-image not stored")
    elif mode == FULL_DOC_WHEN_AVAILABLE:
        event["fullDocumentBeforeChange"] = None


def project(
    seq: int,
    oplog_entry: Mapping[str, Any],
    *,
    storage: Storage,
    full_document_mode: str = FULL_DOC_DEFAULT,
    full_document_before_change_mode: str = FULL_DOC_DEFAULT,
    scope: Mapping[str, Any],
) -> tuple[dict[str, Any] | None, bool]:
    """Return ``(event, invalidates_after)`` for an oplog row.

    ``event`` is None if the row is not surfaced through change streams
    (e.g. ``createIndexes``/``dropIndexes`` in v1, or scope mismatches).
    ``invalidates_after`` is True if the cursor should produce one final
    invalidate event after this one (drop on a watched collection, etc.).
    """
    op = str(oplog_entry.get("op", ""))
    ns = str(oplog_entry.get("ns", ""))
    ts = oplog_entry.get("ts")
    wall = oplog_entry.get("wall")
    if not isinstance(ts, Timestamp):
        return None, False
    if op in {"i", "u", "d"}:
        if not _scope_matches(ns, scope):
            return None, False
        document_key = dict(oplog_entry.get("o2", {"_id": None}))
        op_type = {"i": "insert", "u": "update", "d": "delete"}[op]
        token = make_resume_token(ResumeTokenData(seq, ts, ns, document_key))
        event: dict[str, Any] = {
            "_id": token,
            "operationType": op_type,
            "clusterTime": ts,
            "ns": _ns_doc(ns),
            "documentKey": document_key,
        }
        if isinstance(wall, object) and wall is not None:
            event["wallTime"] = wall
        if op == "u":
            o = oplog_entry.get("o", {})
            diff = o.get("diff") if isinstance(o, Mapping) else None
            if isinstance(diff, Mapping):
                event["updateDescription"] = dict(diff)
            else:
                event["updateDescription"] = {
                    "updatedFields": {},
                    "removedFields": [],
                    "truncatedArrays": [],
                }
        _attach_full_document(
            event, op, oplog_entry, storage=storage, full_document_mode=full_document_mode
        )
        _attach_full_document_before_change(
            event, seq, storage=storage, mode=full_document_before_change_mode
        )
        return event, False
    if op == "c":
        cmd = oplog_entry.get("o", {}) if isinstance(oplog_entry.get("o"), Mapping) else {}
        # Determine event ns (the affected namespace) from the command spec.
        cmd_db, _cmd_dollar = _split_ns(ns)
        if "drop" in cmd:
            affected_ns = f"{cmd_db}.{cmd['drop']}"
            if not _scope_matches(affected_ns, scope):
                return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
            event = {
                "_id": token,
                "operationType": "drop",
                "clusterTime": ts,
                "ns": _ns_doc(affected_ns),
            }
            if wall is not None:
                event["wallTime"] = wall
            invalidates = scope.get("kind") == "coll" and _scope_matches(affected_ns, scope)
            return event, invalidates
        if "dropDatabase" in cmd:
            if scope.get("kind") not in ("db", "cluster"):
                return None, False
            if scope.get("kind") == "db" and scope.get("db") != cmd_db:
                return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, ns, {}))
            event = {
                "_id": token,
                "operationType": "dropDatabase",
                "clusterTime": ts,
                "ns": {"db": cmd_db},
            }
            if wall is not None:
                event["wallTime"] = wall
            invalidates = scope.get("kind") == "db"
            return event, invalidates
        if "renameCollection" in cmd:
            from_ns = str(cmd["renameCollection"])
            to_ns = str(cmd.get("to", ""))
            if not _scope_matches(from_ns, scope):
                # Rename of a different collection — surface only at db/cluster scope.
                if scope.get("kind") in ("db", "cluster") and _scope_matches(from_ns, scope):
                    pass
                else:
                    return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, from_ns, {}))
            to_db, to_coll = _split_ns(to_ns)
            event = {
                "_id": token,
                "operationType": "rename",
                "clusterTime": ts,
                "ns": _ns_doc(from_ns),
                "to": {"db": to_db, "coll": to_coll} if to_coll else {"db": to_db},
            }
            if wall is not None:
                event["wallTime"] = wall
            invalidates = scope.get("kind") == "coll"
            return event, invalidates
        # createIndexes / dropIndexes: deferred (filtered out per backlog).
        return None, False
    return None, False


def invalidate_event(seq: int, oplog_entry: Mapping[str, Any]) -> dict[str, Any]:
    ts = oplog_entry.get("ts") or Timestamp(0, 0)
    ns = str(oplog_entry.get("ns", ""))
    cmd = oplog_entry.get("o", {}) if isinstance(oplog_entry.get("o"), Mapping) else {}
    if "drop" in cmd:
        affected_ns = f"{_split_ns(ns)[0]}.{cmd['drop']}"
    elif "renameCollection" in cmd:
        affected_ns = str(cmd["renameCollection"])
    else:
        affected_ns = ns
    token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
    event = {
        "_id": token,
        "operationType": "invalidate",
        "clusterTime": ts,
    }
    wall = oplog_entry.get("wall")
    if wall is not None:
        event["wallTime"] = wall
    return event


@dataclass
class ChangeStreamSpec:
    """What ``$changeStream`` parsed from the user's pipeline.

    Stashed on ``PipelineContext.change_stream`` by the stage so the
    aggregate handler can build a producer closure for the cursor.
    """

    full_document_mode: str = FULL_DOC_DEFAULT
    full_document_before_change_mode: str = FULL_DOC_DEFAULT
    resume_after: dict[str, Any] | None = None
    start_after: dict[str, Any] | None = None
    start_at_operation_time: Timestamp | None = None


def parse_spec(spec: Mapping[str, Any]) -> ChangeStreamSpec:
    out = ChangeStreamSpec()
    fd = spec.get("fullDocument")
    if isinstance(fd, str):
        out.full_document_mode = fd
    fdb = spec.get("fullDocumentBeforeChange")
    if isinstance(fdb, str):
        out.full_document_before_change_mode = fdb
    if isinstance(spec.get("resumeAfter"), Mapping):
        out.resume_after = dict(spec["resumeAfter"])
    if isinstance(spec.get("startAfter"), Mapping):
        out.start_after = dict(spec["startAfter"])
    if isinstance(spec.get("startAtOperationTime"), Timestamp):
        out.start_at_operation_time = spec["startAtOperationTime"]
    return out


__all__ = [
    "ChangeStreamFatalError",
    "ChangeStreamHistoryLost",
    "ChangeStreamSpec",
    "FULL_DOC_DEFAULT",
    "FULL_DOC_OFF",
    "FULL_DOC_REQUIRED",
    "FULL_DOC_UPDATE_LOOKUP",
    "FULL_DOC_WHEN_AVAILABLE",
    "ResumeTokenData",
    "invalidate_event",
    "make_resume_token",
    "parse_resume_token",
    "parse_spec",
    "project",
]
