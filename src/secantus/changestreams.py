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
    # True when the token came from an invalidate event. mongod encodes
    # ``fromInvalidate`` in its keystring tokens; ``resumeAfter``
    # rejects such tokens (only ``startAfter`` may pass one).
    from_invalidate: bool = False


def make_resume_token(data: ResumeTokenData) -> dict[str, str]:
    inner_doc: dict[str, Any] = {
        "s": data.seq,
        "t": data.ts,
        "n": data.ns,
        "k": data.document_key,
    }
    if data.from_invalidate:
        # Only stamped when set so pre-existing tokens stay parseable
        # and byte-identical.
        inner_doc["i"] = True
    inner = bson.encode(inner_doc)
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
        from_invalidate=bool(inner.get("i", False)),
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
    if event.get("operationType") == "replace":
        # Replacement-style updates emit the full new doc as `o` (mirroring
        # mongod). The change-stream contract says replace events always
        # carry fullDocument; no separate updateLookup is required.
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
        # ``required`` / ``whenAvailable`` read the stored POST-image
        # (mongod 6.0 semantics): the collection must have
        # ``changeStreamPreAndPostImages`` enabled, else ``required``
        # errors and ``whenAvailable`` yields null. Only
        # ``updateLookup`` does a live point-in-time-less lookup. With
        # images enabled we approximate the post-image with the live
        # lookup — exact on a single node unless later writes already
        # changed the doc (recorded divergence, tasks/backlog.md).
        if full_document_mode in (
            FULL_DOC_REQUIRED,
            FULL_DOC_WHEN_AVAILABLE,
        ) and not storage._pre_post_images_enabled(db, coll):
            if full_document_mode == FULL_DOC_REQUIRED:
                raise ChangeStreamFatalError(
                    "the 'fullDocument: required' option requires "
                    "changeStreamPreAndPostImages to be enabled on the "
                    f"collection {ns}"
                )
            event["fullDocument"] = None
            return
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
    show_expanded_events: bool = False,
) -> tuple[dict[str, Any] | None, bool]:
    """Return ``(event, invalidates_after)`` for an oplog row.

    ``event`` is None if the row is not surfaced through change streams
    (scope mismatch, or an "expanded" DDL event when the user did NOT
    pass ``showExpandedEvents: true``). ``invalidates_after`` is True
    if the cursor should produce one final invalidate event after this
    one (drop on a watched collection, etc.).

    ``show_expanded_events`` is a mongod 6.0+ opt-in: when False (the
    default, matching mongod), ``createIndexes`` / ``dropIndexes`` /
    ``modify`` / ``shardCollection`` / ``reshardCollection`` /
    ``refineCollectionShardKey`` events are suppressed. Drop /
    dropDatabase / rename surface unconditionally — they're the
    invalidation triggers the v1 spec requires and have always been
    "non-expanded" events.
    """
    op = str(oplog_entry.get("op", ""))
    ns = str(oplog_entry.get("ns", ""))
    ts = oplog_entry.get("ts")
    wall = oplog_entry.get("wall")
    if not isinstance(ts, Timestamp):
        return None, False
    # Periodic noop heartbeats (``op: "n"``) advance cluster time
    # without surfacing as user events. Skip the projection — the
    # caller still bumps ``position_seq`` past them, so resume tokens
    # on quiet collections track the heartbeat clock.
    if op == "n":
        return None, False
    if op in {"i", "u", "d"}:
        if not _scope_matches(ns, scope):
            return None, False
        document_key = dict(oplog_entry.get("o2", {"_id": None}))
        # `op:"u"` carries either an operator-style diff (`o = {$v: 2, diff: {...}}`)
        # or a full replacement doc (`o = {_id: ..., ...}` with no `$v`/`diff`).
        # Mongod uses the same op code for both; the change stream distinguishes
        # them as `update` vs `replace`. A diff-shaped `o` always has `$v` set,
        # so absence of `$v` is the signal for replacement.
        op_type = {"i": "insert", "u": "update", "d": "delete"}[op]
        if op == "u":
            o = oplog_entry.get("o", {})
            if isinstance(o, Mapping) and "$v" not in o and "diff" not in o:
                op_type = "replace"
        token = make_resume_token(ResumeTokenData(seq, ts, ns, document_key))
        event: dict[str, Any] = {
            "_id": token,
            "operationType": op_type,
            "clusterTime": ts,
            "ns": _ns_doc(ns),
            "documentKey": document_key,
        }
        # mongod 6.0+ attaches the collection's UUID to CRUD events when the
        # stream was opened with ``showExpandedEvents``. The oplog row carries
        # it as ``ui`` (Binary subtype 4).
        if show_expanded_events and oplog_entry.get("ui") is not None:
            event["collectionUUID"] = oplog_entry["ui"]
        if isinstance(wall, object) and wall is not None:
            event["wallTime"] = wall
        # Writes that happened inside a multi-document transaction carry
        # the session/transaction identity on their oplog entries; mongod
        # surfaces both on the change event.
        if "lsid" in oplog_entry:
            event["lsid"] = dict(oplog_entry["lsid"])
        if "txnNumber" in oplog_entry:
            event["txnNumber"] = oplog_entry["txnNumber"]
        if op == "u" and op_type == "update":
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
            # mongod (probed 7.0.12): under showExpandedEvents the update
            # description always carries ``disambiguatedPaths`` — an empty
            # document when nothing was ambiguous (the diff writer only stores
            # the key when non-empty); without the flag the key is absent.
            if show_expanded_events:
                event["updateDescription"].setdefault("disambiguatedPaths", {})
            else:
                event["updateDescription"].pop("disambiguatedPaths", None)
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
            to_doc = {"db": to_db, "coll": to_coll} if to_coll else {"db": to_db}
            event = {
                "_id": token,
                "operationType": "rename",
                "clusterTime": ts,
                "ns": _ns_doc(from_ns),
                "to": to_doc,
            }
            if show_expanded_events:
                # mongod 6.0+ attaches an ``operationDescription`` to expanded
                # rename events: the ``to`` namespace plus ``dropTarget`` (the
                # dropped target collection's UUID) when the rename replaced an
                # existing collection.
                op_desc: dict[str, Any] = {"to": to_doc}
                if "dropTarget" in cmd:
                    op_desc["dropTarget"] = cmd["dropTarget"]
                event["operationDescription"] = op_desc
            if wall is not None:
                event["wallTime"] = wall
            invalidates = scope.get("kind") == "coll"
            return event, invalidates
        if "create" in cmd:
            if not show_expanded_events:
                return None, False
            affected_ns = f"{cmd_db}.{cmd['create']}"
            if not _scope_matches(affected_ns, scope):
                return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
            # ``operationDescription`` carries the collection-creation options
            # (idIndex for an ordinary collection, viewOn / pipeline for a
            # view) — everything in the command spec except the collection
            # name under ``create``.
            op_desc = {k: v for k, v in cmd.items() if k != "create"}
            event = {
                "_id": token,
                "operationType": "create",
                "clusterTime": ts,
                "ns": _ns_doc(affected_ns),
                "operationDescription": op_desc,
            }
            if wall is not None:
                event["wallTime"] = wall
            return event, False
        if "collMod" in cmd:
            if not show_expanded_events:
                return None, False
            affected_ns = f"{cmd_db}.{cmd['collMod']}"
            if not _scope_matches(affected_ns, scope):
                return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
            event = {
                "_id": token,
                "operationType": "modify",
                "clusterTime": ts,
                "ns": _ns_doc(affected_ns),
                "operationDescription": {k: v for k, v in cmd.items() if k != "collMod"},
            }
            if wall is not None:
                event["wallTime"] = wall
            return event, False
        if "createIndexes" in cmd:
            if not show_expanded_events:
                return None, False
            affected_ns = f"{cmd_db}.{cmd['createIndexes']}"
            if not _scope_matches(affected_ns, scope):
                return None, False
            indexes = cmd.get("indexes") or []
            # mongod emits one event per index in a multi-index createIndexes
            # call. We mirror that: surface the *first* index's spec on this
            # event and rely on the oplog entry split (real mongod writes one
            # oplog entry per index too, so when SecantusDB emits multi-
            # index createIndexes, future iterations should split it).
            # Today our storage layer only ever creates one index per
            # createIndexes call, so the loop below collapses to one event.
            spec = indexes[0] if isinstance(indexes, list) and indexes else {}
            token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
            event = {
                "_id": token,
                "operationType": "createIndexes",
                "clusterTime": ts,
                "ns": _ns_doc(affected_ns),
                "operationDescription": {
                    "indexes": [
                        {
                            "v": spec.get("v", 2),
                            "key": dict(spec.get("key", {})),
                            "name": spec.get("name", ""),
                        }
                    ]
                    if spec
                    else []
                },
            }
            if wall is not None:
                event["wallTime"] = wall
            return event, False
        if "dropIndexes" in cmd:
            if not show_expanded_events:
                return None, False
            affected_ns = f"{cmd_db}.{cmd['dropIndexes']}"
            if not _scope_matches(affected_ns, scope):
                return None, False
            token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}))
            event = {
                "_id": token,
                "operationType": "dropIndexes",
                "clusterTime": ts,
                "ns": _ns_doc(affected_ns),
                # mongod (probed 7.0.12) describes the dropped index in full.
                # ``key`` rides in the oplog row; a legacy row without it
                # degrades to the name-only shape.
                "operationDescription": {
                    "indexes": [
                        {"v": 2, "key": dict(cmd["key"]), "name": cmd.get("index", "")}
                        if isinstance(cmd.get("key"), Mapping) and cmd.get("key")
                        else {"name": cmd.get("index", "")}
                    ]
                },
            }
            if wall is not None:
                event["wallTime"] = wall
            return event, False
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
    token = make_resume_token(ResumeTokenData(seq, ts, affected_ns, {}, from_invalidate=True))
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
    split_large_events: bool = False
    show_expanded_events: bool = False


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
    if bool(spec.get("splitLargeChangeStreamEvents")):
        out.split_large_events = True
    if bool(spec.get("showExpandedEvents")):
        out.show_expanded_events = True
    return out


_SPLIT_THRESHOLD_BYTES = 16 * 1024 * 1024
_HEAVY_FIELD_BYTES = 1024 * 1024


def stamp_split_event(event: dict[str, Any]) -> list[dict[str, Any]]:
    """Split ``event`` into fragments when its BSON-encoded size
    exceeds 16 MB; tag each fragment with ``splitEvent: {fragment:
    N, of: M}``. Returns a list of one event (no split needed) or
    N events (split). Always returns at least one event.

    Real mongod splits change events larger than 16 MB when the
    user sets ``splitLargeChangeStreamEvents: true``; each fragment
    is itself a valid change-stream event the driver can process
    independently or reassemble (drivers identify fragments by
    shared ``_id`` resume token and combine fields).

    Strategy is size-based, not field-name-based: any single
    top-level field whose BSON size exceeds ``_HEAVY_FIELD_BYTES``
    (1 MB) is a "heavy" field that goes into its own fragment.
    All "light" fields (resume token, operationType, clusterTime,
    ns, documentKey, wallTime, …) are copied verbatim into every
    fragment so each is a valid change-stream event.

    The practical event shapes that trigger this:

    * ``update`` with ``fullDocumentBeforeChange: required`` and a
      large pre-image PLUS a large ``$set`` value — both
      ``fullDocumentBeforeChange`` (~10 MB) and
      ``updateDescription.updatedFields`` (~10 MB) qualify as heavy,
      so the event splits into 2 fragments.
    * ``update`` with both ``fullDocument`` and
      ``fullDocumentBeforeChange`` present and large.

    When the event is small enough, no split: a single fragment is
    emitted with ``{fragment: 1, of: 1}`` — the user's opt-in is
    honoured by the *presence* of the ``splitEvent`` field;
    drivers don't get back a ``splitEvent`` when the option is off.
    """
    encoded_size = len(bson.encode(event))
    if encoded_size <= _SPLIT_THRESHOLD_BYTES:
        event["splitEvent"] = {"fragment": 1, "of": 1}
        return [event]

    # Identify heavy fields by per-field BSON encoding.
    heavy: list[str] = []
    light: list[str] = []
    for k, v in event.items():
        if len(bson.encode({k: v})) > _HEAVY_FIELD_BYTES:
            heavy.append(k)
        else:
            light.append(k)

    if not heavy:
        # Event > 16 MB but no individual heavy field — punt with
        # a single fragment (driver may surface an OverBson16M
        # error, but we've done what we can).
        event["splitEvent"] = {"fragment": 1, "of": 1}
        return [event]

    light_metadata = {k: event[k] for k in light}
    fragments: list[dict[str, Any]] = []
    for hf in heavy:
        frag = dict(light_metadata)
        frag[hf] = event[hf]
        fragments.append(frag)
    total = len(fragments)
    for i, frag in enumerate(fragments, 1):
        frag["splitEvent"] = {"fragment": i, "of": total}
    return fragments


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
    "stamp_split_event",
]
