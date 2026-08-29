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
    """A change stream cannot continue — e.g. the pipeline projected out
    ``_id``, so no resume token can be built (measured against mongod
    6.0.16: code 280, with the non-resumable label)."""

    code = 280
    codeName = "ChangeStreamFatalError"
    error_labels: tuple[str, ...] = ("NonResumableChangeStreamError",)


class ChangeStreamRequiredImageError(ChangeStreamFatalError):
    """``fullDocument`` / ``fullDocumentBeforeChange`` was ``"required"``
    and the image was not available.

    A *different* condition from the one above and mongod answers it
    differently: measured against 6.0.16 on 2026-08-29 it is code 47
    ``NoMatchingDocument`` with **no** error labels, not 280. Both cases
    used to share 280 here, which is why this is a subclass — the
    getMore path catches the base class and the reply is shaped from
    whichever code/labels the instance carries."""

    code = 47
    codeName = "NoMatchingDocument"
    error_labels: tuple[str, ...] = ()


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
    try:
        decoded = bytes.fromhex(raw)
    except ValueError:
        # mongod's wording, which names the problem rather than quoting
        # Python's ``fromhex()`` complaint back at the client.
        raise ValueError("resume token string was not a valid hex string") from None
    try:
        inner = bson.decode(decoded)
    except Exception:
        # Valid hex that is not valid BSON -- e.g. ``{"_data": "aa"}``, two hex
        # digits that decode to one byte. This raised ``InvalidBSON`` straight
        # out of the handler and escaped as "internal server error" (code 1).
        raise ValueError("resume token is not a valid resume token") from None
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


# mongod's field order for a change event, measured against 6.0.16 on
# 2026-08-29 with tools/probes/change_streams.py. Order is invisible to a dict
# comparison, so it survived every equality-based check we had: the probe
# compares key LISTS, and found 28 of 34 CRUD cases out of order. Any key not
# listed here keeps its relative position after these.
_EVENT_FIELD_ORDER = (
    "_id",
    "operationType",
    "clusterTime",
    "collectionUUID",
    "wallTime",
    "fullDocument",
    "ns",
    "to",
    "documentKey",
    "operationDescription",
    "updateDescription",
    "stateBeforeChange",
    "fullDocumentBeforeChange",
)


def _order_event_fields(event: dict[str, Any]) -> dict[str, Any]:
    """Rebuild an event with mongod's field order.

    Applied once, at the end of projection, rather than by constructing every
    event type in the right order — there are nine construction sites and they
    drifted apart precisely because nothing checked them."""
    known = [k for k in _EVENT_FIELD_ORDER if k in event]
    rest = [k for k in event if k not in _EVENT_FIELD_ORDER]
    return {k: event[k] for k in known + rest}


def _do_lookup(storage: Storage, db: str, coll: str, doc_id: Any) -> dict[str, Any] | None:
    docs = storage.find_matching(db, coll, {"_id": doc_id}, limit=1)
    return docs[0] if docs else None


def _set_full_document(event: dict[str, Any], value: Any) -> None:
    """Set ``fullDocument``; placement is handled by [`_order_event_fields`].

    This used to hand-place the key immediately after ``operationType``, which
    was measured wrong twice over against mongod 6.0.16: mongod puts
    ``fullDocument`` after ``wallTime``, and the hoisting also pushed ``_id``
    out of first position."""
    event["fullDocument"] = value


def _required_image_message(event: Mapping[str, Any], *, pre: bool) -> str:
    """mongod's wording for a missing required pre-/post-image.

    Verbatim from 6.0.16, including the ``Executor error during getMore``
    wrapper it adds when the condition is hit while draining a cursor and
    the shell-style rendering of the offending event."""
    ts = event.get("clusterTime")
    ns = event.get("ns") or {}
    summary = (
        '{{operationType: "{op}", ns: {{db: "{db}", coll: "{coll}"}}, '
        "clusterTime: Timestamp({secs}, {inc})}}"
    ).format(
        op=event.get("operationType", ""),
        db=ns.get("db", ""),
        coll=ns.get("coll", ""),
        secs=getattr(ts, "time", 0),
        inc=getattr(ts, "inc", 0),
    )
    if pre:
        what = (
            "a pre-image for all update, delete and replace events, but the pre-image was not found"
        )
    else:
        what = "a post-image for all update events, but the post-image was not found"
    return (
        "Executor error during getMore :: caused by :: "
        f"Change stream was configured to require {what} for event: {summary}"
    )


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
            _set_full_document(event, dict(oplog_entry["o"]))
        return
    if event.get("operationType") == "replace":
        # Replacement-style updates emit the full new doc as `o` (mirroring
        # mongod). The change-stream contract says replace events always
        # carry fullDocument; no separate updateLookup is required.
        _set_full_document(event, dict(oplog_entry["o"]))
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
                raise ChangeStreamRequiredImageError(_required_image_message(event, pre=False))
            _set_full_document(event, None)
            return
        looked_up = _do_lookup(storage, db, coll, doc_id) if doc_id is not None else None
        if looked_up is not None:
            _set_full_document(event, looked_up)
        elif full_document_mode == FULL_DOC_REQUIRED:
            raise ChangeStreamRequiredImageError(_required_image_message(event, pre=False))
        else:
            _set_full_document(event, None)


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
        raise ChangeStreamRequiredImageError(_required_image_message(event, pre=True))
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
    """Project an oplog entry into a change event, in mongod's field order."""
    event, invalidates = _project(
        seq,
        oplog_entry,
        storage=storage,
        full_document_mode=full_document_mode,
        full_document_before_change_mode=full_document_before_change_mode,
        scope=scope,
        show_expanded_events=show_expanded_events,
    )
    return (_order_event_fields(event) if event is not None else None), invalidates


def _project(
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
        }
        # mongod puts wallTime immediately after clusterTime and before ns
        # (measured against 6.0.16, 2026-08-29; we emitted it after
        # documentKey). Field order is invisible to a dict comparison, so it
        # survived every equality-based check until the probe compared key
        # lists — see tools/probes/change_streams.py.
        if wall is not None:
            event["wallTime"] = wall
        event["ns"] = _ns_doc(ns)
        event["documentKey"] = document_key
        # mongod 6.0+ attaches the collection's UUID to CRUD events when the
        # stream was opened with ``showExpandedEvents``. The oplog row carries
        # it as ``ui`` (Binary subtype 4).
        if show_expanded_events and oplog_entry.get("ui") is not None:
            event["collectionUUID"] = oplog_entry["ui"]
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


_KNOWN_SPEC_FIELDS = frozenset(
    {
        "fullDocument",
        "fullDocumentBeforeChange",
        "resumeAfter",
        "startAfter",
        "startAtOperationTime",
        "allChangesForCluster",
        "showExpandedEvents",
        "showRawUpdateDescription",
        "splitLargeChangeStreamEvents",
        "allowToRunOnSystemNS",
    }
)
_FULL_DOCUMENT_MODES = frozenset({"default", "updateLookup", "whenAvailable", "required"})
_FULL_DOCUMENT_BEFORE_MODES = frozenset({"off", "whenAvailable", "required"})


def _spec_error(message: str, code: int, code_name: str):
    """An ``AggregateError`` carrying mongod's code for a bad $changeStream spec.

    Imported lazily: ``aggregate`` imports this module, so a module-level
    import the other way round would be circular.
    """
    from secantus.aggregate import AggregateError

    return AggregateError(message, code=code, code_name=code_name)


def validate_spec(spec: Mapping[str, Any]) -> None:
    """Reject a ``$changeStream`` spec mongod would reject.

    Every check here covers something previously accepted and IGNORED, which is
    the worst shape for a change stream: the caller believes they asked for
    ``updateLookup``, or to resume from a token, and gets a stream that quietly
    does neither. ``parse_spec``'s ``isinstance`` guards silently skipped a
    wrong-typed value and carried on with the default.

    Note this lives in ``parse_spec``'s module rather than in the ``$changeStream``
    pipeline STAGE, because the ``aggregate`` command routes change streams
    through its own path and never runs that stage.
    """
    unknown = next((k for k in spec if k not in _KNOWN_SPEC_FIELDS), None)
    if unknown is not None:
        raise _spec_error(
            f"BSON field '$changeStream.{unknown}' is an unknown field.",
            40415,
            "Location40415",
        )
    for field, allowed in (
        ("fullDocument", _FULL_DOCUMENT_MODES),
        ("fullDocumentBeforeChange", _FULL_DOCUMENT_BEFORE_MODES),
    ):
        value = spec.get(field)
        if value is not None and value not in allowed:
            raise _spec_error(
                f"Enumeration value '{value}' for field '$changeStream.{field}' "
                "is not a valid value.",
                2,
                "BadValue",
            )
    for field in ("resumeAfter", "startAfter"):
        value = spec.get(field)
        if value is not None and not isinstance(value, Mapping):
            raise _spec_error(
                f"BSON field '$changeStream.{field}' is the wrong type "
                f"'{_bson_type_name(value)}', expected type 'object'",
                14,
                "TypeMismatch",
            )
    sat = spec.get("startAtOperationTime")
    if sat is not None and not isinstance(sat, Timestamp):
        raise _spec_error(
            f"BSON field '$changeStream.startAtOperationTime' is the wrong type "
            f"'{_bson_type_name(sat)}', expected type 'timestamp'",
            14,
            "TypeMismatch",
        )


def _bson_type_name(v: Any) -> str:
    from bson import Decimal128, Int64

    if isinstance(v, bool):
        return "bool"
    if isinstance(v, Int64):
        return "long"
    if isinstance(v, int):
        return "int"
    if isinstance(v, float):
        return "double"
    if isinstance(v, Decimal128):
        return "decimal"
    if isinstance(v, str):
        return "string"
    if v is None:
        return "null"
    if isinstance(v, Timestamp):
        return "timestamp"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, (list, tuple)):
        return "array"
    return type(v).__name__


def parse_spec(spec: Mapping[str, Any]) -> ChangeStreamSpec:
    if not isinstance(spec, Mapping):
        raise _spec_error(
            f"$changeStream must take a nested object but found: $changeStream: {spec}",
            6188500,
            "Location6188500",
        )
    validate_spec(spec)
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
    "ChangeStreamRequiredImageError",
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
