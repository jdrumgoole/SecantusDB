"""WiredTiger-backed document store.

WiredTiger is the default storage engine for MongoDB. We use the same
engine here so that on-disk semantics line up with what test code would
see against a real ``mongod``.

Indexes use a sidecar entries table (``table:secantus_index_entries``)
with a single trailing ``u`` column packing
``escape(sortkey) + b"\\x00\\x00" + id_key``. The sortkey comes from
``secantus.sortkey`` (typed, byte-sortable BSON encoding), so the WT
B-tree gives us ordered access for free. ``find_matching`` routes a wide
range of filter shapes through the index — equality, ``$eq``, ``$in``,
``$gt``/``$gte``/``$lt``/``$lte`` on a single field, plus compound
indexes when filter fields cover a leading prefix (with optional range
on the next field). Sort-by-indexed-field walks the B-tree in order.
"""

from __future__ import annotations

import contextlib
import datetime as _dt
import functools
import os
import re
import shutil
import tempfile
import threading
import time as _time
import uuid as _uuid
from collections.abc import Callable, Iterable, Mapping
from typing import Any

import bson
import wiredtiger as wt
from bson.int64 import Int64
from bson.timestamp import Timestamp

from secantus.diff import compute_update_description
from secantus.geo import GeoError, parse_doc_geometry, parse_query_geometry, validate_coordinates
from secantus.geo_index import (
    encode_cell,
    planar_2d_covering_ranges,
    planar_2d_index_for_point,
    s2_doc_covering,
    s2_query_covering,
)
from secantus.paths import get_path, has_path
from secantus.projection import apply_projection
from secantus.query import matches
from secantus.sortkey import COMPOUND_SEP, encode_value, encode_value_directed
from secantus.update import apply_update, find_positional_matches

_GEO_2DSPHERE = "2dsphere"
_GEO_2D = "2d"
_GEO_TYPES = frozenset({_GEO_2DSPHERE, _GEO_2D})


def _geo_type_of(key_spec: Mapping[str, Any]) -> tuple[str, str] | None:
    """Return ``(field, geo_type)`` if ``key_spec`` declares a geo index.

    A geo index has exactly one field whose value is the string
    ``"2dsphere"`` or ``"2d"`` (rather than ``1`` / ``-1``). Compound
    geo indexes (geo field + scalar trailing fields) are out of scope
    in Phase 2; we treat any spec containing a geo field as geo-only
    and ignore the trailing fields. The picker still works because
    `$geoWithin` etc. are answered by the cell scan + verifier.
    """
    for field, value in key_spec.items():
        if isinstance(value, str) and value in _GEO_TYPES:
            return field, value
    return None


def _doc_geo_cells(
    doc: Mapping[str, Any],
    field: str,
    geo_type: str,
    options: Mapping[str, Any],
    *,
    index_name: str = "",
) -> list[bytes]:
    """Encoded cell bytes for the doc's geo field.

    Returns an empty list when the indexed field is missing or null
    (sparse-by-default semantics, matching mongod's 2dsphere/2d).

    Raises :class:`GeoExtractError` when the value is *present* but
    can't be indexed — unparseable shape, wrong type for a 2d index,
    or coordinates outside the valid range. The caller propagates this
    to the wire as a write error (code 16572 "Can't extract geo keys").
    """
    value = get_path(dict(doc), field)
    if value is None:
        # Field missing or explicitly null — sparse semantics, no entry.
        return []
    geom = parse_doc_geometry(value)
    if geom is None:
        raise GeoExtractError(
            index_name,
            field,
            doc.get("_id"),
            f"value {value!r} is not a recognised geometry",
        )
    try:
        validate_coordinates(geom, geo_type=geo_type, options=options)
    except GeoError as exc:
        raise GeoExtractError(index_name, field, doc.get("_id"), str(exc)) from exc
    if geo_type == _GEO_2DSPHERE:
        return [encode_cell(c) for c in s2_doc_covering(geom)]
    # 2d: single point only.
    from shapely.geometry import Point as _Point

    if not isinstance(geom, _Point):
        raise GeoExtractError(
            index_name,
            field,
            doc.get("_id"),
            "2d index requires a point; got a non-point geometry",
        )
    return [encode_cell(planar_2d_index_for_point(geom.x, geom.y, options))]


_COLL_TABLE = "table:secantus_collections"
_DOC_TABLE = "table:secantus_documents"
# Natural-order (insertion-order) index. mongod returns documents from an
# unsorted ``find`` in insertion order (its RecordId store order); our doc
# table is keyed by ``id_key`` (``_id`` sort order), which only coincides with
# insertion order for monotonic ``_id``s. ``_NAT_TABLE`` maps a monotonic
# insertion ``seq`` -> ``id_key`` so an unsorted scan yields insertion order;
# ``_NAT_SEQ_TABLE`` is the reverse (``id_key`` -> ``seq``) so a delete can find
# and drop the doc's ordering entry. The doc table itself is unchanged.
_NAT_TABLE = "table:secantus_natural"
_NAT_SEQ_TABLE = "table:secantus_natural_seq"
_INT64_MIN = -(2**63)  # lowest WT ``q`` key — scan a (db, coll) prefix from the start
_IDX_TABLE = "table:secantus_indexes"
_IDX_ENTRIES_TABLE = "table:secantus_index_entries"
_OPLOG_TABLE = "table:secantus_oplog"
_PREIMAGE_TABLE = "table:secantus_preimages"
_OPLOG_META_TABLE = "table:secantus_oplog_meta"
_USERS_TABLE = "table:secantus_users"
_ROLES_TABLE = "table:secantus_roles"
_PROFILE_TABLE = "table:secantus_profile_settings"

_OPLOG_PRUNE_INTERVAL = 1000  # call prune_oplog every N emits

_ENTRY_SEP = b"\x00\x00"


def _escape_kb(kb: bytes) -> bytes:
    """Order-preserving escape so ``\\x00\\x00`` is unambiguous as a separator."""
    return kb.replace(b"\x00", b"\x00\xff")


def _pack_entry(kb: bytes, id_key: bytes) -> bytes:
    """Pack a sortable index-entry payload into a single ``u`` column.

    WiredTiger length-prefixes ``u`` columns when they're not last in the
    key, which breaks lexicographic comparison. Packing both fields into
    one trailing ``u`` column lets the B-tree do the sort for us.
    """
    return _escape_kb(kb) + _ENTRY_SEP + id_key


def _unpack_entry(packed: bytes) -> tuple[bytes, bytes]:
    """Return ``(escaped_kb, id_key)`` from a packed entry."""
    sep = packed.find(_ENTRY_SEP)
    return packed[:sep], packed[sep + 2 :]


def extract_backup_archive(
    archive_path: str,
    target_dir: str,
    *,
    allow_existing: bool = False,
) -> dict[str, int | str]:
    """Extract a SecantusDB backup archive into ``target_dir``.

    Side-channel restore: the archive is unpacked into a fresh
    directory that the caller then points a new ``SecantusDBServer`` at
    (``SecantusDBServer(storage_path=<target_dir>)``). The function
    does **not** touch any running server's storage — that mode of
    "hot restore over a live WT connection" can't be done safely
    without restructuring how connection threads cache WT sessions,
    and isn't what real mongod's restore tooling supports either.

    Returns ``{"targetDir": <abs>, "fileCount": <int>, "archive": <abs>}``
    on success. Raises ``RuntimeError`` if:

    * the archive doesn't exist,
    * the archive doesn't contain a ``WiredTiger`` metadata file
      (so it's not a SecantusDB / WT backup at all),
    * ``target_dir`` already exists, is non-empty, and ``allow_existing``
      is False (default).

    The WT metadata check runs **before** extraction so a malformed
    archive can't pollute ``target_dir``.
    """
    import tarfile

    abs_archive = os.path.abspath(archive_path)
    abs_target = os.path.abspath(target_dir)
    if not os.path.isfile(abs_archive):
        raise RuntimeError(f"extract_backup_archive: archive not found: {abs_archive}")
    if os.path.exists(abs_target):
        if not os.path.isdir(abs_target):
            raise RuntimeError(
                f"extract_backup_archive: target exists and is not a directory: {abs_target}"
            )
        if os.listdir(abs_target) and not allow_existing:
            raise RuntimeError(
                "extract_backup_archive: target directory is not empty "
                f"(pass allow_existing=True to overlay): {abs_target}"
            )
    else:
        os.makedirs(abs_target)

    with tarfile.open(abs_archive, "r:*") as tar:
        names = tar.getnames()
        if "WiredTiger" not in names:
            raise RuntimeError(
                f"extract_backup_archive: archive {abs_archive!r} is not "
                "a SecantusDB backup (no WiredTiger metadata file inside)"
            )
        tar.extractall(abs_target, filter="data")

    return {
        "targetDir": abs_target,
        "fileCount": len(names),
        "archive": abs_archive,
    }


class DuplicateKeyError(Exception):
    def __init__(self, doc_id: Any) -> None:
        super().__init__(f"duplicate _id: {doc_id!r}")
        self.doc_id = doc_id


def _is_operator_expr(v: Any) -> bool:
    """True when ``v`` is a query OPERATOR expression (a non-empty dict
    whose keys all start with ``$``, e.g. ``{$gt: 5}``) — as opposed to
    a literal subdocument equality value (``{f: 1, f2: 2}``). Used by the
    upsert seed extraction to tell the two apart."""
    return isinstance(v, dict) and len(v) > 0 and all(k.startswith("$") for k in v)


def _id_key(doc_id: Any) -> bytes:
    """Byte-sortable canonical bytes for an ``_id`` value.

    Uses the same byte-sortable encoding the secondary-index entries
    table relies on. Two consequences worth knowing:

    * Cross-numeric collision: ``1 == 1.0 == Decimal128("1")`` produce
      identical bytes (so they hit the same doc / clash on uniqueness),
      because ``encode_value`` normalises numerics through ``Decimal``.
    * Natural iteration: walking the doc table in WT-key order yields
      docs in BSON cross-type sort order, which matches what real
      MongoDB calls "natural order" for non-capped collections.
    """
    return encode_value(doc_id)


def _is_regex_value(v: Any) -> bool:
    return isinstance(v, (re.Pattern, bson.Regex))


def _id_point_lookup_keys(spec: Any) -> list[bytes] | None:
    """id_keys for an ``{_id: <spec>}`` equality predicate, or ``None``.

    The documents table is keyed by ``(db, coll, encode_value(_id))``, so
    an ``_id`` equality is a direct primary-key point lookup rather than a
    COLLSCAN. This returns the WT key bytes to fetch for:

    * a scalar bare equality (``{_id: 5}``),
    * ``{_id: {$eq: scalar}}``,
    * ``{_id: {$in: [scalars]}}``.

    Returns ``None`` (caller falls back to its normal routing / COLLSCAN)
    for range operators, regex, subdocument or operator-valued equalities,
    or anything else that isn't a pure point lookup. ``$in`` keys come back
    deduplicated and in ascending byte (== ``_id``) order so the caller's
    sort-acceleration can treat the result as already sorted on ``_id``.
    An empty ``$in`` yields ``[]`` — a valid no-match point lookup.
    """
    if isinstance(spec, Mapping):
        keys = list(spec.keys())
        if not keys or not all(isinstance(k, str) and k.startswith("$") for k in keys):
            # Literal subdocument _id — leave to the normal path.
            return None
        if keys == ["$eq"]:
            v = spec["$eq"]
            if isinstance(v, Mapping) or _is_regex_value(v):
                return None
            return [_id_key(v)]
        if keys == ["$in"]:
            vals = spec["$in"]
            if not isinstance(vals, (list, tuple)):
                return None
            if any(isinstance(v, Mapping) or _is_regex_value(v) for v in vals):
                return None
            return sorted({_id_key(v) for v in vals})
        return None
    if _is_regex_value(spec):
        return None
    return [_id_key(spec)]


def _parse_index_collation(spec: Any) -> Any:
    """Parse an index's stored ``collation`` option into a Collation.

    Returns ``None`` for falsy / non-dict input, or for collations
    that don't support index encoding (``numericOrdering``) — the
    picker treats those as "index isn't usable for collation
    lookups," falling back to COLLSCAN, while the write path writes
    raw-codepoint entries unchanged.

    Local import avoids the ``storage → collation → sortkey →
    storage`` cycle that a top-level import would create.
    """
    if not isinstance(spec, dict) or not spec:
        return None
    from secantus.collation import parse as _parse_coll

    coll = _parse_coll(spec)
    if coll is None or not coll.supports_index_encoding:
        return None
    return coll


def _doc_makes_multikey(doc: Mapping[str, Any], key_spec: Mapping[str, Any]) -> bool:
    """True if any field in ``key_spec`` resolves to a list value in ``doc``.

    Such a value is encoded as a single composite array sortkey, so a
    later scalar-equality query against this index would silently miss
    the doc — the index must fall back to a full scan.
    """
    return any(isinstance(get_path(dict(doc), field), list) for field in key_spec)


def _index_key(
    doc: Mapping[str, Any],
    key_spec: Mapping[str, Any],
    *,
    sparse: bool,
    collation: Any = None,
) -> bytes | None:
    """Direction-aware byte-sortable encoding for an index ``key_spec``.

    Each field is encoded with ``encode_value_directed`` so ``-1``
    (descending) fields get bitwise-inverted bytes, making a forward
    B-tree walk yield values in descending order. Compound keys are
    joined with ``\\x00\\x00`` between components.

    ``collation`` propagates to every string field — when set, string
    values are normalised (accent-stripped / case-folded per the
    collation strength) before encoding so the entries table sorts
    by the collation's rules rather than raw codepoint. Must match
    the index's stored ``collation`` option; the writers handle
    that.

    For docs whose indexed field is array-valued, this returns the
    whole-array sortkey only — the single canonical "doc-shape" key
    used by uniqueness probes. The full set of multikey entries
    (per-element + whole-array) is produced by
    :func:`_index_key_variants`.
    """
    if sparse:
        for field in key_spec:
            if not has_path(dict(doc), field):
                return None
    fields = list(key_spec)
    if len(fields) == 1:
        d = int(key_spec[fields[0]])
        return encode_value_directed(get_path(dict(doc), fields[0]), d, collation=collation)
    parts = [
        encode_value_directed(get_path(dict(doc), f), int(key_spec[f]), collation=collation)
        for f in fields
    ]
    return COMPOUND_SEP.join(parts)


def _index_key_variants(
    doc: Mapping[str, Any],
    key_spec: Mapping[str, Any],
    *,
    sparse: bool,
    collation: Any = None,
) -> list[bytes]:
    """All byte-keys this doc contributes to an index under ``key_spec``.

    For scalar-valued fields, returns one key — same as ``_index_key``.
    For array-valued fields, returns one key per array element *and*
    the whole-array key, mirroring real ``mongod``'s multikey index
    layout. This makes:

    * ``{tags: "python"}`` against ``{tags: ["python", "go"]}`` light
      up via the per-element entry for ``"python"``.
    * ``{tags: ["python", "go"]}`` (whole-array equality) light up via
      the whole-array entry — without this, the equality lookup would
      false-negative.
    * Range / ``$in`` queries on array fields hit at least all true
      matches (the post-index ``matches()`` filter discards
      false-positives).

    For compound indexes whose multiple fields are array-valued, the
    cartesian product is taken across each field's candidate values.
    Real mongod restricts compound indexes to one multikey field per
    doc; we don't enforce that — we just emit the cross-product, which
    is correct (over-includes; the post-filter discards) but pays a
    cardinality blow-up the user is then on the hook for.

    Returns an empty list when ``sparse`` and any field is missing.
    Per-element values are deduplicated against their encoded bytes,
    so ``[1, 1, 2]`` writes two element entries (``1`` and ``2``) plus
    the whole-array entry, not three.
    """
    fields = list(key_spec)
    if sparse:
        for field in fields:
            if not has_path(dict(doc), field):
                return []

    # Per-field candidate values: scalars contribute [val]; arrays
    # contribute [unique_elements..., whole_array].
    per_field: list[list[Any]] = []
    for field in fields:
        v = get_path(dict(doc), field)
        if isinstance(v, list):
            seen: set[bytes] = set()
            uniq: list[Any] = []
            d = int(key_spec[field])
            for elem in v:
                eb = encode_value_directed(elem, d, collation=collation)
                if eb in seen:
                    continue
                seen.add(eb)
                uniq.append(elem)
            # Whole-array sortkey may collide with an element when the
            # array is a single scalar repeated; the dedup below at the
            # entry level (set of bytes) catches that.
            per_field.append([*uniq, v])
        else:
            per_field.append([v])

    if len(fields) == 1:
        d = int(key_spec[fields[0]])
        keys: list[bytes] = []
        seen_kb: set[bytes] = set()
        for val in per_field[0]:
            kb = encode_value_directed(val, d, collation=collation)
            if kb in seen_kb:
                continue
            seen_kb.add(kb)
            keys.append(kb)
        return keys

    # Compound: cartesian product across per-field candidate lists.
    from itertools import product

    keys = []
    seen_kb = set()
    for combo in product(*per_field):
        parts = [
            encode_value_directed(combo[i], int(key_spec[fields[i]]), collation=collation)
            for i in range(len(fields))
        ]
        kb = COMPOUND_SEP.join(parts)
        if kb in seen_kb:
            continue
        seen_kb.add(kb)
        keys.append(kb)
    return keys


# The pure BSON sort comparator lives in ``secantus.ordering`` (no I/O, so it's
# importable without the WiredTiger extension). Re-exported here for the many
# existing ``from secantus.storage import sort_docs / _SortKey / _bson_lt`` call
# sites and ``find_matching``'s internal ``sort_docs`` calls below.
from secantus.ordering import (  # noqa: E402, F401  (re-exported for back-compat)
    _bson_lt,
    _bson_type_rank,
    _SortKey,
    _to_decimal,
    sort_docs,
)

_ID_INDEX_NAME = "_id_"


def _shell_value(v: Any) -> str:
    """Format a scalar BSON value the way the mongo shell prints it.

    Used inside the ``dup key: { … }`` fragment of an E11000 message so the
    text matches ``mongod`` (drivers like the PHP extension pin the message
    verbatim). Only the value kinds that show up as index keys need exact
    handling; anything else falls back to ``repr``-free ``str``.
    """
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return f'"{v}"'
    if isinstance(v, bson.ObjectId):
        return f"ObjectId('{v}')"
    if v is None:
        return "null"
    return str(v)


def format_dup_key_errmsg(namespace: str, index_name: str, key_value: dict[str, Any] | None) -> str:
    """Build mongod's E11000 duplicate-key ``errmsg`` string.

    ``E11000 duplicate key error collection: <ns> index: <name> dup key: { <k>: <v>, … }``
    — the exact shape ``mongod`` returns and drivers (e.g. the PHP extension's
    ``WriteError::getMessage()``) assert against.
    """
    if key_value:
        inner = ", ".join(f"{k}: {_shell_value(v)}" for k, v in key_value.items())
        dup = "{ " + inner + " }"
    else:
        dup = "{ }"
    return f"E11000 duplicate key error collection: {namespace} index: {index_name} dup key: {dup}"


class IndexConflict(Exception):
    def __init__(
        self,
        index_name: str,
        doc_id: Any,
        *,
        key_pattern: dict[str, Any] | None = None,
        key_value: dict[str, Any] | None = None,
        namespace: str | None = None,
    ) -> None:
        # Build mongod's exact E11000 text when we know the namespace; fall
        # back to ``_id``-derived key for legacy raise sites that pass only a
        # doc id, and to a namespace-less form when even that's unavailable.
        kv = key_value
        if kv is None:
            kv = {"_id": doc_id} if doc_id is not None else None
        super().__init__(format_dup_key_errmsg(namespace or "", index_name, kv))
        self.index_name = index_name
        self.doc_id = doc_id
        self.namespace = namespace
        # Real mongod returns ``keyPattern`` (the index spec) and
        # ``keyValue`` (the conflicting field values) in the dup-key
        # error response. Drivers expose them as ``errorResponse``
        # fields; mongo-java-driver's ``findOneAndUpdate-errorResponse``
        # asserts both. Optional because legacy raise-sites
        # (``_id`` collision before index machinery, recovery paths)
        # don't have the index spec handy.
        self.key_pattern = key_pattern
        self.key_value = key_value


class WriteConflictError(Exception):
    """A WiredTiger WT_ROLLBACK: two transactions touched the same item.

    Inside a user (multi-document) transaction this surfaces to the
    client as mongod's statement-time ``WriteConflict`` (code 112) with
    the ``TransientTransactionError`` label, and the transaction is
    aborted server-side. Outside a transaction the storage layer
    retries the write briefly (a user transaction holds its uncommitted
    writes until commit/abort) before giving up with the same error.
    """


def _is_wt_rollback(exc: BaseException) -> bool:
    """True when a ``WiredTigerError`` is the WT_ROLLBACK conflict signal
    (as opposed to e.g. WT_DUPLICATE_KEY). The SWIG binding raises a
    typed ``WiredTigerRollbackError`` subclass; the message match is a
    fallback for raise-sites that re-wrap into the base class."""
    if isinstance(exc, wt.WiredTigerRollbackError):
        return True
    msg = str(exc)
    return "WT_ROLLBACK" in msg or "conflict between concurrent operations" in msg


# Non-transactional writers that hit a user transaction's uncommitted
# write retry briefly instead of blocking: mongod blocks such writers
# until the transaction commits or aborts, which we approximate with a
# backoff loop bounded by this deadline (the transaction lifetime cap
# is 60s, but a multi-second stall already covers the overwhelmingly
# common test patterns; see tasks/backlog.md for the divergence note).
# mongod's per-document BSON cap (16 MiB). Duplicated from wire.py on
# purpose: storage must not import the wire layer, and both values pin
# the same protocol constant.
MAX_BSON_OBJECT_SIZE = 16 * 1024 * 1024


class DocumentTooLargeError(Exception):
    """A write produced a document over ``MAX_BSON_OBJECT_SIZE``.

    Carries mongod's per-path error code: 10334 (BSONObjectTooLarge)
    for inserts and update-grown documents, 17420 for upserts. The
    message is mongod's verbatim wording — drivers' tests assert it.
    """

    def __init__(self, code: int, errmsg: str) -> None:
        super().__init__(errmsg)
        self.code = code


_WRITE_CONFLICT_RETRY_DEADLINE_S = 5.0
_WRITE_CONFLICT_RETRY_DELAY_S = 0.005
_WRITE_CONFLICT_RETRY_DELAY_MAX_S = 0.02


def _retry_write_conflicts(fn: Callable[..., Any]) -> Callable[..., Any]:
    """Retry a whole public write method on WT_ROLLBACK.

    Safe because the failed attempt's ``_batch_transaction`` already
    rolled everything back and the per-collection lock is released on
    the way out — the retry re-runs from scratch. Inside a user
    transaction the conflict is NOT retried: it surfaces immediately so
    the command layer can abort the transaction with mongod's
    statement-time ``WriteConflict``.
    """

    @functools.wraps(fn)
    def wrapper(self: Storage, *args: Any, **kwargs: Any) -> Any:
        deadline: float | None = None
        delay = _WRITE_CONFLICT_RETRY_DELAY_S
        while True:
            try:
                return fn(self, *args, **kwargs)
            except (WriteConflictError, wt.WiredTigerError) as exc:
                if not isinstance(exc, WriteConflictError) and not _is_wt_rollback(exc):
                    raise
                if getattr(self._tls, "user_txn", None) is not None:
                    raise
                now = _time.monotonic()
                if deadline is None:
                    deadline = now + _WRITE_CONFLICT_RETRY_DEADLINE_S
                if now >= deadline:
                    if isinstance(exc, WriteConflictError):
                        raise
                    raise WriteConflictError(str(exc)) from exc
                _time.sleep(delay)
                delay = min(delay * 2, _WRITE_CONFLICT_RETRY_DELAY_MAX_S)

    return wrapper


class UserTransactionHandle:
    """Storage-side state of one multi-document transaction.

    Knows nothing about ``lsid`` / ``txnNumber`` — that's the
    ``secantus.transactions`` registry's layer. Carries the dedicated
    WT session, its cursor cache (same ``(table, overwrite)`` keying as
    the per-thread cache), and the buffered oplog entries + pre-images
    that ``commit_user_transaction`` flushes.
    """

    __slots__ = ("session", "cursors", "began", "closed", "oplog_entries", "pre_images")

    def __init__(self, session: Any) -> None:
        self.session = session
        self.cursors: dict[tuple[str, bool], Any] = {}
        self.began = False
        self.closed = False
        self.oplog_entries: list[dict[str, Any]] = []
        self.pre_images: list[bytes | None] = []


class DocumentValidationError(Exception):
    """A write produced a doc that didn't satisfy the collection's
    ``validator``. Caught at the command layer and surfaced as the
    mongod-shaped writeError (code 121, ``DocumentValidationFailure``)
    with the ``errInfo.failingDocumentId`` field drivers' errorResponse
    tests assert on."""

    def __init__(self, doc_id: Any) -> None:
        super().__init__("Document failed validation")
        self.doc_id = doc_id


class CreateIndexUnsupported(Exception):
    """``create_index`` was given an index type SecantusDB doesn't support
    (currently ``text`` / ``hashed``). Caught at the command layer and
    surfaced as a typed wire error rather than letting the cell-encoder
    later trip over an opaque internal exception."""


class IndexOptionsConflict(Exception):
    """``create_index`` was called with a name that already exists in the
    collection but with conflicting options (different ``unique`` /
    ``sparse`` / ``hidden`` / ``expireAfterSeconds`` /
    ``partialFilterExpression``). Real mongod rejects with
    ``IndexOptionsConflict`` (code 85); drivers (mongo-ruby-driver's
    ``Collection#create_indexes`` specs) assert on the rejection."""


class GeoExtractError(Exception):
    """Doc's geo field can't be indexed — bad shape or out-of-bounds coords.

    Raised from the geo-index write path when an insert / update / index
    creation hits a doc the geo extractor can't make sense of (bad
    GeoJSON, non-numeric coordinates, longitude / latitude outside the
    valid range, etc.). Caught at the command-layer write boundary and
    surfaced as a wire-level write error with mongod's documented code
    16572 ("Can't extract geo keys").
    """

    def __init__(self, index_name: str, field: str, doc_id: Any, reason: str) -> None:
        super().__init__(
            f"Can't extract geo keys for index {index_name!r} on field {field!r}: {reason}"
        )
        self.index_name = index_name
        self.field = field
        self.doc_id = doc_id
        self.reason = reason


class BadHint(Exception):
    """The ``hint`` passed to ``find_matching`` doesn't name an existing index."""


class MinMaxKeyError(Exception):
    """Cursor ``min`` / ``max`` bounds don't match the hinted index key
    pattern (mongod surfaces this as 51174)."""


def _op_implies_bound(qop: str, qv: Any, pop: str, pv: Any) -> bool:
    """Does a single query constraint ``(qop, qv)`` guarantee the partial
    bound ``(pop, pv)``? Comparison uses ``encode_value`` so it follows
    MongoDB's cross-type BSON sort order. Returns ``False`` for any
    operator pairing it can't prove (soundness over completeness)."""
    try:
        a, b = encode_value(qv), encode_value(pv)
    except Exception:
        return False
    le, lt, ge, gt, eq = a <= b, a < b, a >= b, a > b, a == b
    if pop in ("$lte", "$lt"):
        # query upper-bounds the field; need its max <= / < pv.
        if qop == "$eq":
            return le if pop == "$lte" else lt
        if qop == "$lte":
            return le if pop == "$lte" else lt
        if qop == "$lt":
            return le  # a < qv <= pv  => a < pv => a <= pv (and a < pv for $lt)
        return False
    if pop in ("$gte", "$gt"):
        if qop == "$eq":
            return ge if pop == "$gte" else gt
        if qop == "$gte":
            return ge if pop == "$gte" else gt
        if qop == "$gt":
            return ge
        return False
    if pop == "$eq":
        return qop == "$eq" and eq
    return False


def _clause_implies_bounds(qval: Any, pbound: Mapping[str, Any]) -> bool:
    """True if the query clause ``qval`` (a bare value or an operator
    dict) guarantees every constraint in the partial operator dict
    ``pbound`` (e.g. ``{$lte: 1.5}``)."""
    if isinstance(qval, Mapping) and qval and all(k.startswith("$") for k in qval):
        q_constraints = list(qval.items())
    else:
        q_constraints = [("$eq", qval)]
    for pop, pv in pbound.items():
        if pop not in ("$eq", "$lt", "$lte", "$gt", "$gte"):
            return False  # partial filter uses an operator we can't reason about
        if not any(_op_implies_bound(qop, qv, pop, pv) for qop, qv in q_constraints):
            return False
    return True


class Storage:
    def __init__(
        self,
        path: str = ":memory:",
        *,
        oplog_retention_seconds: float = 3600.0,
        oplog_max_entries: int = 100_000,
        time_func: Callable[[], float] | None = None,
        enable_oplog: bool = True,
        ttl_sweep_seconds: float = 60.0,
        noop_heartbeat_seconds: float = 0.0,
        cache_size: str = "1G",
        session_max: int = 1000,
        sync_on_commit: bool = False,
    ) -> None:
        # When False, _emit_oplog short-circuits and writes nothing —
        # used in standalone (non-replica-set) mode to skip the per-write
        # BSON encode + WT cursor write cost of oplog entries that no
        # change-stream client will ever read. The oplog WT tables are
        # still created so toggling at runtime stays safe.
        self.enable_oplog = enable_oplog
        self._lock = threading.RLock()
        self._closed = False
        # Per-insert discriminator counter for timeseries doc keys (see
        # ``_timeseries_doc_suffix``). Only disambiguates inserts that land
        # in the same nanosecond; wall-clock restart-safety comes from the
        # ``time_ns`` prefix.
        self._ts_suffix_counter = 0
        self._tempdir: str | None = None
        # session_max default is ~120; each client connection thread
        # caches its own session in `threading.local()`, and cross-
        # thread oplog readers open additional short-lived sessions on
        # demand. With a few dozen concurrent client connections plus
        # active change-stream tailers, the default ceiling is hit
        # mid-handshake and surfaces as `out of sessions` /
        # WT_ERROR. mongod itself runs with session_max=33000 — 1000
        # is a generous floor for a single-node test surrogate while
        # still well under the WT hard limit.
        # cache_size default is 100 MB. With ``in_memory=true`` every
        # write also lives in cache, so a workload that inserts a
        # handful of 16 MB documents (mongod's per-doc max) blows the
        # cap as ``WT_CACHE_FULL: operation would overflow cache``.
        # 1 GB gives generous headroom for tests + reasonable
        # in-process workloads while staying well under the limits
        # ``mongod`` itself runs with on a normal box.
        # Tracked so ``checkpoint()`` calls are skipped in in-memory
        # mode (WT's in_memory backend rejects them with a noisy
        # ``__wt_inmem_unsupported_op`` log line on every call).
        self._in_memory = path == ":memory:"
        # Stashed for reuse in restore-archive / explain output.
        self.cache_size = cache_size
        self.session_max = session_max
        self.sync_on_commit = sync_on_commit
        if path == ":memory:":
            self._tempdir = tempfile.mkdtemp(prefix="secantus_wt_")
            home = self._tempdir
            # in_memory=true disables the journal entirely (no files);
            # ephemeral by definition, so durability isn't a concern.
            config = f"create,in_memory=true,session_max={session_max},cache_size={cache_size}"
        else:
            os.makedirs(path, exist_ok=True)
            home = path
            # ``log=(enabled=true)`` turns on WT's redo journal: every
            # transaction commit writes a log record before it returns,
            # and recovery replays the log on reopen. Without this,
            # WT's only durability mechanism is checkpoints (default
            # cadence: every 60s, or on clean ``WT_CONNECTION->close``).
            # On SIGKILL between checkpoints, every uncommitted write
            # is lost — which is exactly the failure mode observed by
            # ``bench/chaos.py`` (3-min chaos run, 17 SIGKILLs:
            # 432,881 acked / 1 persisted).
            #
            # ``transaction_sync`` is the per-commit durability knob.
            # Default ``enabled=false,method=fsync`` matches mongod's
            # default ``writeConcern: {w:1, j:false}`` — log records
            # land in the OS page cache, the OS flushes them on its
            # own schedule, SIGKILL is durable, true power-loss
            # between commits can lose data.
            #
            # ``sync_on_commit=True`` (config-file knob) bumps to
            # ``enabled=true,method=fsync``: every commit fsyncs the
            # log before returning, so the wire-protocol equivalent of
            # ``writeConcern: {j: true}`` is effectively enforced for
            # the whole connection. Throughput cost on small-doc
            # inserts is significant (1-2 orders of magnitude),
            # which is why it's opt-in.
            #
            # ``file_max=10MB`` bounds journal segment size; smaller
            # files churn the log more, larger files delay reclamation.
            # 10 MB matches mongod's WT default.
            sync_part = (
                "transaction_sync=(enabled=true,method=fsync)"
                if sync_on_commit
                else "transaction_sync=(enabled=false,method=fsync)"
            )
            config = (
                f"create,session_max={session_max},cache_size={cache_size},"
                f"log=(enabled=true,file_max=10MB),"
                f"{sync_part}"
            )
        # The on-disk WT home is stashed so ``create_archive`` can tar
        # it after a checkpoint without re-deriving the path.
        self.home_path = home
        self._conn = wt.wiredtiger_open(home, config)
        self._tls = threading.local()
        self._all_sessions: list[Any] = []
        boot = self._conn.open_session()
        try:
            boot.create(_COLL_TABLE, "key_format=SS,value_format=u")
            boot.create(_DOC_TABLE, "key_format=SSu,value_format=u")
            boot.create(_NAT_TABLE, "key_format=SSq,value_format=u")
            boot.create(_NAT_SEQ_TABLE, "key_format=SSu,value_format=q")
            boot.create(_IDX_TABLE, "key_format=SSS,value_format=u")
            boot.create(_IDX_ENTRIES_TABLE, "key_format=SSSu,value_format=u")
            boot.create(_OPLOG_TABLE, "key_format=q,value_format=u")
            boot.create(_PREIMAGE_TABLE, "key_format=q,value_format=u")
            boot.create(_OPLOG_META_TABLE, "key_format=S,value_format=u")
            boot.create(_USERS_TABLE, "key_format=SS,value_format=u")
            boot.create(_ROLES_TABLE, "key_format=SS,value_format=u")
            boot.create(_PROFILE_TABLE, "key_format=S,value_format=u")
        finally:
            boot.close()

        # Oplog state — durable across restart via _OPLOG_META_TABLE.
        self.oplog_retention_seconds = float(oplog_retention_seconds)
        self.oplog_max_entries = int(oplog_max_entries)
        self._time = time_func or _time.time
        self._oplog_cv = threading.Condition(threading.Lock())
        # Set by ``signal_shutdown()`` at server stop so tailable getMore
        # waiters stop blocking and their connection threads drain *before*
        # ``close()`` tears down the WT connection — a thread mid-WT-op when
        # the connection closes is a use-after-free / native crash.
        self._shutting_down = False
        self._oplog_emit_count = 0
        # Tiny fine-grained lock for seq + timestamp minting. Held in
        # microseconds while reserving the next seq range and bumping
        # the cluster-time counter. Carved out of ``_lock`` (Phase 2.1
        # of the WT concurrency plan) so concurrent writers can mint
        # without contending on the global storage lock.
        self._oplog_seq_lock = threading.Lock()
        # Tiny lock for the monotonic insertion-order counter (_NAT_TABLE seq).
        # Global (not per-collection): seqs are unique across the whole store
        # so an unsorted scan within any one collection still sees a strictly
        # increasing insertion order.
        self._nat_seq_lock = threading.Lock()
        # Per-collection RLocks for the CRUD path (Phase 2.4 of the WT
        # concurrency plan). Writes to *different* collections can now
        # run in parallel; writes to the *same* collection still
        # serialise (preserves unique-index correctness + the pre-check
        # racing windows that would otherwise need an architectural
        # refactor of the index-entries schema). DDL operations also
        # acquire the per-coll lock(s) they affect so they cannot
        # reshape schema mid-CRUD-write.
        self._coll_locks: dict[tuple[str, str], threading.RLock] = {}
        self._coll_locks_mutex = threading.Lock()
        with self._lock:
            (
                self._next_seq,
                self._last_ts_secs,
                self._last_ts_ord,
                self._next_nat_seq,
            ) = self._load_oplog_meta()

        # TTL sweeper. Real mongod runs ``ttlMonitor`` every 60s by
        # default; we mirror that. ``ttl_sweep_seconds <= 0`` disables
        # the thread entirely (tests that drive expiry deterministically
        # via ``prune_ttl(now=...)`` use that escape hatch). The
        # sweeper walks every (db, coll) and calls ``prune_ttl`` on
        # each — collections with no TTL index short-circuit cheaply
        # at the index-scan step, so the steady-state cost is small.
        self._ttl_sweep_seconds = float(ttl_sweep_seconds)
        self._ttl_stop = threading.Event()
        self._ttl_thread: threading.Thread | None = None
        if self._ttl_sweep_seconds > 0:
            self._ttl_thread = threading.Thread(
                target=self._ttl_sweep_loop, name="secantus-ttl-sweeper", daemon=True
            )
            self._ttl_thread.start()

        # Periodic noop heartbeat. Real mongod writes ``{op: "n"}``
        # entries to the oplog every ~10s (configurable via
        # ``periodicNoopIntervalSecs``) so cluster time advances and
        # change-stream resume tokens minted from the oplog don't fall
        # outside the retention window during quiet stretches. Default
        # disabled (0) — embedded test users typically don't need it
        # and the extra writes would noise up tight oplog assertions.
        # Set ``noop_heartbeat_seconds=10`` (mongod default) for
        # production-ish behaviour. ``enable_oplog=False`` short-
        # circuits anyway, so the heartbeat is a no-op in that mode.
        self._noop_heartbeat_seconds = float(noop_heartbeat_seconds)
        self._noop_stop = threading.Event()
        self._noop_thread: threading.Thread | None = None
        if self._noop_heartbeat_seconds > 0 and self.enable_oplog:
            self._noop_thread = threading.Thread(
                target=self._noop_heartbeat_loop, name="secantus-noop-heartbeat", daemon=True
            )
            self._noop_thread.start()

    def _load_oplog_meta(self) -> tuple[int, int, int, int]:
        c = self._cursor(_OPLOG_META_TABLE)
        c.set_key("state")
        if c.search() == 0:
            blob = bytes(c.get_value())
            if blob:
                state = bson.decode(blob)
                nat = state.get("next_nat_seq")
                if nat is None:
                    nat = self._scan_max_nat_seq() + 1
                return (
                    int(state.get("next_seq", 1)),
                    int(state.get("last_ts_secs", 0)),
                    int(state.get("last_ts_ord", 0)),
                    int(nat),
                )
        # Fallback: scan oplog table for max key + reconstruct from entry.
        c2 = self._cursor(_OPLOG_TABLE)
        # Walk to last row.
        last_seq = 0
        last_secs = 0
        last_ord = 0
        rc = c2.next()
        while rc == 0:
            seq = int(c2.get_key())
            if seq > last_seq:
                last_seq = seq
                blob = bytes(c2.get_value())
                if blob:
                    entry = bson.decode(blob)
                    ts = entry.get("ts")
                    if isinstance(ts, Timestamp):
                        last_secs, last_ord = ts.time, ts.inc
            rc = c2.next()
        return last_seq + 1, last_secs, last_ord, self._scan_max_nat_seq() + 1

    def _scan_max_nat_seq(self) -> int:
        """Largest insertion ``seq`` present in ``_NAT_TABLE`` (0 if empty).

        Used to recover the insertion counter on reopen when the persisted
        ``next_nat_seq`` is absent (legacy DBs / runs with the oplog disabled),
        so minted seqs stay strictly greater than any existing one.
        """
        c = self._cursor(_NAT_TABLE)
        rc = c.prev()  # last row = highest (db, coll, seq)
        max_seq = 0
        while rc == 0:
            seq = int(c.get_key()[2])
            if seq > max_seq:
                max_seq = seq
            rc = c.prev()
        return max_seq

    def _mint_nat_seq(self) -> int:
        with self._nat_seq_lock:
            seq = self._next_nat_seq
            self._next_nat_seq += 1
            return seq

    def _write_nat_entry(self, db: str, coll: str, id_key: bytes) -> None:
        """Record a doc's insertion position: seq -> id_key (+ reverse)."""
        seq = self._mint_nat_seq()
        nat = self._cursor(_NAT_TABLE)
        nat.reset()
        nat[db, coll, seq] = id_key
        rev = self._cursor(_NAT_SEQ_TABLE)
        rev.reset()
        rev[db, coll, id_key] = seq

    def _delete_nat_entry(self, db: str, coll: str, id_key: bytes) -> None:
        """Drop a doc's insertion-order entry (both directions). No-op if absent
        (legacy docs inserted before this index existed)."""
        rev = self._cursor(_NAT_SEQ_TABLE)
        rev.reset()
        rev.set_key(db, coll, id_key)
        if rev.search() != 0:
            return
        seq = int(rev.get_value())
        rev.remove()
        nat = self._cursor(_NAT_TABLE)
        nat.reset()
        nat.set_key(db, coll, seq)
        if nat.search() == 0:
            nat.remove()

    def _scan_docs_natural(self, db: str, coll: str) -> Iterable[tuple[bytes, bytes]]:
        """Yield ``(id_key, blob)`` in **insertion order** via ``_NAT_TABLE``.

        Walks the natural-order index (seq ascending) and fetches each doc by
        ``id_key`` from the doc table. Falls back to the plain ``id_key``-order
        ``_scan_docs`` when the collection has no natural-order entries (legacy
        data written before this index existed), so order is best-effort there
        rather than empty.
        """
        nat = self._cursor(_NAT_TABLE)
        nat.set_key(db, coll, _INT64_MIN)
        rc = nat.search_near()
        if rc == wt.WT_NOTFOUND or (rc < 0 and nat.next() != 0):
            yield from self._scan_docs(db, coll)
            return
        doc = self._cursor(_DOC_TABLE)
        saw_any = False
        while True:
            k = nat.get_key()
            if k[0] != db or k[1] != coll:
                break
            id_key = bytes(nat.get_value())
            doc.reset()
            doc.set_key(db, coll, id_key)
            if doc.search() == 0:
                saw_any = True
                yield id_key, bytes(doc.get_value())
            if nat.next() != 0:
                break
        if not saw_any:
            # Collection has docs but no nat entries (legacy) — fall back.
            yield from self._scan_docs(db, coll)

    def _persist_oplog_meta(self) -> None:
        c = self._cursor(_OPLOG_META_TABLE)
        c["state"] = bson.encode(
            {
                "next_seq": self._next_seq,
                "last_ts_secs": self._last_ts_secs,
                "last_ts_ord": self._last_ts_ord,
                "next_nat_seq": self._next_nat_seq,
            }
        )

    def _mint_ts(self) -> Timestamp:
        """Return a strictly-monotonic ``Timestamp(secs, ord)``.

        Caller must hold ``self._oplog_seq_lock``. Within a single
        wall-clock second ``ord`` increments; on a new second it resets
        to 1. Recovered state on startup ensures the first mint after
        restart is strictly greater than any previously-emitted
        timestamp.
        """
        now = int(self._time())
        if now > self._last_ts_secs:
            self._last_ts_secs = now
            self._last_ts_ord = 1
        else:
            self._last_ts_ord += 1
        return Timestamp(self._last_ts_secs, self._last_ts_ord)

    def _coll_lock(self, db: str, coll: str) -> threading.RLock:
        """Return the per-collection RLock for ``(db, coll)``, creating it
        on first reference. Phase 2.4 of the WT concurrency plan.

        CRUD on a given collection serialises through this lock; CRUD on
        *other* collections proceeds in parallel. DDL on this collection
        also acquires this lock so schema changes cannot interleave with
        in-flight writes.
        """
        key = (db, coll)
        # Fast path: lock already exists — read without any mutation,
        # safe under GIL.
        existing = self._coll_locks.get(key)
        if existing is not None:
            return existing
        # Create-or-fetch under the small registry mutex. RLocks are
        # never removed (collections come and go but the lock identity
        # for a given (db, coll) stays stable across drop+recreate to
        # avoid races with in-flight writers).
        with self._coll_locks_mutex:
            existing = self._coll_locks.get(key)
            if existing is not None:
                return existing
            lock = threading.RLock()
            self._coll_locks[key] = lock
            return lock

    def _mint_oplog_seq_and_ts(self, n: int) -> tuple[int, list[Timestamp]]:
        """Atomically reserve ``n`` consecutive oplog seq numbers and mint
        ``n`` strictly-monotonic timestamps. Returns ``(start_seq,
        [ts_0, ..., ts_{n-1}])``.

        Held only under ``_oplog_seq_lock`` (microseconds of work) — the
        actual oplog cursor writes happen in the caller's WT session
        without blocking other writers on this lock.
        """
        with self._oplog_seq_lock:
            start = self._next_seq
            self._next_seq += n
            timestamps = [self._mint_ts() for _ in range(n)]
            return start, timestamps

    def _collection_uuid(self, db: str, coll: str) -> _uuid.UUID:
        """Return the collection's UUID, minting and persisting on first call.

        Fast path (UUID already present): no Python lock — straight WT
        cursor read on the calling thread's session. This was a major
        per-insert bottleneck before Phase 2.4: every write re-acquired
        ``self._lock`` here, defeating the per-collection lock split.
        Slow path (mint a new UUID): take ``_coll_lock`` for the
        namespace to serialise the persist; double-check inside the
        lock so two racing callers can't mint different UUIDs for the
        same collection.
        """
        opts = self._coll_options(db, coll) or {}
        existing = opts.get("uuid")
        if isinstance(existing, _uuid.UUID):
            return existing
        if isinstance(existing, bson.Binary) and len(existing) == 16:
            return _uuid.UUID(bytes=bytes(existing))
        if isinstance(existing, bytes) and len(existing) == 16:
            return _uuid.UUID(bytes=existing)
        # Mint path — take the per-coll lock; re-read after acquiring
        # so a racer that won the mint race is observed.
        with self._coll_lock(db, coll):
            opts = self._coll_options(db, coll) or {}
            existing = opts.get("uuid")
            if isinstance(existing, _uuid.UUID):
                return existing
            if isinstance(existing, bson.Binary) and len(existing) == 16:
                return _uuid.UUID(bytes=bytes(existing))
            if isinstance(existing, bytes) and len(existing) == 16:
                return _uuid.UUID(bytes=existing)
            new_uuid = _uuid.uuid4()
            opts["uuid"] = new_uuid
            self._write_coll_options(db, coll, opts)
            return new_uuid

    def collection_uuid(self, db: str, coll: str) -> _uuid.UUID:
        """Public alias for ``_collection_uuid``."""
        return self._collection_uuid(db, coll)

    def current_cluster_time(self) -> Timestamp:
        """Return a strictly-monotonic ``Timestamp`` advancing the cluster clock."""
        with self._oplog_seq_lock:
            ts = self._mint_ts()
        # Meta persist uses the calling thread's WT session/cursor —
        # safe to do outside the seq lock since it doesn't depend on
        # the in-memory counters being held stable past the mint.
        self._persist_oplog_meta()
        return ts

    def peek_cluster_time(self) -> Timestamp:
        """The last minted cluster time WITHOUT advancing the clock.

        Reply gossip (``$clusterTime`` / ``operationTime`` attached to
        every command reply) observes cluster time; only writes and the
        explicit ``current_cluster_time`` advance it — matching mongod,
        where reads gossip the node's known cluster time. A virgin
        store mints once so the gossiped value is never
        ``Timestamp(0, 0)``.
        """
        with self._oplog_seq_lock:
            if self._last_ts_secs:
                return Timestamp(self._last_ts_secs, self._last_ts_ord)
        return self.current_cluster_time()

    def _write_coll_options(self, db: str, coll: str, opts: Mapping[str, Any]) -> None:
        c = self._cursor(_COLL_TABLE)
        # bson can't directly encode a uuid.UUID without a codec, so store as Binary subtype 4.
        encoded: dict[str, Any] = {}
        for k, v in opts.items():
            if isinstance(v, _uuid.UUID):
                encoded[k] = bson.Binary(v.bytes, subtype=4)
            else:
                encoded[k] = v
        c[db, coll] = bson.encode(encoded) if encoded else b""

    def set_collection_options(self, db: str, coll: str, **opts: Any) -> None:
        """Merge ``opts`` into the collection's options blob (creates if absent)."""
        with self._lock:
            self._ensure_collection(db, coll)
            current = self._coll_options(db, coll) or {}
            current.update(opts)
            self._write_coll_options(db, coll, current)

    def get_collection_options(self, db: str, coll: str) -> dict[str, Any]:
        """Return the collection's options blob, or ``{}`` if absent."""
        if self.enable_oplog and db == "local" and coll == "oplog.rs":
            # Synthetic ``local.oplog.rs``: report the capped-collection
            # shape mongod uses so $collStats / listCollections options
            # match. ``size`` is a notional byte cap derived from the
            # entry cap × a conservative per-entry estimate; we don't
            # track real byte usage, only entry count.
            return {
                "capped": True,
                "size": self.oplog_max_entries * 16 * 1024,
                "max": self.oplog_max_entries,
            }
        self._refresh_read_snapshot()
        with self._lock:
            opts = self._coll_options(db, coll) or {}
            # Decode UUID Binary back into uuid.UUID for callers.
            decoded: dict[str, Any] = {}
            for k, v in opts.items():
                if k == "uuid" and isinstance(v, bson.Binary) and len(v) == 16:
                    decoded[k] = _uuid.UUID(bytes=bytes(v))
                else:
                    decoded[k] = v
            return decoded

    def _is_oplog_rs(self, db: str, coll: str) -> bool:
        """``(local, oplog.rs)`` is the synthetic oplog view."""
        return self.enable_oplog and db == "local" and coll == "oplog.rs"

    def _scan_oplog_entries(self) -> list[dict[str, Any]]:
        """Walk every persisted oplog entry and return the decoded docs.

        Uses a private short-lived session so the read view always
        reflects rows committed by writer threads on other connections
        (same pattern as ``read_oplog``).
        """
        rows: list[dict[str, Any]] = []
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_OPLOG_TABLE, None)
                try:
                    rc = c.next()
                    while rc == 0:
                        blob = bytes(c.get_value())
                        if blob:
                            rows.append(bson.decode(blob))
                        rc = c.next()
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()
        return rows

    def _find_oplog_rs(
        self,
        filter: dict[str, Any] | None,
        *,
        skip: int,
        limit: int,
        sort: Mapping[str, Any] | None,
        projection: Mapping[str, Any] | None,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> list[dict[str, Any]]:
        """Read path for the synthetic ``local.oplog.rs`` view.

        Entries are walked in seq order (== ts order). Filter / sort /
        skip / limit / projection are all honoured against the decoded
        entry docs via the existing pure-Python helpers.
        """
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = self._scan_oplog_entries()
        if filter:
            rows = [r for r in rows if matches(r, filter, vars=let, collation=collation_obj)]
        if sort:
            # ``$natural`` is the oplog's only meaningful order: entries are
            # already scanned in natural (seq == insertion == ts) order, so
            # ``$natural: 1`` is the identity and ``$natural: -1`` reverses.
            # It's a pseudo-field, not a document field, so it must not go
            # through the generic field-sort (which would see it as missing).
            natural = sort.get("$natural") if isinstance(sort, Mapping) else None
            if natural is not None:
                if int(natural) < 0:
                    rows = list(reversed(rows))
            else:
                rows = sort_docs(rows, sort)
        if skip:
            rows = rows[skip:]
        if limit > 0:
            rows = rows[:limit]
        if projection:
            rows = [apply_projection(r, projection) for r in rows]
        return rows

    def _is_system_users(self, db: str, coll: str) -> bool:
        """``admin.system.users`` is the synthetic view onto the user
        store. Mongod surfaces user records there regardless of which
        database the user was created against — the per-user ``db``
        field of each record names the authentication database. Other
        databases' ``system.users`` namespace exists but is empty (also
        matches mongod)."""
        return db == "admin" and coll == "system.users"

    def _scan_user_records(self) -> list[dict[str, Any]]:
        """Walk every persisted user record across all databases and
        return the decoded docs. Uses a private short-lived session for
        the same cross-thread visibility reason as
        :meth:`_scan_oplog_entries`."""
        rows: list[dict[str, Any]] = []
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_USERS_TABLE, None)
                try:
                    rc = c.next()
                    while rc == 0:
                        blob = bytes(c.get_value())
                        if blob:
                            rows.append(bson.decode(blob))
                        rc = c.next()
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()
        return rows

    def _find_system_users(
        self,
        filter: dict[str, Any] | None,
        *,
        skip: int,
        limit: int,
        sort: Mapping[str, Any] | None,
        projection: Mapping[str, Any] | None,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> list[dict[str, Any]]:
        """Read path for ``admin.system.users``. The user records
        themselves already carry the mongod-shaped fields (``_id`` =
        ``<db>.<user>``, ``user``, ``db``, ``credentials``, ``roles``,
        ``mechanisms``), so the view is the row set unchanged plus the
        usual filter / sort / skip / limit / projection pipeline."""
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = self._scan_user_records()
        if filter:
            rows = [r for r in rows if matches(r, filter, vars=let, collation=collation_obj)]
        if sort:
            rows = sort_docs(rows, sort)
        if skip:
            rows = rows[skip:]
        if limit > 0:
            rows = rows[:limit]
        if projection:
            rows = [apply_projection(r, projection) for r in rows]
        return rows

    def _count_system_users(
        self,
        filter: dict[str, Any] | None,
        *,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> int:
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = self._scan_user_records()
        if not filter:
            return len(rows)
        return sum(1 for r in rows if matches(r, filter, vars=let, collation=collation_obj))

    def _is_system_version(self, db: str, coll: str) -> bool:
        """``admin.system.version`` is the synthetic view that surfaces
        the user-management auth-schema doc. Mongod stores other
        cluster-state docs here too (e.g. the version-2-to-3 schema
        upgrade snapshot from MongoDB 2.6 → 3.0), but in modern
        deployments the only doc that tooling cares about is
        ``{_id: "authSchema", currentVersion: 5}`` — the version SCRAM
        introduced. Surfacing just that doc is what driver tools
        actually check on startup before issuing user-management
        commands."""
        return db == "admin" and coll == "system.version"

    def _system_version_docs(self) -> list[dict[str, Any]]:
        """The fixed contents of ``admin.system.version``.

        Mongod's ``authSchema`` currentVersion is ``5`` as of MongoDB
        4.0 — the SCRAM-SHA-256 baseline. We advertise the same number
        so tools that gate user-management on the schema version
        proceed (we implement SCRAM-SHA-256 natively, so 5 is honest).
        """
        return [{"_id": "authSchema", "currentVersion": 5}]

    def _find_system_version(
        self,
        filter: dict[str, Any] | None,
        *,
        skip: int,
        limit: int,
        sort: Mapping[str, Any] | None,
        projection: Mapping[str, Any] | None,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> list[dict[str, Any]]:
        """Read path for ``admin.system.version`` — synthetic fixed-doc view."""
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = self._system_version_docs()
        if filter:
            rows = [r for r in rows if matches(r, filter, vars=let, collation=collation_obj)]
        if sort:
            rows = sort_docs(rows, sort)
        if skip:
            rows = rows[skip:]
        if limit > 0:
            rows = rows[:limit]
        if projection:
            rows = [apply_projection(r, projection) for r in rows]
        return rows

    def _count_system_version(
        self,
        filter: dict[str, Any] | None,
        *,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> int:
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = self._system_version_docs()
        if not filter:
            return len(rows)
        return sum(1 for r in rows if matches(r, filter, vars=let, collation=collation_obj))

    def _count_oplog_rs(
        self,
        filter: dict[str, Any] | None,
        *,
        let: dict[str, Any] | None,
        collation: Any,
    ) -> int:
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        if not filter:
            return len(self._scan_oplog_entries())
        return sum(
            1
            for r in self._scan_oplog_entries()
            if matches(r, filter, vars=let, collation=collation_obj)
        )

    def _emit_oplog(
        self,
        entries: list[dict[str, Any]],
        pre_images: list[bytes | None] | None = None,
    ) -> int:
        """Append ``entries`` to the oplog table under ``self._lock``.

        ``pre_images`` is parallel to ``entries``; non-None elements are
        stored under the matching seq in ``_PREIMAGE_TABLE``. Returns the
        highest seq emitted (0 if ``entries`` is empty). Notifies waiters
        on ``self._oplog_cv`` once writes have committed.

        If ``self.enable_oplog`` is False, returns 0 immediately — the
        caller's prebuilt ``entries`` list is discarded. The change-stream
        condvar is still notified so any tailable getMore wakes up and
        observes the (empty) state.

        When a user (multi-document) transaction is installed on this
        thread, entries are **buffered** on the transaction handle and
        nothing is written or notified: seqs must be minted at commit
        time, because a statement-time seq could become visible *behind*
        a concurrent change-stream reader's position and the event would
        be silently skipped. ``commit_user_transaction`` flushes the
        buffer through this same method (with the buffering hook
        disarmed) inside the transaction's WT session.
        """
        handle = getattr(self._tls, "user_txn", None)
        if handle is not None:
            if self.enable_oplog and entries:
                if pre_images is None:
                    pre_images = [None] * len(entries)
                handle.oplog_entries.extend(entries)
                handle.pre_images.extend(pre_images)
            return 0
        if not self.enable_oplog:
            with self._oplog_cv:
                self._oplog_cv.notify_all()
            return 0
        if not entries:
            return 0
        if pre_images is None:
            pre_images = [None] * len(entries)
        assert len(pre_images) == len(entries)
        # Reserve seq + ts range up-front under the tiny seq lock.
        # The actual cursor writes below run on this thread's WT
        # session without holding any cross-thread Python lock.
        n = len(entries)
        start_seq, ts_range = self._mint_oplog_seq_and_ts(n)
        op_cur = self._cursor(_OPLOG_TABLE)
        pre_cur = None
        last_seq = 0
        for i, (entry, pre) in enumerate(zip(entries, pre_images, strict=True)):
            seq = start_seq + i
            entry_with_ts = dict(entry)
            if "ts" not in entry_with_ts:
                entry_with_ts["ts"] = ts_range[i]
            if "wall" not in entry_with_ts:
                entry_with_ts["wall"] = _dt.datetime.now(_dt.timezone.utc)
            op_cur[seq] = bson.encode(entry_with_ts)
            if pre is not None:
                if pre_cur is None:
                    pre_cur = self._cursor(_PREIMAGE_TABLE)
                pre_cur[seq] = pre
            last_seq = seq
        # ``_persist_oplog_meta`` was called here on every emit, but
        # under concurrent writers it WT-rollbacks half the time —
        # every writer hits the same single ``"state"`` meta row.
        # The meta row is purely a recovery optimisation; if it's
        # stale, ``_load_oplog_meta``'s fallback scans the oplog
        # table for the actual max seq. So we now persist only on
        # close + on prune_oplog, both of which are rare. The seq
        # mint itself is durable because the actual oplog rows are
        # written on every emit.
        self._oplog_emit_count += len(entries)
        if self._oplog_emit_count >= _OPLOG_PRUNE_INTERVAL:
            self._oplog_emit_count = 0
            self._prune_oplog_locked(now=self._time())
        with self._oplog_cv:
            self._oplog_cv.notify_all()
        return last_seq

    def read_oplog(
        self,
        *,
        start_seq: int,
        limit: int,
        ns_filter: Callable[[str], bool] | None = None,
    ) -> list[tuple[int, dict[str, Any]]]:
        """Forward-scan the oplog from ``start_seq`` (inclusive).

        Uses a private short-lived session so the read view always reflects
        rows committed by other sessions. The cached per-thread session's
        snapshot is sticky — under WiredTiger's MVCC, reusing it across
        getMore polls would never observe oplog rows produced by a writer
        running on a different connection thread.
        """
        out: list[tuple[int, dict[str, Any]]] = []
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_OPLOG_TABLE, None)
                try:
                    c.set_key(int(start_seq))
                    rc = c.search_near()
                    if rc == wt.WT_NOTFOUND:
                        return out
                    if rc < 0 and c.next() != 0:
                        return out
                    while True:
                        seq = int(c.get_key())
                        blob = bytes(c.get_value())
                        if blob:
                            entry = bson.decode(blob)
                            if ns_filter is None or ns_filter(str(entry.get("ns", ""))):
                                out.append((seq, entry))
                        if len(out) >= limit:
                            break
                        if c.next() != 0:
                            break
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()
        return out

    def read_preimage(self, seq: int) -> dict[str, Any] | None:
        """Return the pre-image doc for ``seq`` if one was stored, else ``None``.

        Uses a private session for cross-thread visibility (see ``read_oplog``).
        """
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_PREIMAGE_TABLE, None)
                try:
                    c.set_key(int(seq))
                    if c.search() != 0:
                        return None
                    blob = bytes(c.get_value())
                    if not blob:
                        return None
                    return bson.decode(blob)
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def oplog_tail_seq(self) -> int:
        """Highest seq currently present (or last emitted). 0 if empty."""
        with self._lock:
            return self._next_seq - 1

    def oplog_tail_seq_nolock(self) -> int:
        """Highest seq read without acquiring ``self._lock``.

        Safe for use **only** as the wake predicate for a tailable
        ``getMore`` waiting on ``self._oplog_cv``: lock order in the
        write path is ``_lock`` -> ``_oplog_cv``, so a waiter that
        already holds ``_oplog_cv`` (which is what ``cv.wait_for``
        does) MUST NOT then take ``_lock`` -- that's an ABBA deadlock
        with any concurrent writer. Reading ``_next_seq`` directly is
        safe because (a) ``int`` reads are atomic under the GIL and
        (b) the cv is also notified on every commit, so any momentary
        stale read self-corrects on the next iteration of the
        ``wait_for`` predicate.
        """
        return self._next_seq - 1

    def oplog_floor_seq(self) -> int:
        """Smallest seq currently present after pruning. 0 if empty.

        Uses a private session for cross-thread visibility.
        """
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_OPLOG_TABLE, None)
                try:
                    rc = c.next()
                    if rc != 0:
                        return 0
                    return int(c.get_key())
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def find_seq_for_ts(self, ts: Timestamp) -> int:
        """Smallest seq whose entry ``ts >= target``. Tail+1 if none qualify.

        Uses a private session for cross-thread visibility.
        """
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_OPLOG_TABLE, None)
                try:
                    rc = c.next()
                    while rc == 0:
                        seq = int(c.get_key())
                        blob = bytes(c.get_value())
                        if blob:
                            entry = bson.decode(blob)
                            entry_ts = entry.get("ts")
                            if isinstance(entry_ts, Timestamp) and (
                                entry_ts.time > ts.time
                                or (entry_ts.time == ts.time and entry_ts.inc >= ts.inc)
                            ):
                                return seq
                        rc = c.next()
                    return self._next_seq
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def prune_oplog(self, *, now: float | None = None) -> int:
        """Drop oplog rows older than retention or above the entry cap."""
        with self._lock:
            return self._prune_oplog_locked(now=now)

    def _ns(self, db: str, coll: str) -> str:
        return f"{db}.{coll}"

    def _pre_post_images_enabled(self, db: str, coll: str) -> bool:
        opts = self._coll_options(db, coll) or {}
        cfg = opts.get("changeStreamPreAndPostImages")
        return isinstance(cfg, Mapping) and bool(cfg.get("enabled"))

    def _prune_oplog_locked(self, *, now: float | None = None) -> int:
        when = now if now is not None else self._time()
        cutoff_secs = int(when - self.oplog_retention_seconds)
        # Two-phase: collect doomed seqs, then delete (avoid mutating during scan).
        doomed: list[int] = []
        all_seqs: list[int] = []
        c = self._cursor(_OPLOG_TABLE)
        rc = c.next()
        while rc == 0:
            seq = int(c.get_key())
            blob = bytes(c.get_value())
            all_seqs.append(seq)
            if blob:
                entry = bson.decode(blob)
                ts = entry.get("ts")
                if isinstance(ts, Timestamp) and ts.time < cutoff_secs:
                    doomed.append(seq)
            rc = c.next()
        # Trim to entry cap by extending doom set to oldest entries.
        kept_count = len(all_seqs) - len(doomed)
        if kept_count > self.oplog_max_entries:
            extra = kept_count - self.oplog_max_entries
            doomed_set = set(doomed)
            for seq in all_seqs:
                if extra <= 0:
                    break
                if seq not in doomed_set:
                    doomed.append(seq)
                    doomed_set.add(seq)
                    extra -= 1
        if not doomed:
            return 0
        op_del = self._cursor(_OPLOG_TABLE)
        pre_del = self._cursor(_PREIMAGE_TABLE)
        for seq in doomed:
            op_del.set_key(seq)
            with contextlib.suppress(wt.WiredTigerError):
                op_del.remove()
            op_del.reset()
            pre_del.set_key(seq)
            with contextlib.suppress(wt.WiredTigerError):
                pre_del.remove()
            pre_del.reset()
        return len(doomed)

    # --- Users (auth) ---

    def add_user(
        self,
        db: str,
        username: str,
        record: Mapping[str, Any],
        *,
        replace: bool = False,
    ) -> bool:
        """Persist a user record. Returns True if added; False if it already
        existed and ``replace=False``.

        ``record`` is a BSON-encodable dict of arbitrary shape (the
        commands layer owns the structure). Stored verbatim.
        """
        with self._lock:
            c = self._cursor(_USERS_TABLE)
            c.set_key(db, username)
            if c.search() == 0 and not replace:
                return False
            c.reset()
            c[db, username] = bson.encode(dict(record))
            return True

    def get_user(self, db: str, username: str) -> dict[str, Any] | None:
        with self._lock:
            c = self._cursor(_USERS_TABLE)
            c.set_key(db, username)
            if c.search() != 0:
                return None
            blob = bytes(c.get_value())
            return bson.decode(blob) if blob else None

    def drop_user(self, db: str, username: str) -> bool:
        with self._lock:
            c = self._cursor(_USERS_TABLE)
            c.set_key(db, username)
            if c.search() != 0:
                return False
            c.remove()
            return True

    def list_users(
        self,
        db: str | None = None,
        *,
        skip: int = 0,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """Paginated user listing. ``db=None`` lists across all databases."""
        if limit <= 0 or limit > 1000:
            limit = 1000
        out: list[dict[str, Any]] = []
        with self._lock:
            c = self._cursor(_USERS_TABLE)
            rc = c.next()
            seen = 0
            while rc == 0:
                k = c.get_key()
                row_db = k[0]
                if db is None or row_db == db:
                    if seen >= skip:
                        blob = bytes(c.get_value())
                        if blob:
                            out.append(bson.decode(blob))
                        if len(out) >= limit:
                            break
                    seen += 1
                rc = c.next()
        return out

    # ------------------------------------------------------------------
    # Per-database profiling settings.
    #
    # Real mongod tracks (level, slowms, sampleRate) per database in
    # memory + persists to the database's metadata. We persist in a
    # dedicated WT table keyed by db name. The dispatch path reads
    # these settings on every command — keep ``get_profile`` fast.
    # ------------------------------------------------------------------

    def get_profile(self, db: str) -> dict[str, Any]:
        """Return the active profile settings for ``db``, defaults if unset.

        Defaults match mongod: level 0 (off), slowms 100, sampleRate 1.0.
        """
        with self._lock:
            c = self._cursor(_PROFILE_TABLE)
            c.set_key(db)
            if c.search() != 0:
                return {"level": 0, "slowms": 100, "sampleRate": 1.0}
            blob = bytes(c.get_value())
            if not blob:
                return {"level": 0, "slowms": 100, "sampleRate": 1.0}
            doc = bson.decode(blob)
            # ``or default`` is wrong here — slowms=0 / sampleRate=0.0 are
            # legitimate values that must round-trip, not be replaced
            # with defaults. Use direct ``.get`` with the default and
            # coerce only when a value is actually present.
            level_v = doc.get("level", 0)
            slowms_v = doc.get("slowms", 100)
            rate_v = doc.get("sampleRate", 1.0)
            return {
                "level": int(level_v) if level_v is not None else 0,
                "slowms": int(slowms_v) if slowms_v is not None else 100,
                "sampleRate": float(rate_v) if rate_v is not None else 1.0,
            }

    def set_profile(
        self,
        db: str,
        *,
        level: int,
        slowms: int = 100,
        sample_rate: float = 1.0,
    ) -> None:
        """Persist profile settings for ``db``."""
        if level not in (0, 1, 2):
            raise ValueError("level must be 0, 1, or 2")
        if slowms < 0:
            raise ValueError("slowms must be non-negative")
        if not (0.0 <= sample_rate <= 1.0):
            raise ValueError("sampleRate must be in [0, 1]")
        doc = {"level": int(level), "slowms": int(slowms), "sampleRate": float(sample_rate)}
        with self._lock:
            c = self._cursor(_PROFILE_TABLE)
            c[db] = bson.encode(doc)

    def ensure_profile_collection(self, db: str, *, size_bytes: int = 10 * 1024 * 1024) -> None:
        """Ensure ``<db>.system.profile`` exists as a 10 MB-default capped collection."""
        if self.collection_exists(db, "system.profile"):
            return
        self.create_collection(db, "system.profile")
        self.set_collection_options(db, "system.profile", capped=True, size=int(size_bytes))

    # ------------------------------------------------------------------
    # Custom roles. Storage layer is a thin BSON-blob CRUD; the commands
    # layer owns the role-record shape (privileges + inherited roles)
    # and ``secantus.rbac`` owns the privilege-check logic that walks
    # the inheritance graph.
    # ------------------------------------------------------------------

    def add_role(
        self,
        db: str,
        name: str,
        record: Mapping[str, Any],
        *,
        replace: bool = False,
    ) -> bool:
        """Persist a custom role record. Returns True if added; False if
        it already existed and ``replace=False``."""
        with self._lock:
            c = self._cursor(_ROLES_TABLE)
            c.set_key(db, name)
            if c.search() == 0 and not replace:
                return False
            c.reset()
            c[db, name] = bson.encode(dict(record))
            return True

    def get_role(self, db: str, name: str) -> dict[str, Any] | None:
        # Use a private short-lived session so cross-thread visibility
        # is guaranteed: connection-thread A may have written a role
        # while we're on connection-thread B, and B's cached session
        # carries a sticky snapshot that won't observe A's commit.
        # Same pattern as ``read_oplog``. The cost (one open_session +
        # close per call) is negligible vs the correctness win.
        with self._lock:
            session = self._conn.open_session()
            try:
                c = session.open_cursor(_ROLES_TABLE, None, None)
                try:
                    c.set_key(db, name)
                    if c.search() != 0:
                        return None
                    blob = bytes(c.get_value())
                    return bson.decode(blob) if blob else None
                finally:
                    with contextlib.suppress(Exception):
                        c.close()
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def drop_role(self, db: str, name: str) -> bool:
        with self._lock:
            c = self._cursor(_ROLES_TABLE)
            c.set_key(db, name)
            if c.search() != 0:
                return False
            c.remove()
            return True

    def list_roles(
        self,
        db: str | None = None,
        *,
        skip: int = 0,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """Paginated custom-role listing. ``db=None`` spans every db."""
        if limit <= 0 or limit > 1000:
            limit = 1000
        out: list[dict[str, Any]] = []
        with self._lock:
            c = self._cursor(_ROLES_TABLE)
            rc = c.next()
            seen = 0
            while rc == 0:
                k = c.get_key()
                row_db = k[0]
                if db is None or row_db == db:
                    if seen >= skip:
                        blob = bytes(c.get_value())
                        if blob:
                            out.append(bson.decode(blob))
                        if len(out) >= limit:
                            break
                    seen += 1
                rc = c.next()
        return out

    def signal_shutdown(self) -> None:
        """Tell tailable getMore waiters the server is stopping so they wake
        and return immediately, letting their connection threads drain before
        :meth:`close` tears down WiredTiger. One-way: only set at stop."""
        self._shutting_down = True
        with self._oplog_cv:
            self._oplog_cv.notify_all()

    def close(self) -> None:
        # Stop background threads before tearing down WT — both the
        # TTL sweeper and the noop heartbeat acquire ``self._lock``,
        # so racing them against close would deadlock or
        # use-after-close.
        self._ttl_stop.set()
        if self._ttl_thread is not None and self._ttl_thread.is_alive():
            self._ttl_thread.join(timeout=2.0)
            self._ttl_thread = None
        self._noop_stop.set()
        if self._noop_thread is not None and self._noop_thread.is_alive():
            self._noop_thread.join(timeout=2.0)
            self._noop_thread = None
        with self._lock:
            if self._closed:
                return
            self._closed = True
            # Persist the oplog meta one last time. We dropped the
            # per-emit persist in Phase 2.4 (it caused WT-rollback
            # storms under concurrent writers), so this is the
            # canonical place to write the in-memory ``_next_seq``
            # and timestamp counters down to disk before shutdown.
            with contextlib.suppress(Exception):
                self._persist_oplog_meta()
            # Force a checkpoint before tearing the connection down.
            # ``WT_CONNECTION->close`` does this implicitly, but only
            # when logging is off (or hits the connection's
            # close-time flush window). Driving it explicitly here
            # gives a durable on-disk image of the dataset at the
            # moment of shutdown regardless of journal state — the
            # behaviour callers reasonably expect from ``close()``.
            # Skip for in-memory backends: WT's in_memory engine
            # rejects checkpoint() with a noisy stderr log
            # (``__wt_inmem_unsupported_op``) on every call.
            if not self._in_memory:
                with contextlib.suppress(Exception):
                    self._session().checkpoint()
            for s in self._all_sessions:
                with contextlib.suppress(Exception):
                    s.close()
            self._all_sessions.clear()
            with contextlib.suppress(Exception):
                self._conn.close()
            if self._tempdir is not None:
                # Don't follow symlinks during cleanup. A local attacker
                # racing the mkdtemp could replace `_tempdir` with a
                # symlink to elsewhere on the filesystem before close()
                # fires — `shutil.rmtree(symlink, ignore_errors=True)`
                # would then delete the symlink target. The mkdtemp
                # already creates with mode 0700 (owner-only), but the
                # parent /tmp is world-writable, so this is the
                # belt-and-braces guard. Failures during cleanup are
                # logged but not raised — close() must remain idempotent.
                try:
                    if not os.path.islink(self._tempdir):
                        shutil.rmtree(self._tempdir)
                except OSError:
                    # Best-effort: log via warnings rather than crash close().
                    import warnings as _warn

                    _warn.warn(
                        f"failed to remove WiredTiger tempdir {self._tempdir!r}",
                        ResourceWarning,
                        stacklevel=2,
                    )
                self._tempdir = None

    def prune_ttl_all_collections(self, *, now: _dt.datetime | None = None) -> int:
        """Run :meth:`prune_ttl` against every collection, returning the
        total docs pruned. Used by the background sweeper and exposed
        publicly so callers (admin tooling, tests) can drive a
        deterministic global pass.

        Callers using the cached per-thread session must call
        :meth:`_reset_thread_session` first — WiredTiger snapshots
        are sticky per-session, so reads otherwise miss rows
        committed by other threads. The sweeper does this on every
        iteration; one-shot user calls happen on the writer's thread
        and see their own writes.
        """
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            namespaces: list[tuple[str, str]] = []
            rc = c.next()
            while rc == 0:
                k = c.get_key()
                namespaces.append((k[0], k[1]))
                rc = c.next()
        total = 0
        for db, coll in namespaces:
            with contextlib.suppress(Exception):
                # Storage close races: drop_collection between snapshot
                # and prune fails inside prune_ttl with a missing-coll
                # error. The sweeper should never crash the daemon.
                total += self.prune_ttl(db, coll, now=now)
        return total

    def _ttl_sweep_loop(self) -> None:
        """Background sweeper: every ``ttl_sweep_seconds`` walk all
        collections and prune expired docs. Stops when ``_ttl_stop``
        is set or the storage is closed.

        Drops the per-thread WT session before each iteration so the
        next cursor call opens a fresh session. WiredTiger sessions
        carry a sticky read snapshot — without the reset, reads on
        this thread would never observe rows committed by other
        writers, and TTL sweeps would always return 0 even when
        expired docs existed. Same pattern as ``read_oplog``.
        """
        import logging

        log = logging.getLogger("secantus.storage.ttl")
        while not self._ttl_stop.wait(self._ttl_sweep_seconds):
            if self._closed:
                return
            self._reset_thread_session()
            try:
                self.prune_ttl_all_collections()
            except Exception:
                # Sweeper failures must not propagate — they'd kill
                # the daemon thread and silently disable expiry.
                log.exception("ttl sweep failed")

    def ensure_oplog_bootstrap(self) -> None:
        """Seed a bootstrap noop on a *fresh* oplog so ``local.oplog.rs`` is
        never empty — mirroring mongod, whose first oplog entry is the replica
        set's "initiating set" noop. Without it a brand-new server's oplog has
        zero rows and a client tailing ``local.oplog.rs`` (pymongo's
        ``test_cursor.test_to_list_tailable``) finds nothing to read.

        Called by :class:`SecantusDBServer` at startup (replica-set initiation
        is a server/replication concern, not a storage-engine one — bare
        ``Storage`` instances in unit tests keep a clean empty oplog). A noop
        (``op: "n"``) is skipped by change-stream projection, so it never
        surfaces as a change event. Idempotent: fires only when the oplog is
        enabled and truly fresh (``_next_seq == 1``); reopening a populated
        oplog is a no-op.
        """
        with self._lock:
            if self.enable_oplog and self._next_seq == 1:
                self._emit_oplog([{"op": "n", "ns": "", "o": {"msg": "initiating set"}}])

    def emit_noop_heartbeat(self) -> int:
        """Append one ``{op: "n"}`` heartbeat to the oplog and return its seq.

        The entry shape mirrors mongod's periodic noop: ``op = "n"``,
        an empty namespace, current cluster time, and a small
        ``o = {msg: "periodic noop"}`` payload. Change-stream consumers
        skip ``op: "n"`` rows in projection but still advance their
        ``position_seq`` and ``last_token`` past them, so the resume
        token of a quiet collection stays current.

        Public so callers (admin tooling, tests that drive heartbeats
        deterministically) can fire one explicitly.
        """
        with self._lock:
            return self._emit_oplog(
                [
                    {
                        "op": "n",
                        "ns": "",
                        "o": {"msg": "periodic noop"},
                    }
                ]
            )

    def _noop_heartbeat_loop(self) -> None:
        """Background heartbeat: emit one ``{op: "n"}`` oplog entry every
        ``noop_heartbeat_seconds``. Stops when ``_noop_stop`` is set
        or the storage is closed. Failures are logged and swallowed —
        a transient WT error must not kill the daemon thread.
        """
        import logging

        log = logging.getLogger("secantus.storage.noop")
        while not self._noop_stop.wait(self._noop_heartbeat_seconds):
            if self._closed:
                return
            try:
                self.emit_noop_heartbeat()
            except Exception:
                log.exception("noop heartbeat failed")

    def _reset_thread_session(self) -> None:
        """Close the calling thread's cached WT session + cursors so
        the next ``_session()`` call opens fresh ones. Needed when a
        thread reads in a loop and must observe writes from other
        threads (snapshot is otherwise sticky)."""
        s = getattr(self._tls, "session", None)
        if s is None:
            return
        cursors = getattr(self._tls, "cursors", {}) or {}
        for c in cursors.values():
            with contextlib.suppress(Exception):
                c.close()
        with contextlib.suppress(Exception):
            s.close()
        with self._lock, contextlib.suppress(ValueError):
            self._all_sessions.remove(s)
        self._tls.session = None
        self._tls.cursors = {}

    def checkpoint(self) -> None:
        """Force a WiredTiger checkpoint to flush dirty pages to disk.

        Backs the ``fsync`` command and the admin UI's maintenance
        slice. Lock-protected so concurrent commands wait their turn.
        On in-memory backends the call is a no-op (WT's in_memory
        engine has no disk to flush and rejects with a noisy stderr
        log).
        """
        with self._lock:
            if self._closed or self._in_memory:
                return
            self._session().checkpoint()

    def create_archive(self, output_path: str) -> dict[str, int | str]:
        """Force a checkpoint, then tar the consistent file set into ``output_path``.

        Returns ``{"path": <abs>, "sizeBytes": <int>}`` on success.
        Raises ``RuntimeError`` for in-memory backends — there's no
        on-disk state to archive.

        Uses WT's dedicated ``backup:`` cursor to enumerate the files
        that constitute a consistent snapshot. WT promises during the
        cursor's lifetime that those files won't change and that they
        are read-shareable — the latter matters on Windows, where WT
        otherwise holds exclusive file locks that block ``tarfile``'s
        reads. Walking the directory directly worked on Unix (open
        files are shareable by default) but ``PermissionError``'d on
        Windows.

        Output is a single ``.tar.gz`` (gzip-compressed) so the archive
        round-trips cleanly through git/mail/scp; the typical workload
        compresses well because WT pages aren't snappy/zstd at rest.
        """
        import tarfile

        if self._in_memory:
            raise RuntimeError(
                "create_archive: cannot archive an in-memory backend "
                "(WT in_memory engine has no on-disk state)"
            )
        # Resolve to absolute so the returned ``path`` is unambiguous
        # for the caller even if their cwd has shifted.
        abs_out = os.path.abspath(output_path)
        os.makedirs(os.path.dirname(abs_out) or ".", exist_ok=True)
        with self._lock:
            if self._closed:
                raise RuntimeError("create_archive: storage is closed")
            self._session().checkpoint()
            # A private session for the backup cursor so its lifecycle
            # doesn't interfere with the per-thread cached session
            # that handles regular work.
            backup_session = self._conn.open_session()
            try:
                cursor = backup_session.open_cursor("backup:", None, None)
                try:
                    # Tar inline while the cursor is open: WT creates
                    # the ``WiredTiger.backup`` metadata file as part
                    # of the cursor's open state and removes it on
                    # close, so collecting filenames first then tarring
                    # would race the cleanup. Iterate-and-add keeps
                    # every file readable for the duration of the tar.
                    with tarfile.open(abs_out, "w:gz") as tar:
                        while cursor.next() == 0:
                            rel = cursor.get_key()
                            full = os.path.join(self.home_path, rel)
                            tar.add(full, arcname=rel)
                finally:
                    cursor.close()
            finally:
                backup_session.close()
        return {"path": abs_out, "sizeBytes": os.path.getsize(abs_out)}

    @contextlib.contextmanager
    def _batch_transaction(self, *, sync: bool = False) -> Any:
        """Group multiple cursor writes into one WT transaction = one log record.

        WT auto-commits every individual ``cursor.insert()`` /
        ``cursor.update()`` etc., which means N writes produce N log
        records and N commit overheads. With this wrapper, the same N
        writes share a single commit (and therefore a single log
        record): on a typical bulk insert that's a 2-5x throughput
        win for ``--batch-size > 1`` on the wire side, with the same
        durability guarantee (all-or-nothing on commit).

        ``sync=True`` overrides the connection-level
        ``transaction_sync`` setting and forces this individual
        commit to fsync the log to disk before returning — the
        per-transaction equivalent of the server-wide
        ``sync_on_commit`` knob. Used to honour
        ``writeConcern: {j: true}`` on a single write even when the
        daemon is otherwise running with ``sync_on_commit=false``.
        ``sync=False`` (default) inherits the connection's
        ``transaction_sync`` config.

        Caller must already hold ``self._lock``. Reads within the
        transaction observe the in-progress writes — fine for our
        unique-conflict probes which need to see uncommitted siblings
        in the same batch.

        On exception the transaction is rolled back. Callers that
        accumulate per-doc errors (e.g. ``ordered=False`` insert)
        should NOT raise out of the block — they handle the per-doc
        errors locally and let the surviving writes commit.

        Inside a user (multi-document) transaction this is a no-op
        passthrough: WT doesn't nest transactions, and the statement's
        writes must stay uncommitted in the user transaction until its
        ``commitTransaction``.
        """
        if getattr(self._tls, "user_txn", None) is not None:
            yield self._session()
            return
        session = self._session()
        # Cached cursors must be reset before begin_transaction so they
        # don't carry a stale snapshot from before the transaction
        # boundary. WT documents this requirement explicitly.
        for c in getattr(self._tls, "cursors", {}).values():
            with contextlib.suppress(Exception):
                c.reset()
        session.begin_transaction()
        try:
            yield session
        except Exception:
            with contextlib.suppress(Exception):
                session.rollback_transaction()
            raise
        else:
            session.commit_transaction("sync=on" if sync else None)

    def _session(self) -> Any:
        s = getattr(self._tls, "session", None)
        if s is None:
            s = self._conn.open_session()
            self._tls.session = s
            self._tls.cursors = {}
            with self._lock:
                self._all_sessions.append(s)
        return s

    def _refresh_read_snapshot(self) -> None:
        """Force the per-thread WT session to acquire a fresh read snapshot.

        WiredTiger's default snapshot isolation pins a session's read
        view at first cursor access; subsequent reads on the same
        session see exactly that point-in-time view until the session
        commits / rolls back a transaction. That's correct for a single
        in-flight operation, but our daemon reuses one session per
        connection thread across the full lifetime of a TCP connection.
        Without an explicit snapshot refresh, a long-lived client
        connection (Java's ``ClusterFixture`` is the canonical case)
        does an insert, idles while another connection commits a
        write, then reads — and sees the stale pre-other-write view.

        ``session.reset_snapshot()`` releases the held snapshot so the
        next cursor read picks up the latest committed state. Called at
        the top of every public read entry point (``find_matching``,
        ``count_matching``, ``list_*``, ``explain_plan``) so cross-
        connection visibility matches real ``mongod``.
        """
        if getattr(self._tls, "user_txn", None) is not None:
            # A user transaction's whole point is the pinned snapshot:
            # reads inside it must keep seeing the transaction's view.
            return
        s = getattr(self._tls, "session", None)
        if s is None:
            return
        with contextlib.suppress(Exception):
            # ``reset_snapshot()`` errors if the session is in an
            # explicit transaction. Reads never run inside one
            # (``_batch_transaction`` is write-only), so the exception
            # path is defensive — log via ``suppress`` and move on.
            s.reset_snapshot()

    # -- user (multi-document) transactions --------------------------------
    #
    # A user transaction owns a dedicated WT session, NOT the connection
    # thread's ``threading.local`` one: pymongo can legally send a
    # transaction's statements and its retryable commit on different
    # pooled connections (= different server threads). Statements run
    # with the transaction's session/cursors swapped into ``_tls`` so
    # every existing storage path (unique probes, index writes,
    # ``_ensure_collection``, ``find_matching``) transparently executes
    # inside the WT transaction — read-your-own-writes and the pinned
    # snapshot fall out for free. The command layer serializes access
    # per transaction; these primitives assume no two threads install
    # the same handle concurrently.

    def begin_user_transaction(self) -> UserTransactionHandle:
        """Open a dedicated WT session for a multi-document transaction.

        The WT ``begin_transaction`` itself happens lazily on the first
        ``use_user_transaction`` entry so the snapshot pins at the
        transaction's first statement (mongod semantics).
        """
        with self._lock:
            if self._closed:
                raise RuntimeError("storage is closed")
            session = self._conn.open_session()
            # Registered so ``close()``'s sweep rolls back leftovers.
            self._all_sessions.append(session)
        return UserTransactionHandle(session)

    @contextlib.contextmanager
    def use_user_transaction(self, handle: UserTransactionHandle) -> Any:
        """Run the body with ``handle``'s session installed as this
        thread's storage session, arming the oplog buffering hook."""
        if not handle.began:
            handle.session.begin_transaction()
            handle.began = True
        with self._install_txn_session(handle):
            self._tls.user_txn = handle
            try:
                yield
            finally:
                self._tls.user_txn = None

    def commit_user_transaction(
        self,
        handle: UserTransactionHandle,
        *,
        lsid_doc: Mapping[str, Any] | None = None,
        txn_number: int | None = None,
    ) -> int:
        """Flush the buffered oplog + commit the WT transaction.

        All buffered entries get one shared commit ``Timestamp``
        (mongod stamps every op in a transaction with the commit time)
        plus ``lsid`` / ``txnNumber`` for change-stream events. The
        oplog/preimage rows are written through the transaction's own
        session *before* ``commit_transaction``, so data and oplog
        become visible atomically. Returns the last oplog seq emitted.

        On failure the transaction is rolled back and the exception
        propagates — a failed WT commit cannot be retried into success.
        """
        last_seq = 0
        try:
            if handle.began:
                entries = handle.oplog_entries
                pre_images = handle.pre_images
                if entries and self.enable_oplog:
                    # Mint the shared commit timestamp before installing
                    # the txn session: ``current_cluster_time`` persists
                    # oplog meta through the calling thread's session and
                    # that write must not ride inside the transaction.
                    ts = self.current_cluster_time()
                    wall = _dt.datetime.now(_dt.timezone.utc)
                    for entry in entries:
                        entry.setdefault("ts", ts)
                        entry.setdefault("wall", wall)
                        if lsid_doc is not None:
                            entry["lsid"] = dict(lsid_doc)
                        if txn_number is not None:
                            entry["txnNumber"] = Int64(txn_number)
                with self._install_txn_session(handle):
                    # ``_tls.user_txn`` is deliberately NOT set here, so
                    # ``_emit_oplog`` takes its real write path on the
                    # transaction's session instead of re-buffering.
                    if entries:
                        last_seq = self._emit_oplog(entries, pre_images)
                    handle.session.commit_transaction()
        except Exception:
            self.abort_user_transaction(handle)
            raise
        self._close_user_txn_session(handle)
        # ``_emit_oplog`` notified before the WT commit (same order as
        # the non-transactional write path); one more notify after the
        # commit guarantees tailable getMore waiters re-poll against
        # the now-visible rows.
        with self._oplog_cv:
            self._oplog_cv.notify_all()
        return last_seq

    def abort_user_transaction(self, handle: UserTransactionHandle) -> None:
        """Roll back and release the transaction's WT session. Idempotent."""
        if handle.closed:
            return
        if handle.began:
            with contextlib.suppress(Exception):
                handle.session.rollback_transaction()
        self._close_user_txn_session(handle)

    @contextlib.contextmanager
    def _install_txn_session(self, handle: UserTransactionHandle) -> Any:
        tls = self._tls
        prev_session = getattr(tls, "session", None)
        prev_cursors = getattr(tls, "cursors", {})
        tls.session = handle.session
        tls.cursors = handle.cursors
        try:
            yield
        finally:
            tls.session = prev_session
            tls.cursors = prev_cursors

    def _close_user_txn_session(self, handle: UserTransactionHandle) -> None:
        if handle.closed:
            return
        handle.closed = True
        for c in handle.cursors.values():
            with contextlib.suppress(Exception):
                c.close()
        handle.cursors.clear()
        with contextlib.suppress(Exception):
            handle.session.close()
        with self._lock, contextlib.suppress(ValueError):
            self._all_sessions.remove(handle.session)

    def _cursor(self, table: str, *, overwrite: bool = True) -> Any:
        self._session()
        cursors: dict[tuple[str, bool], Any] = self._tls.cursors
        key = (table, overwrite)
        c = cursors.get(key)
        if c is None:
            cfg = None if overwrite else "overwrite=false"
            c = self._tls.session.open_cursor(table, None, cfg)
            cursors[key] = c
        else:
            c.reset()
        return c

    def _coll_options(self, db: str, coll: str) -> dict[str, Any] | None:
        c = self._cursor(_COLL_TABLE)
        c.set_key(db, coll)
        rc = c.search()
        if rc != 0:
            return None
        blob = bytes(c.get_value())
        return bson.decode(blob) if blob else {}

    def _is_timeseries(self, db: str, coll: str) -> bool:
        opts = self._coll_options(db, coll)
        return bool(opts) and "timeseries" in opts

    def _timeseries_doc_suffix(self) -> bytes:
        """Doc-table key discriminator for timeseries collections.

        Timeseries collections don't enforce ``_id`` uniqueness (mongod
        buckets measurements by time; ``_id`` is not a key), but our doc
        table is keyed by ``encode_value(_id)`` — equal ``_id``s would
        structurally collide. Suffixing the key keeps duplicates adjacent
        (the sortkey encoding is prefix-free, so grouping by ``_id`` is
        preserved) in insertion order. ``time_ns`` keeps suffixes unique
        across store reopens; the counter disambiguates same-nanosecond
        inserts. Reads decode and filter by content, so the suffix is
        invisible above storage — but the ``_id`` point-lookup fast path
        must not be used (it reconstructs the UNsuffixed key).
        """
        self._ts_suffix_counter = (self._ts_suffix_counter + 1) % 0x10000
        return _time.time_ns().to_bytes(8, "big") + self._ts_suffix_counter.to_bytes(2, "big")

    def _ensure_collection(self, db: str, coll: str) -> None:
        c = self._cursor(_COLL_TABLE)
        c.set_key(db, coll)
        if c.search() == 0:
            return
        c.reset()
        c[db, coll] = b""

    def collection_exists(self, db: str, coll: str) -> bool:
        with self._lock:
            return self._coll_options(db, coll) is not None

    def create_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, coll)
            if c.search() == 0:
                return False
            c.reset()
            c[db, coll] = b""
            self._collection_uuid(db, coll)  # mint and persist
            ui = self._collection_uuid(db, coll)
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {
                            "create": coll,
                            "idIndex": {"v": 2, "key": {"_id": 1}, "name": "_id_"},
                        },
                    }
                ]
            )
            return True

    def _scan_docs(self, db: str, coll: str) -> Iterable[tuple[bytes, bytes]]:
        c = self._cursor(_DOC_TABLE)
        c.set_key(db, coll, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll:
                return
            yield bytes(k[2]), bytes(c.get_value())
            if c.next() != 0:
                return

    def _all_docs(self, db: str, coll: str) -> list[dict[str, Any]]:
        # Two-stage to keep ``bson.decode`` out of ``self._lock`` —
        # otherwise an N-doc scan blocks every other thread for the
        # whole decode loop. Lock owns the WT cursor walk; decode
        # happens after release.
        with self._lock:
            blobs = [blob for _id_k, blob in self._scan_docs(db, coll)]
        return [bson.decode(blob) for blob in blobs]

    def _all_docs_with_id_key(self, db: str, coll: str) -> list[tuple[dict[str, Any], bytes]]:
        with self._lock:
            raw = [(id_k, blob) for id_k, blob in self._scan_docs(db, coll)]
        return [(bson.decode(blob), id_k) for id_k, blob in raw]

    def scan_docs_after_id_key(
        self, db: str, coll: str, after: bytes | None
    ) -> list[tuple[bytes, dict[str, Any]]]:
        """Scan the document table in natural (id_key) order, returning
        only rows whose ``id_key`` is strictly greater than ``after``.
        ``after`` of ``None`` returns the entire collection.

        Used by the tailable-cursor producer to emit only the docs
        inserted since the last poll. Returns ``[(id_key, doc), ...]``
        — callers update their ``after`` checkpoint to the last
        returned ``id_key`` for the next poll.
        """
        # Two-stage: collect raw bytes under the lock, decode after.
        with self._lock:
            raw: list[tuple[bytes, bytes]] = [
                (id_k, blob)
                for id_k, blob in self._scan_docs(db, coll)
                if after is None or id_k > after
            ]
        return [(id_k, bson.decode(blob)) for id_k, blob in raw]

    def collection_min_id_key(self, db: str, coll: str) -> bytes | None:
        """Smallest ``id_key`` in the collection — the oldest doc in natural
        (insertion, for monotonic ``_id``) order — or ``None`` if empty.

        Used to detect capped-collection rollover for tailable cursors: if a
        cursor's last-returned ``id_key`` is below this, the document it was
        anchored on has been evicted, and mongod kills the cursor with
        ``CappedPositionLost``. ``_scan_docs`` yields in ``id_key`` order, so
        the first row is the minimum — we stop after it.
        """
        with self._lock:
            for id_k, _blob in self._scan_docs(db, coll):
                return bytes(id_k)
            return None

    def collection_is_capped(self, db: str, coll: str) -> bool:
        """Public predicate: does the collection have ``capped: true`` set?

        The synthetic ``local.oplog.rs`` view is always capped (mongod models
        the oplog as a capped collection) even though it isn't materialised in
        the collections table — so tailable cursors over it are accepted.
        """
        with self._lock:
            if self._is_oplog_rs(db, coll):
                return True
            opts = self._coll_options(db, coll) or {}
            return bool(opts.get("capped"))

    @_retry_write_conflicts
    def insert(
        self,
        db: str,
        coll: str,
        docs: Iterable[dict[str, Any]],
        *,
        ordered: bool = True,
        journal: bool = False,
    ) -> tuple[int, list[dict[str, Any]]]:
        # Materialized so the conflict-retry wrapper can safely re-run
        # the whole method (a generator would arrive exhausted).
        docs = list(docs)
        inserted = 0
        errors: list[dict[str, Any]] = []
        oplog_entries: list[dict[str, Any]] = []
        fresh_id_keys: set[bytes] = set()
        oplog_on = self.enable_oplog
        with self._coll_lock(db, coll), self._batch_transaction(sync=journal):
            # Per-collection lock (Phase 2.4): writes to other
            # collections proceed in parallel; same-collection writes
            # still serialise to keep the unique-index pre-check
            # race-free. _batch_transaction wraps the per-doc cursor
            # inserts (doc table + index entries + oplog) in one
            # explicit WT transaction so they share a single commit /
            # log record.
            self._ensure_collection(db, coll)
            ns = self._ns(db, coll) if oplog_on else ""
            ui = self._collection_uuid(db, coll) if oplog_on else None
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            multikey_names = self._multikey_index_names(db, coll)
            timeseries = self._is_timeseries(db, coll)
            for index, doc in enumerate(docs):
                if "_id" not in doc:
                    doc["_id"] = bson.ObjectId()
                key = _id_key(doc["_id"])
                if timeseries:
                    # Duplicate _ids are legal in timeseries collections —
                    # see _timeseries_doc_suffix.
                    key += self._timeseries_doc_suffix()
                conflict = self._unique_conflict(
                    db, coll, doc, indexes, exclude_id_key=None, partials=partials
                )
                if conflict is not None:
                    cname, kpat, kval = conflict
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": format_dup_key_errmsg(f"{db}.{coll}", cname, kval),
                            "keyPattern": kpat,
                            "keyValue": kval,
                        }
                    )
                    if ordered:
                        break
                    continue
                # Pre-flight every geo index: a bad geometry should reject
                # the doc *before* it lands in the doc table, so we don't
                # leave a half-indexed write behind. Validation is cheap;
                # _write_index_entries below recomputes the same cells.
                try:
                    self._validate_geo_indexes(db, coll, doc, indexes, partials)
                except GeoExtractError as exc:
                    errors.append({"index": index, "code": 16572, "errmsg": str(exc)})
                    if ordered:
                        break
                    continue
                blob = bson.encode(doc)
                if len(blob) > MAX_BSON_OBJECT_SIZE:
                    # mongod rejects per-document at insert time with
                    # BSONObjectTooLarge (10334) and this exact wording.
                    errors.append(
                        {
                            "index": index,
                            "code": 10334,
                            "errmsg": (
                                f"object to insert too large. size in bytes: "
                                f"{len(blob)}, max size: {MAX_BSON_OBJECT_SIZE}"
                            ),
                        }
                    )
                    if ordered:
                        break
                    continue
                doc_cur = self._cursor(_DOC_TABLE, overwrite=False)
                doc_cur.set_key(db, coll, key)
                doc_cur.set_value(blob)
                try:
                    doc_cur.insert()
                except wt.WiredTigerError as exc:
                    if _is_wt_rollback(exc):
                        # Concurrency conflict, not a duplicate key —
                        # surface for transaction/retry handling instead
                        # of lying with an E11000.
                        raise WriteConflictError(str(exc)) from exc
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": format_dup_key_errmsg(
                                f"{db}.{coll}", "_id_", {"_id": doc["_id"]}
                            ),
                            "keyPattern": {"_id": 1},
                            "keyValue": {"_id": doc["_id"]},
                        }
                    )
                    if ordered:
                        break
                    continue
                self._write_index_entries(db, coll, doc, indexes, partials, id_key_override=key)
                self._write_nat_entry(db, coll, key)
                multikey_names = self._maybe_mark_multikey(db, coll, doc, indexes, multikey_names)
                inserted += 1
                if oplog_on:
                    oplog_entries.append(
                        {
                            "op": "i",
                            "ns": ns,
                            "ui": bson.Binary(ui.bytes, subtype=4),
                            "o": dict(doc),
                            "o2": {"_id": doc["_id"]},
                        }
                    )
                fresh_id_keys.add(key)
            cap_entries, cap_pre_images = self._enforce_capped_bounds_locked(
                db, coll, fresh_id_keys, indexes, partials, oplog_on, ns, ui
            )
            if oplog_entries or cap_entries:
                pre_images = [None] * len(oplog_entries) + cap_pre_images
                self._emit_oplog(oplog_entries + cap_entries, pre_images)
        return inserted, errors

    def _enforce_capped_bounds_locked(
        self,
        db: str,
        coll: str,
        fresh_id_keys: set[bytes],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        partials: dict[str, dict[str, Any]],
        oplog_on: bool,
        ns: str,
        ui: _uuid.UUID | None,
    ) -> tuple[list[dict[str, Any]], list[bytes | None]]:
        """Evict oldest non-fresh docs from a capped collection until within bounds.

        "Oldest" is the natural-order walk over the doc table, which matches
        insertion order when ``_id`` is monotonic (e.g. the default
        ObjectId). For non-monotonic ``_id`` values the eviction order
        reflects ``_id`` byte order, not literal insertion order — capped
        users with custom ``_id`` should not rely on FIFO semantics.
        """
        raw = self._coll_options(db, coll) or {}
        if not raw.get("capped"):
            return [], []
        size_limit = raw.get("size")
        max_limit = raw.get("max")
        if size_limit is None and max_limit is None:
            return [], []
        scanned = list(self._scan_docs(db, coll))
        total = sum(len(blob) for _id_k, blob in scanned)
        count = len(scanned)
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
        for id_k, blob in scanned:
            over_size = size_limit is not None and total > size_limit
            over_max = max_limit is not None and count > max_limit
            if not over_size and not over_max:
                break
            if id_k in fresh_id_keys:
                # Don't evict docs we just inserted in this batch — they
                # always sort to the tail with monotonic _ids, so reaching
                # one means everything left is fresh too.
                break
            doc = bson.decode(blob)
            self._delete_index_entries(db, coll, doc, indexes, partials)
            doc_cur = self._cursor(_DOC_TABLE)
            doc_cur.set_key(db, coll, id_k)
            doc_cur.remove()
            self._delete_nat_entry(db, coll, id_k)
            total -= len(blob)
            count -= 1
            if oplog_on:
                entry: dict[str, Any] = {
                    "op": "d",
                    "ns": ns,
                    "o": {"_id": doc["_id"]},
                    "o2": {"_id": doc["_id"]},
                }
                if ui is not None:
                    entry["ui"] = bson.Binary(ui.bytes, subtype=4)
                oplog_entries.append(entry)
                pre_images.append(bson.encode(doc) if preimages_on else None)
        return oplog_entries, pre_images

    def find_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        skip: int = 0,
        limit: int = 0,
        sort: Mapping[str, Any] | None = None,
        projection: Mapping[str, Any] | None = None,
        hint: str | Mapping[str, Any] | None = None,
        let: dict[str, Any] | None = None,
        collation: Any = None,
        min_bound: Mapping[str, Any] | None = None,
        max_bound: Mapping[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        if self._is_oplog_rs(db, coll):
            return self._find_oplog_rs(
                filter,
                skip=skip,
                limit=limit,
                sort=sort,
                projection=projection,
                let=let,
                collation=collation,
            )
        if self._is_system_users(db, coll):
            return self._find_system_users(
                filter,
                skip=skip,
                limit=limit,
                sort=sort,
                projection=projection,
                let=let,
                collation=collation,
            )
        if self._is_system_version(db, coll):
            return self._find_system_version(
                filter,
                skip=skip,
                limit=limit,
                sort=sort,
                projection=projection,
                let=let,
                collation=collation,
            )
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        self._refresh_read_snapshot()
        filter = filter or {}
        in_sort_order = False
        # Two-stage decode discipline: the lock is held only for the
        # WT cursor walk and any index routing; the COLLSCAN fallback
        # collects raw blobs while the lock is held and defers
        # ``bson.decode`` (and the ``matches()`` predicate, sorting,
        # projection) to *after* the lock releases. Concurrent readers
        # then decode in parallel even while a writer holds the lock
        # for inserts. Index-path candidates still come back already
        # decoded — that's a deeper refactor (Phase 2 territory).
        candidates: list[dict[str, Any]] | None = None
        raw_blobs: list[bytes] | None = None
        with self._lock:
            sort_field, sort_dir = self._single_sort_spec(sort)
            if hint is not None:
                resolved = self._resolve_hint(db, coll, hint)
                candidates, in_sort_order = self._candidates_from_hint(
                    db, coll, resolved, sort_field, sort_dir
                )
            else:
                # Per-index collation: ``_try_index_lookup`` gates indexes
                # by exact match against ``collation_obj`` (None counts as
                # "no collation"), so the same code path covers both the
                # plain and the collation-bearing cases. Same applies to
                # the sort-acceleration pickers below — they all thread
                # ``collation_obj`` through so a sort on a collation-
                # indexed string field walks the index when the query's
                # collation matches and falls back to a Python sort
                # otherwise.
                candidates = self._try_index_lookup(db, coll, filter, collation=collation_obj)
                if candidates is not None and sort_field is not None:
                    if (
                        len(filter) == 1
                        and not next(iter(filter)).startswith("$")
                        and next(iter(filter)) == sort_field
                    ):
                        in_sort_order = True
                        idx = self._find_leading_field_index(
                            db, coll, sort_field, filter, collation=collation_obj
                        )
                        idx_dir = idx[1] if idx else 1
                        if sort_dir != idx_dir:
                            candidates = list(reversed(candidates))
                elif candidates is None and not filter and sort_field is not None:
                    idx = self._find_leading_field_index(
                        db, coll, sort_field, filter, collation=collation_obj
                    )
                    if idx is not None:
                        idx_name, idx_dir, _is_compound = idx
                        # If the index direction matches the sort direction,
                        # walk forward; if it's opposite, walk backward.
                        reverse = sort_dir != idx_dir
                        candidates = self._walk_index_in_order(db, coll, idx_name, reverse=reverse)
                        in_sort_order = True
                # Multi-field sort acceleration: when sort has 2+ fields and
                # filter is empty, try to find a compound index whose key
                # spec exactly matches (or fully inverts) the sort. Walking
                # that index in the right direction yields the requested
                # order without a Python-side post-sort.
                if candidates is None and not filter and sort_field is None and sort:
                    multi_spec = self._multi_sort_spec(sort)
                    if multi_spec is not None and len(multi_spec) > 1:
                        match = self._compound_index_for_sort(
                            db, coll, multi_spec, collation=collation_obj
                        )
                        if match is not None:
                            idx_name, reverse = match
                            candidates = self._walk_index_in_order(
                                db, coll, idx_name, reverse=reverse
                            )
                            in_sort_order = True
                if candidates is None:
                    # COLLSCAN: no usable index. mongod returns these in
                    # insertion (natural) order, not _id order — walk the
                    # natural-order index. (When a sort is applied the post-sort
                    # below reorders anyway, and feeding it insertion order makes
                    # equal-key ties break like mongod's RecordId order.)
                    raw_blobs = [b for _, b in self._scan_docs_natural(db, coll)]
        if candidates is None:
            assert raw_blobs is not None
            candidates = [bson.decode(b) for b in raw_blobs]
        out = [d for d in candidates if matches(d, filter, vars=let, collation=collation_obj)]
        if min_bound is not None or max_bound is not None:
            out = self._apply_minmax_bounds(
                db, coll, out, hint, min_bound, max_bound, collation_obj
            )
        if sort and not in_sort_order:
            out = sort_docs(out, sort)
        if skip:
            out = out[skip:]
        if limit > 0:
            out = out[:limit]
        if projection:
            out = [apply_projection(d, projection) for d in out]
        return out

    def _apply_minmax_bounds(
        self,
        db: str,
        coll: str,
        docs: list[dict[str, Any]],
        hint: str | Mapping[str, Any] | None,
        min_bound: Mapping[str, Any] | None,
        max_bound: Mapping[str, Any] | None,
        collation: Any,
    ) -> list[dict[str, Any]]:
        """Filter ``docs`` by cursor ``min`` / ``max`` index bounds.

        ``max`` is an exclusive upper bound, ``min`` an inclusive lower
        bound, evaluated on the hinted index's key (mongod semantics).
        The bound documents must name a leading prefix of the hinted
        index's key fields, in the same order — otherwise mongod raises
        51174, which we mirror via ``MinMaxKeyError``. Bounds and docs
        are encoded with the same ``_index_key`` direction-aware byte
        encoder, so a byte comparison reflects the index's natural order
        (cross-type, per-field direction).
        """
        if hint is None:
            raise MinMaxKeyError("min/max requires a hint")
        resolved = self._resolve_hint(db, coll, hint)
        key_spec: dict[str, Any] | None = None
        if resolved == _ID_INDEX_NAME:
            key_spec = {"_id": 1}
        else:
            for name, ks, _sparse, _unique in self._all_indexes(db, coll):
                if name == resolved:
                    key_spec = dict(ks)
                    break
        if key_spec is None:
            raise MinMaxKeyError("min/max hint does not correspond to an index")
        index_fields = list(key_spec)

        def _bound_spec(bound: Mapping[str, Any]) -> dict[str, Any]:
            bound_fields = list(bound)
            if bound_fields != index_fields[: len(bound_fields)]:
                raise MinMaxKeyError(
                    "The field order of the min/max query option does not "
                    "match the order of the hinted index's key pattern"
                )
            return {f: key_spec[f] for f in bound_fields}

        min_key = (
            _index_key(dict(min_bound), _bound_spec(min_bound), sparse=False, collation=collation)
            if min_bound is not None
            else None
        )
        max_key = (
            _index_key(dict(max_bound), _bound_spec(max_bound), sparse=False, collation=collation)
            if max_bound is not None
            else None
        )

        def _in_bounds(doc: dict[str, Any]) -> bool:
            if min_key is not None:
                dk = _index_key(doc, _bound_spec(min_bound), sparse=False, collation=collation)
                if dk is None or dk < min_key:  # min is inclusive
                    return False
            if max_key is not None:
                dk = _index_key(doc, _bound_spec(max_bound), sparse=False, collation=collation)
                if dk is None or dk >= max_key:  # max is exclusive
                    return False
            return True

        return [d for d in docs if _in_bounds(d)]

    def _resolve_hint(self, db: str, coll: str, hint: str | Mapping[str, Any]) -> str:
        """Resolve ``hint`` to an index name (or ``$natural``).

        ``hint`` may be an index name string, a key-spec dict matching an
        existing index, ``"$natural"``, or ``{"$natural": +/-1}``. Anything
        else raises ``BadHint`` so the command layer can return a Mongo
        ``BadValue`` error.
        """
        if isinstance(hint, str):
            if hint == "$natural":
                return "$natural"
            if hint == _ID_INDEX_NAME:
                return _ID_INDEX_NAME
            for name, _key_spec, _sparse, _unique in self._all_indexes(db, coll):
                if name == hint:
                    return name
            raise BadHint(f"hint {hint!r} does not correspond to an existing index")
        if isinstance(hint, Mapping):
            if list(hint) == ["$natural"]:
                return "$natural"
            if list(hint) == ["_id"] and int(hint["_id"]) == 1:
                return _ID_INDEX_NAME
            for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
                if dict(key_spec) == dict(hint):
                    return name
            raise BadHint(f"hint {dict(hint)!r} does not correspond to an existing index")
        raise BadHint(f"invalid hint type: {type(hint).__name__}")

    def _candidates_from_hint(
        self,
        db: str,
        coll: str,
        resolved: str,
        sort_field: str | None,
        sort_dir: int,
    ) -> tuple[list[dict[str, Any]], bool]:
        """Walk the index named by ``resolved`` (or full collection for $natural).

        Returns ``(candidates, in_sort_order)`` where ``in_sort_order`` is
        True when the hint's leading field matches the sort field — in
        which case ``find_matching`` skips the post-sort step.
        """
        if resolved == "$natural":
            # $natural == insertion order (mongod's RecordId store order).
            return [bson.decode(b) for _, b in self._scan_docs_natural(db, coll)], False
        if resolved == _ID_INDEX_NAME:
            # The doc table is keyed by id_key; iterating it gives entries
            # sorted by encoded _id, which matches the _id_ index walk.
            docs = [bson.decode(b) for _, b in self._scan_docs(db, coll)]
            in_order = sort_field == "_id"
            if in_order and sort_dir == -1:
                docs = list(reversed(docs))
            return docs, in_order
        # Find the index's leading field and its direction
        leading: str | None = None
        leading_dir = 1
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if name == resolved:
                first = next(iter(key_spec))
                leading = first
                leading_dir = int(key_spec[first])
                break
        candidates = self._walk_index_in_order(db, coll, resolved, reverse=False)
        in_order = sort_field is not None and sort_field == leading
        if in_order and sort_dir != leading_dir:
            candidates = list(reversed(candidates))
        return candidates, in_order

    @staticmethod
    def _single_sort_spec(sort: Mapping[str, Any] | None) -> tuple[str | None, int]:
        """Return ``(field, direction)`` if ``sort`` is single-field +/-1, else ``(None, 0)``."""
        if not sort or len(sort) != 1:
            return None, 0
        f, d = next(iter(sort.items()))
        if f.startswith("$"):
            return None, 0
        try:
            di = int(d)
        except (TypeError, ValueError):
            return None, 0
        if di not in (-1, 1):
            return None, 0
        return f, di

    @staticmethod
    def _multi_sort_spec(
        sort: Mapping[str, Any] | None,
    ) -> list[tuple[str, int]] | None:
        """Return a list of ``(field, direction)`` pairs for a multi-field
        sort spec, or ``None`` if any entry is operator-prefixed or has a
        non-``±1`` direction.

        Used for compound-index sort acceleration: an index whose key
        spec exactly matches (or fully inverts) the returned list lets
        ``find_matching`` walk WT in the requested order and skip the
        Python-side post-sort entirely.
        """
        if not sort:
            return None
        out: list[tuple[str, int]] = []
        for field, direction in sort.items():
            if field.startswith("$"):
                return None
            try:
                d = int(direction)
            except (TypeError, ValueError):
                return None
            if d not in (-1, 1):
                return None
            out.append((field, d))
        return out

    def _compound_index_for_sort(
        self,
        db: str,
        coll: str,
        sort_fields: list[tuple[str, int]],
        *,
        collation: Any = None,
    ) -> tuple[str, bool] | None:
        """Find a compound index that satisfies ``sort_fields`` end-to-end.

        Returns ``(index_name, reverse_walk)`` where ``reverse_walk`` is
        True when the matching index is the *fully-inverted* permutation
        of the sort (walking backward yields the requested order).

        Multikey indexes are excluded — array values in the index could
        produce row order that doesn't match the BSON cross-type sort
        the user expects from a sort spec, so we'd fall back to Python
        sort anyway.

        Strict match only: the index key spec must have the same fields
        in the same order with directions either matching the sort spec
        or being the full inverse. Partial-prefix matches (sort uses 3
        fields, index has 2) aren't accelerated; the savings on the
        leading prefix are usually less than the cost of the trailing
        Python sort over the materialised set.

        ``collation``: same exact-match gate as the filter pickers — an
        index is only considered if its stored collation parses to the
        same :class:`Collation` as the query's (or both None). A
        no-collation sort against a collation-having index would walk
        the index in collation order rather than codepoint order, which
        is wrong for the user; the reverse is also wrong. So mismatched
        indexes are skipped and the caller falls back to a Python sort.
        """
        multikey = self._multikey_index_names(db, coll)
        index_options = self._index_options_map(db, coll)
        target = list(sort_fields)
        inverted = [(f, -d) for f, d in target]
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if name in multikey:
                continue
            try:
                idx_pairs = [(f, int(d)) for f, d in key_spec.items()]
            except (TypeError, ValueError):
                continue
            if any(d not in (-1, 1) for _, d in idx_pairs):
                continue
            idx_coll = _parse_index_collation(index_options.get(name, {}).get("collation"))
            if idx_coll != collation:
                continue
            if idx_pairs == target:
                return name, False
            if idx_pairs == inverted:
                return name, True
        return None

    def _single_field_index_for(self, db: str, coll: str, field: str) -> tuple[str, int] | None:
        """Return ``(index_name, direction)`` for a single-field index on
        ``field``, or ``None`` if no such index exists. Direction is the
        index's stored sort direction (`+1` for ASC, `-1` for DESC)."""
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if list(key_spec.keys()) == [field]:
                d = int(key_spec[field])
                if d in (1, -1):
                    return name, d
        return None

    def _walk_index_in_order(
        self, db: str, coll: str, name: str, *, reverse: bool = False
    ) -> list[dict[str, Any]]:
        c = self._cursor(_IDX_ENTRIES_TABLE)
        c.set_key(db, coll, name, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        id_keys: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            _esc, row_id = _unpack_entry(packed)
            id_keys.append(row_id)
            if c.next() != 0:
                break
        if reverse:
            id_keys.reverse()
        return self._docs_by_id_keys(db, coll, id_keys)

    def explain_plan(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        sort: Mapping[str, Any] | None = None,
        hint: str | Mapping[str, Any] | None = None,
        collation: Any = None,
    ) -> dict[str, Any]:
        """Plan summary for what ``find_matching`` would do with these args.

        No execution; mirrors the same routing decisions. Returns
        ``{"kind": "COLLSCAN"}`` or ``{"kind": "IXSCAN", "index_name",
        "key_pattern", "direction"}``. ``direction`` is ``"forward"``
        unless a sort spec inverts it relative to the chosen index.

        ``collation``: mirrors the runtime gate — when set, only
        indexes whose stored ``collation`` matches the query's are
        considered for string-bearing predicates. Mismatched indexes
        produce COLLSCAN, same as ``find_matching`` would.
        """
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        filter = filter or {}
        with self._lock:
            sort_field, sort_dir = self._single_sort_spec(sort)
            if hint is not None:
                try:
                    resolved = self._resolve_hint(db, coll, hint)
                except BadHint:
                    return {"kind": "COLLSCAN"}
                if resolved == "$natural":
                    return {"kind": "COLLSCAN"}
                if resolved == _ID_INDEX_NAME:
                    direction = "forward"
                    if sort_field == "_id" and sort_dir == -1:
                        direction = "backward"
                    return {
                        "kind": "IXSCAN",
                        "index_name": _ID_INDEX_NAME,
                        "key_pattern": {"_id": 1},
                        "direction": direction,
                    }
                key_spec = self._key_spec_for(db, coll, resolved)
                if key_spec is None:
                    return {"kind": "COLLSCAN"}
                return self._make_ixscan_plan(resolved, key_spec, sort_field, sort_dir)
            picked = self._pick_index_for_filter(db, coll, filter, collation=collation_obj)
            if picked is not None:
                name, key_spec = picked
                return self._make_ixscan_plan(name, key_spec, sort_field, sort_dir)
            if not filter and sort_field is not None:
                idx = self._find_leading_field_index(
                    db, coll, sort_field, filter, collation=collation_obj
                )
                if idx is not None:
                    name, _idx_dir, _is_compound = idx
                    key_spec = self._key_spec_for(db, coll, name)
                    if key_spec is not None:
                        return self._make_ixscan_plan(name, key_spec, sort_field, sort_dir)
            # Multi-field sort acceleration mirrored in the planner: same
            # rules as find_matching (compound key spec exactly matches
            # or fully inverts the sort, filter empty).
            if not filter and sort_field is None and sort:
                multi_spec = self._multi_sort_spec(sort)
                if multi_spec is not None and len(multi_spec) > 1:
                    match = self._compound_index_for_sort(
                        db, coll, multi_spec, collation=collation_obj
                    )
                    if match is not None:
                        name, reverse = match
                        key_spec = self._key_spec_for(db, coll, name)
                        if key_spec is not None:
                            return {
                                "kind": "IXSCAN",
                                "index_name": name,
                                "key_pattern": key_spec,
                                "direction": "backward" if reverse else "forward",
                            }
            return {"kind": "COLLSCAN"}

    def _key_spec_for(self, db: str, coll: str, name: str) -> dict[str, Any] | None:
        for n, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            if n == name:
                return dict(key_spec)
        return None

    def _pick_geo_index_for_filter(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> tuple[str, dict[str, Any]] | None:
        """Mirror :meth:`_try_geo_index_id_keys`'s index selection (no exec).

        Returns ``(name, key_spec)`` if the filter has a geo operator on
        a geo-indexed field; ``None`` otherwise. The picker is exact —
        ``_try_geo_index_id_keys`` may still bail (e.g. ``$near`` with no
        max distance), but ``explain`` reports IXSCAN whenever an index
        *could* serve the query, matching mongod's planner explain.
        """
        for field, value in filter.items():
            if not isinstance(value, dict):
                continue
            if not any(op in value for op in self._GEO_OPS):
                continue
            for name, key_spec, _opts in self._iter_indexes(db, coll):
                geo = _geo_type_of(key_spec)
                if geo is not None and geo[0] == field:
                    return name, dict(key_spec)
        return None

    def _pick_index_for_filter(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        collation: Any = None,
    ) -> tuple[str, dict[str, Any]] | None:
        """Mirror ``_try_index_lookup``'s index-selection (no execution).

        ``collation`` propagates from the query; when set, only
        indexes with a matching stored ``collation`` option are
        considered. Single-field, compound bare-eq, and compound
        prefix + trailing-operator pickers all collation-match;
        ``numericOrdering`` queries fall through to COLLSCAN.
        """
        if not filter:
            return None
        if any(f.startswith("$") for f in filter):
            return None
        # Mirror the _id point-lookup fast path: report it as an IXSCAN on
        # the virtual _id_ index (key pattern {_id: 1}), matching mongod.
        # Timeseries collections fall through to COLLSCAN (suffixed keys).
        if (
            len(filter) == 1
            and "_id" in filter
            and _id_point_lookup_keys(filter["_id"]) is not None
            and not self._is_timeseries(db, coll)
        ):
            return _ID_INDEX_NAME, {"_id": 1}
        # Mirror `_try_index_id_keys`: geo dispatch first.
        geo_pick = self._pick_geo_index_for_filter(db, coll, filter)
        if geo_pick is not None:
            return geo_pick
        if all(not isinstance(v, dict) for v in filter.values()):
            picked = self._pick_compound_eq_index(db, coll, filter, collation=collation)
            if picked is not None:
                return picked
        if len(filter) >= 2:
            picked = self._pick_compound_range_index(db, coll, filter, collation=collation)
            if picked is not None:
                return picked
        if len(filter) == 1:
            field, value = next(iter(filter.items()))
            # Mirror the lookup: {field: {$exists: true}} → sparse index IXSCAN.
            if isinstance(value, dict) and len(value) == 1 and value.get("$exists"):
                name = self._sparse_index_for_exists(db, coll, field)
                if name is None:
                    return None
                key_spec = self._key_spec_for(db, coll, name)
                return (name, key_spec) if key_spec is not None else None
            idx_match = self._find_leading_field_index(db, coll, field, filter, collation=collation)
            if idx_match is None:
                return None
            if isinstance(value, dict):
                if not value or not all(k.startswith("$") for k in value):
                    return None
                if not all(op in self._RANGE_OPS for op in value):
                    return None
            name, _direction, _is_compound = idx_match
            key_spec = self._key_spec_for(db, coll, name)
            if key_spec is None:
                return None
            return name, key_spec
        # Multi-field filter: mirror the lookup's single-field + partial-absorbed
        # residual path so explain reports IXSCAN (with isPartial) where the
        # query would actually use the index.
        match = self._single_field_partial_residual_match(db, coll, filter, collation=collation)
        if match is None:
            return None
        name = match[2][0]
        key_spec = self._key_spec_for(db, coll, name)
        if key_spec is None:
            return None
        return name, key_spec

    @staticmethod
    def _make_ixscan_plan(
        name: str,
        key_spec: Mapping[str, Any],
        sort_field: str | None,
        sort_dir: int,
    ) -> dict[str, Any]:
        direction = "forward"
        if sort_field is not None and sort_field in key_spec:
            idx_dir = int(key_spec[sort_field])
            if sort_dir != 0 and sort_dir != idx_dir:
                direction = "backward"
        return {
            "kind": "IXSCAN",
            "index_name": name,
            "key_pattern": dict(key_spec),
            "direction": direction,
        }

    def count_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        let: dict[str, Any] | None = None,
        collation: Any = None,
    ) -> int:
        if self._is_oplog_rs(db, coll):
            return self._count_oplog_rs(filter, let=let, collation=collation)
        if self._is_system_users(db, coll):
            return self._count_system_users(filter, let=let, collation=collation)
        if self._is_system_version(db, coll):
            return self._count_system_version(filter, let=let, collation=collation)
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        self._refresh_read_snapshot()
        if not filter:
            with self._lock:
                return sum(1 for _ in self._scan_docs(db, coll))
        return sum(
            1
            for doc in self._all_docs(db, coll)
            if matches(doc, filter, vars=let, collation=collation_obj)
        )

    def collection_data_size(self, db: str, coll: str) -> int:
        """Sum of bson-encoded doc bytes for ``coll``.

        Used by ``collStats`` / ``dbStats`` for ``size`` / ``dataSize``.
        Best-effort estimate — doesn't include WT block overhead.
        """
        with self._lock:
            return sum(len(blob) for _id_k, blob in self._scan_docs(db, coll))

    def index_sizes(self, db: str, coll: str) -> dict[str, int]:
        """Map of index name → sum of packed entry-key bytes.

        ``_id_`` is reported separately as ``len(id_key)`` summed across
        the doc table, so callers can include it alongside secondary
        indexes for an accurate ``totalIndexSize``.
        """
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            sizes: dict[str, int] = {}
            id_size = sum(len(id_k) for id_k, _blob in self._scan_docs(db, coll))
            if id_size:
                sizes[_ID_INDEX_NAME] = id_size
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll))
            for k, _v in entry_rows:
                name = k[2]
                packed = bytes(k[3])
                sizes[name] = sizes.get(name, 0) + len(packed)
            return sizes

    @_retry_write_conflicts
    def update_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        update: dict[str, Any],
        *,
        multi: bool = False,
        upsert: bool = False,
        array_filters: list[dict[str, Any]] | None = None,
        let: dict[str, Any] | None = None,
        collation: Any = None,
        validator: dict[str, Any] | None = None,
        journal: bool = False,
    ) -> dict[str, Any]:
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        # Release any sticky session snapshot before the write's
        # ``begin_transaction`` acquires a new one. Otherwise the
        # transaction inherits a stale view and the candidate scan
        # misses rows committed by other connections (the cross-
        # connection visibility fix applied to reads — see
        # ``_refresh_read_snapshot``).
        self._refresh_read_snapshot()
        matched = 0
        modified = 0
        upserted_id: Any = None
        did_upsert = False
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        oplog_on = self.enable_oplog
        with self._coll_lock(db, coll), self._batch_transaction(sync=journal):
            # Per-collection lock + one WT transaction per call. Every
            # doc-table write + index-entry delete/insert + oplog write
            # that lands in this method shares a single commit. Phase
            # 2.4: was self._lock; now per-coll so different
            # collections update in parallel.
            self._ensure_collection(db, coll)
            ns = self._ns(db, coll)
            ui = self._collection_uuid(db, coll) if oplog_on else None
            preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            multikey_names = self._multikey_index_names(db, coll)
            # Index-routed when the filter is covered (only matching id_keys
            # come back from the index walk); full scan otherwise. Either
            # way the doc cursor isn't held across writes — bytes are
            # eagerly buffered. Only matching docs pay ``bson.decode``.
            # With a collation in effect, fall back to a doc-table scan:
            # the index entries don't carry the collation's folding, so
            # an indexed equality probe would miss case-insensitive
            # matches. Always materialise the list — the update loop
            # rewrites the doc table via the cached cursor, which
            # invalidates a still-walking ``_scan_docs`` cursor on the
            # same session.
            if collation_obj is not None:
                candidates = list(self._scan_docs(db, coll))
            else:
                candidates = self._candidates_iter(db, coll, filter)
            for id_k, blob in candidates:
                doc = bson.decode(blob)
                if not matches(doc, filter, vars=let, collation=collation_obj):
                    continue
                matched += 1
                pos = find_positional_matches(doc, filter)
                new = apply_update(
                    doc,
                    update,
                    array_filters=array_filters,
                    positional_matches=pos,
                    let=let,
                )
                if new != doc:
                    # Document-validator check: collection-level
                    # ``validator`` (set via ``create`` / ``collMod``)
                    # rejects updates whose result fails the predicate.
                    # Caller passes ``None`` to skip
                    # (``bypassDocumentValidation: true``).
                    if validator is not None and not matches(new, validator):
                        raise DocumentValidationError(new.get("_id"))
                    # _id is immutable, so the row's actual key is the right
                    # write target. For ordinary collections that equals
                    # _id_key(new["_id"]); for timeseries the row key carries
                    # a uniqueness suffix that a recompute would drop —
                    # writing at the recomputed key would strand the old row.
                    new_id_key = id_k
                    conflict = self._unique_conflict(
                        db, coll, new, indexes, exclude_id_key=id_k, partials=partials
                    )
                    if conflict is not None:
                        cname, kpat, kval = conflict
                        raise IndexConflict(
                            cname,
                            new["_id"],
                            key_pattern=kpat,
                            key_value=kval,
                            namespace=f"{db}.{coll}",
                        )
                    # Geo validation must reject the update before any
                    # write happens, otherwise we'd be left with a
                    # half-deleted set of index entries.
                    self._validate_geo_indexes(db, coll, new, indexes, partials)
                    new_blob = bson.encode(new)
                    if len(new_blob) > MAX_BSON_OBJECT_SIZE:
                        raise DocumentTooLargeError(
                            10334,
                            "Plan executor error during update :: caused by :: "
                            f"Resulting document after update is larger than "
                            f"{MAX_BSON_OBJECT_SIZE}",
                        )
                    modified += 1
                    self._delete_index_entries(
                        db, coll, doc, indexes, partials, id_key_override=id_k
                    )
                    doc_cur = self._cursor(_DOC_TABLE)
                    doc_cur[db, coll, new_id_key] = new_blob
                    self._write_index_entries(
                        db, coll, new, indexes, partials, id_key_override=id_k
                    )
                    multikey_names = self._maybe_mark_multikey(
                        db, coll, new, indexes, multikey_names
                    )
                    # Pipeline-form updates (a list of stages) are
                    # diff-style in the oplog — mongod emits op "u" with
                    # an update description (the unified "array
                    # truncation" spec asserts operationType "update",
                    # not "replace").
                    is_replacement = not isinstance(update, list) and not any(
                        isinstance(k, str) and k.startswith("$") for k in update
                    )
                    if oplog_on:
                        if is_replacement:
                            o_field: dict[str, Any] = dict(new)
                        else:
                            o_field = {"$v": 2, "diff": compute_update_description(doc, new)}
                        oplog_entries.append(
                            {
                                "op": "u",
                                "ns": ns,
                                "ui": bson.Binary(ui.bytes, subtype=4),
                                "o": o_field,
                                "o2": {"_id": doc["_id"]},
                            }
                        )
                        pre_images.append(bson.encode(doc) if preimages_on else None)
                if not multi:
                    break
            if matched == 0 and upsert:
                seed: dict[str, Any] = {}
                for k, v in filter.items():
                    # Seed bare-equality predicates into the upserted doc.
                    # A dict value is only skipped when it's an OPERATOR
                    # expression ({$gt: 5}); a literal subdocument value
                    # ({f: ..., f2: ...}, e.g. a compound ``_id``) is a
                    # real equality and must be seeded — Python's
                    # ``isinstance(v, dict)`` alone wrongly drops it,
                    # generating a fresh ObjectId instead.
                    if k.startswith("$") or _is_operator_expr(v):
                        continue
                    seed[k] = v
                new = apply_update(seed, update, is_upsert=True, array_filters=array_filters)
                if "_id" not in new:
                    new["_id"] = bson.ObjectId()
                if validator is not None and not matches(new, validator):
                    raise DocumentValidationError(new.get("_id"))
                upserted_id = new["_id"]
                did_upsert = True
                conflict = self._unique_conflict(
                    db, coll, new, indexes, exclude_id_key=None, partials=partials
                )
                if conflict is not None:
                    cname, kpat, kval = conflict
                    raise IndexConflict(
                        cname,
                        new["_id"],
                        key_pattern=kpat,
                        key_value=kval,
                        namespace=f"{db}.{coll}",
                    )
                self._validate_geo_indexes(db, coll, new, indexes, partials)
                upsert_blob = bson.encode(new)
                if len(upsert_blob) > MAX_BSON_OBJECT_SIZE:
                    raise DocumentTooLargeError(
                        17420,
                        "Plan executor error during update :: caused by :: "
                        f"Document to upsert is larger than {MAX_BSON_OBJECT_SIZE}",
                    )
                doc_cur = self._cursor(_DOC_TABLE)
                doc_cur[db, coll, _id_key(upserted_id)] = upsert_blob
                self._write_index_entries(db, coll, new, indexes, partials)
                self._write_nat_entry(db, coll, _id_key(upserted_id))
                self._maybe_mark_multikey(db, coll, new, indexes, multikey_names)
                if oplog_on:
                    oplog_entries.append(
                        {
                            "op": "i",
                            "ns": ns,
                            "ui": bson.Binary(ui.bytes, subtype=4),
                            "o": dict(new),
                            "o2": {"_id": upserted_id},
                        }
                    )
                    pre_images.append(None)
            cap_ns = ns if oplog_on else ""
            cap_entries, cap_pre = self._enforce_capped_bounds_locked(
                db, coll, set(), indexes, partials, oplog_on, cap_ns, ui
            )
            if cap_entries:
                oplog_entries.extend(cap_entries)
                pre_images.extend(cap_pre)
            if oplog_entries:
                self._emit_oplog(oplog_entries, pre_images)
        return {
            "matched": matched,
            "modified": modified,
            "upserted_id": upserted_id,
            "did_upsert": did_upsert,
        }

    @_retry_write_conflicts
    def delete_matching(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        limit: int = 0,
        let: dict[str, Any] | None = None,
        collation: Any = None,
        journal: bool = False,
    ) -> int:
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        # See ``update_matching`` — release the sticky snapshot so the
        # candidate scan sees writes committed by other connections.
        self._refresh_read_snapshot()
        deleted = 0
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        oplog_on = self.enable_oplog
        with self._coll_lock(db, coll), self._batch_transaction(sync=journal):
            # Per-collection lock (Phase 2.4) + one WT transaction.
            # Groups the per-doc removes + index-entry deletes + oplog
            # writes into one commit. Other collections delete in
            # parallel.
            ns = self._ns(db, coll) if oplog_on else ""
            preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
            ui = (
                self._collection_uuid(db, coll)
                if oplog_on and self._coll_options(db, coll) is not None
                else None
            )
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            # Index-routed candidates when the filter is covered; full scan
            # otherwise. See update_matching for the full-scan rationale.
            # Collation forces a full scan — index entries don't carry the
            # collation's folding. Always materialise into a list so the
            # delete loop's writes don't invalidate the iteration cursor
            # mid-scan (deletes via ``_cursor(_DOC_TABLE)`` share the
            # cached cursor with ``_scan_docs``).
            if collation_obj is not None:
                candidates = list(self._scan_docs(db, coll))
            else:
                candidates = self._candidates_iter(db, coll, filter)
            for id_k, blob in candidates:
                doc = bson.decode(blob)
                if not matches(doc, filter, vars=let, collation=collation_obj):
                    continue
                self._delete_index_entries(db, coll, doc, indexes, partials, id_key_override=id_k)
                doc_cur = self._cursor(_DOC_TABLE)
                doc_cur.set_key(db, coll, id_k)
                doc_cur.remove()
                self._delete_nat_entry(db, coll, id_k)
                deleted += 1
                if oplog_on:
                    entry: dict[str, Any] = {
                        "op": "d",
                        "ns": ns,
                        "o": {"_id": doc["_id"]},
                        "o2": {"_id": doc["_id"]},
                    }
                    if ui is not None:
                        entry["ui"] = bson.Binary(ui.bytes, subtype=4)
                    oplog_entries.append(entry)
                    pre_images.append(bson.encode(doc) if preimages_on else None)
                if limit > 0 and deleted >= limit:
                    break
            if oplog_entries:
                self._emit_oplog(oplog_entries, pre_images)
        return deleted

    def prune_ttl(
        self,
        db: str,
        coll: str,
        *,
        now: _dt.datetime | None = None,
    ) -> int:
        """Delete docs whose indexed Date field is older than now - TTL.

        For every index on ``coll`` with an ``expireAfterSeconds`` option,
        walks the collection and deletes docs whose indexed field resolves
        to a ``datetime`` older than ``now - expireAfterSeconds``. Docs
        without the field, with non-date values, or with values inside the
        TTL window are left in place. Real MongoDB runs this on a 60s
        background sweeper; SecantusDB invokes it explicitly so tests can
        drive expiry with an injected ``now``. Returns the number of docs
        pruned.
        """
        ttl_indexes: list[tuple[str, str, float]] = []
        for name, key_spec, opts in self._iter_indexes(db, coll):
            ttl = opts.get("expireAfterSeconds")
            if not isinstance(ttl, (int, float)) or ttl < 0:
                continue
            field = next(iter(key_spec), None)
            if not isinstance(field, str):
                continue
            ttl_indexes.append((name, field, float(ttl)))
        if not ttl_indexes:
            return 0
        when = now if now is not None else _dt.datetime.now(_dt.timezone.utc)
        if when.tzinfo is None:
            when = when.replace(tzinfo=_dt.timezone.utc)
        pruned = 0
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        with self._lock:
            ns = self._ns(db, coll)
            preimages_on = self._pre_post_images_enabled(db, coll)
            ui = (
                self._collection_uuid(db, coll)
                if self._coll_options(db, coll) is not None
                else None
            )
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            candidates = list(self._scan_docs(db, coll))
            for id_k, blob in candidates:
                doc = bson.decode(blob)
                expired = False
                for _name, field, ttl_seconds in ttl_indexes:
                    value = get_path(doc, field)
                    if not isinstance(value, _dt.datetime):
                        continue
                    value_aware = value if value.tzinfo else value.replace(tzinfo=_dt.timezone.utc)
                    if (when - value_aware).total_seconds() > ttl_seconds:
                        expired = True
                        break
                if not expired:
                    continue
                self._delete_index_entries(db, coll, doc, indexes, partials, id_key_override=id_k)
                doc_cur = self._cursor(_DOC_TABLE)
                doc_cur.set_key(db, coll, id_k)
                doc_cur.remove()
                self._delete_nat_entry(db, coll, id_k)
                pruned += 1
                entry: dict[str, Any] = {
                    "op": "d",
                    "ns": ns,
                    "o": {"_id": doc["_id"]},
                    "o2": {"_id": doc["_id"]},
                }
                if ui is not None:
                    entry["ui"] = bson.Binary(ui.bytes, subtype=4)
                oplog_entries.append(entry)
                pre_images.append(bson.encode(doc) if preimages_on else None)
            if oplog_entries:
                self._emit_oplog(oplog_entries, pre_images)
        return pruned

    @staticmethod
    def _table_kf(table: str) -> str:
        return {
            _COLL_TABLE: "SS",
            _DOC_TABLE: "SSu",
            _NAT_TABLE: "SSq",
            _NAT_SEQ_TABLE: "SSu",
            _IDX_TABLE: "SSS",
            _IDX_ENTRIES_TABLE: "SSSu",
        }[table]

    @staticmethod
    def _smallest_for_kf(kf: str) -> tuple[Any, ...]:
        # Smallest value per WT column type: ``u`` -> empty bytes, ``q`` ->
        # INT64_MIN (the lowest int64 key), ``S`` -> empty string.
        return tuple(b"" if c == "u" else _INT64_MIN if c == "q" else "" for c in kf)

    def _collect_prefix(
        self, table: str, prefix: tuple[Any, ...]
    ) -> list[tuple[tuple[Any, ...], Any]]:
        c = self._cursor(table)
        kf = self._table_kf(table)
        seed = prefix + self._smallest_for_kf(kf)[len(prefix) :]
        c.set_key(*seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[tuple[tuple[Any, ...], Any]] = []
        while True:
            k = tuple(c.get_key())
            if k[: len(prefix)] != prefix:
                break
            v = c.get_value()
            out.append((k, bytes(v) if isinstance(v, (bytes, bytearray)) else v))
            if c.next() != 0:
                break
        return out

    def _delete_keys(self, table: str, keys: list[tuple[Any, ...]]) -> None:
        if not keys:
            return
        c = self._cursor(table)
        for k in keys:
            c.set_key(*k)
            c.remove()
            c.reset()

    def drop_collection(self, db: str, coll: str) -> bool:
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            existed = self._coll_options(db, coll) is not None
            ui = self._collection_uuid(db, coll) if existed else None
            for tbl in (_DOC_TABLE, _NAT_TABLE, _NAT_SEQ_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                rows = self._collect_prefix(tbl, (db, coll))
                self._delete_keys(tbl, [k for k, _ in rows])
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, coll)
            if c.search() == 0:
                c.remove()
            if existed and ui is not None:
                self._emit_oplog(
                    [
                        {
                            "op": "c",
                            "ns": f"{db}.$cmd",
                            "ui": bson.Binary(ui.bytes, subtype=4),
                            "o": {"drop": coll},
                        }
                    ]
                )
            return existed

    def drop_database(self, db: str) -> None:
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            colls_with_ui: list[tuple[str, _uuid.UUID]] = []
            for c_name in self.list_collections(db):
                ui = self._collection_uuid(db, c_name)
                colls_with_ui.append((c_name, ui))
            for tbl in (
                _DOC_TABLE,
                _NAT_TABLE,
                _NAT_SEQ_TABLE,
                _IDX_TABLE,
                _IDX_ENTRIES_TABLE,
                _COLL_TABLE,
            ):
                rows = self._collect_prefix(tbl, (db,))
                self._delete_keys(tbl, [k for k, _ in rows])
            entries: list[dict[str, Any]] = []
            for c_name, ui in colls_with_ui:
                entries.append(
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {"drop": c_name},
                    }
                )
            entries.append({"op": "c", "ns": f"{db}.$cmd", "o": {"dropDatabase": 1}})
            self._emit_oplog(entries)

    def rename_collection(
        self,
        src_db: str,
        src_coll: str,
        dst_db: str,
        dst_coll: str,
        *,
        drop_target: bool = False,
    ) -> tuple[bool, str | None]:
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            if self._coll_options(src_db, src_coll) is None:
                return False, f"source namespace does not exist: {src_db}.{src_coll}"
            if (src_db, src_coll) == (dst_db, dst_coll):
                return True, None
            ui = self._collection_uuid(src_db, src_coll)
            dst_existed = self._coll_options(dst_db, dst_coll) is not None
            dst_ui = self._collection_uuid(dst_db, dst_coll) if dst_existed else None
            if dst_existed:
                if not drop_target:
                    return False, f"target namespace exists: {dst_db}.{dst_coll}"
                for tbl in (_DOC_TABLE, _NAT_TABLE, _NAT_SEQ_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                    rows = self._collect_prefix(tbl, (dst_db, dst_coll))
                    self._delete_keys(tbl, [k for k, _ in rows])
                c = self._cursor(_COLL_TABLE)
                c.set_key(dst_db, dst_coll)
                if c.search() == 0:
                    c.remove()
            for tbl in (_DOC_TABLE, _NAT_TABLE, _NAT_SEQ_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE):
                rows = self._collect_prefix(tbl, (src_db, src_coll))
                self._delete_keys(tbl, [k for k, _ in rows])
                c = self._cursor(tbl)
                for k, v in rows:
                    new_k = (dst_db, dst_coll) + k[2:]
                    c.set_key(*new_k)
                    c.set_value(v)
                    c.insert()
                    c.reset()
            ensure = self._cursor(_COLL_TABLE)
            ensure.set_key(dst_db, dst_coll)
            if ensure.search() != 0:
                ensure.reset()
                ensure[dst_db, dst_coll] = b""
            ensure.reset()
            ensure.set_key(src_db, src_coll)
            if ensure.search() == 0:
                ensure.remove()
            entries: list[dict[str, Any]] = []
            if dst_existed and dst_ui is not None:
                entries.append(
                    {
                        "op": "c",
                        "ns": f"{dst_db}.$cmd",
                        "ui": bson.Binary(dst_ui.bytes, subtype=4),
                        "o": {"drop": dst_coll},
                    }
                )
            rename_o: dict[str, Any] = {
                "renameCollection": f"{src_db}.{src_coll}",
                "to": f"{dst_db}.{dst_coll}",
            }
            if dst_existed and dst_ui is not None:
                # mongod records the dropped target's UUID under ``dropTarget``
                # in the rename oplog entry; the change-stream ``rename`` event
                # surfaces it under ``operationDescription.dropTarget`` when
                # ``showExpandedEvents`` is on.
                rename_o["dropTarget"] = bson.Binary(dst_ui.bytes, subtype=4)
            entries.append(
                {
                    "op": "c",
                    "ns": f"{src_db}.$cmd",
                    "ui": bson.Binary(ui.bytes, subtype=4),
                    "o": rename_o,
                }
            )
            self._emit_oplog(entries)
            return True, None

    def record_collmod(self, db: str, coll: str, description: dict[str, Any]) -> None:
        """Emit a ``collMod`` command oplog entry so change streams watching
        ``db`` / ``db.coll`` (with ``showExpandedEvents``) can surface a
        ``modify`` event. ``description`` carries the changed options (empty
        for a no-op ``collMod``); it becomes the event's
        ``operationDescription``. The collection's option mutation has already
        been applied by the caller via :meth:`set_collection_options`.
        """
        with self._lock:
            if self._coll_options(db, coll) is None:
                return
            ui = self._collection_uuid(db, coll)
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {"collMod": coll, **description},
                    }
                ]
            )

    def list_collections(self, db: str) -> list[str]:
        self._refresh_read_snapshot()
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, "")
            rc = c.search_near()
            if rc != wt.WT_NOTFOUND and not (rc < 0 and c.next() != 0):
                out: list[str] = []
                while True:
                    k = c.get_key()
                    if k[0] != db:
                        break
                    out.append(k[1])
                    if c.next() != 0:
                        break
            else:
                out = []
        # Synthesise ``local.oplog.rs`` for the ``local`` db whenever the
        # oplog is enabled. The collection isn't materialised in
        # ``_COLL_TABLE`` — it's a view over the oplog WT table — but
        # ``listCollections`` needs to surface it so pymongo clients can
        # discover it before querying.
        if self.enable_oplog and db == "local" and "oplog.rs" not in out:
            out.append("oplog.rs")
        return sorted(out)

    def list_databases(self) -> list[str]:
        self._refresh_read_snapshot()
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            seen: set[str] = set()
            rc = c.next()
            while rc == 0:
                k = c.get_key()
                seen.add(k[0])
                rc = c.next()
        # mongod always exposes the ``local`` database; mirror that
        # when the oplog is enabled so listDatabases includes it even
        # before any user-created collection lands in ``local``.
        if self.enable_oplog:
            seen.add("local")
        return sorted(seen)

    def create_index(
        self,
        db: str,
        coll: str,
        name: str,
        key_spec: Mapping[str, Any],
        options: Mapping[str, Any] | None = None,
    ) -> bool:
        if name == _ID_INDEX_NAME:
            return False
        # Text / hashed indexes are documented out-of-scope (CLAUDE.md
        # "Out of scope regardless: text / hashed / wildcard indexes").
        # Surface the rejection as a typed exception (caught in
        # ``commands._create_indexes``) instead of letting the geo
        # picker / encoder later fall over with an opaque internal
        # error. Mongo-node-driver's ``Find should correctly sort using
        # text search`` test expects a clean error here.
        for _field, _spec_val in key_spec.items():
            if _spec_val in ("text", "hashed"):
                raise CreateIndexUnsupported(f"{_spec_val} indexes are not supported by SecantusDB")
        options = dict(options or {})
        with self._lock:
            self._ensure_collection(db, coll)
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() == 0:
                # Index exists. Mongo rejects re-creation with conflicting
                # options (different ``unique`` / ``sparse`` / ``hidden``
                # / ``expireAfterSeconds``). Silently succeeding hides
                # a bug surface that mongo-ruby-driver's ``Collection#
                # create_indexes when index creation fails`` test pins.
                existing_raw = bytes(c.get_value())
                existing = bson.decode(existing_raw) if existing_raw else {}
                existing_opts = dict(existing.get("options") or {})
                _CONFLICTING_OPTS = (
                    "unique",
                    "sparse",
                    "hidden",
                    "expireAfterSeconds",
                    "partialFilterExpression",
                )
                for opt in _CONFLICTING_OPTS:
                    if (opt in options or opt in existing_opts) and options.get(
                        opt
                    ) != existing_opts.get(opt):
                        raise IndexOptionsConflict(
                            f"Index with name '{name}' already exists with different options"
                        )
                return False
            sparse = bool(options.get("sparse"))
            unique = bool(options.get("unique"))
            partial_filter = options.get("partialFilterExpression")
            if not isinstance(partial_filter, Mapping) or not partial_filter:
                partial_filter = None
            key_spec_dict = dict(key_spec)
            geo = _geo_type_of(key_spec_dict)
            # Geo indexes use the same entries table but write **multiple**
            # entries per doc (one per S2 cell or 2d bucket). They're inherently
            # multikey-style; uniqueness is meaningless for geo and is rejected
            # by mongod, so we mirror.
            if geo is not None:
                if unique:
                    raise IndexConflict(name, None)
                geo_field, geo_type = geo
                # mongod stamps every 2dsphere index with its index format
                # version (3 since 3.2); drivers surface it via listIndexes
                # (the PHP library's IndexInfo::is2dSphere / ['2dsphereIndexVersion']
                # assertion reads it). 2d indexes don't carry this field.
                if geo_type == _GEO_2DSPHERE:
                    options.setdefault("2dsphereIndexVersion", 3)
                # Geo indexes are always multikey from the picker's perspective
                # — each doc may produce many cell entries. Mark it so the
                # regular pickers skip the index for non-geo queries.
                options["multikey"] = True
                entries: list[tuple[bytes, bytes]] = []
                for id_k, blob in self._scan_docs(db, coll):
                    d = bson.decode(blob)
                    if partial_filter is not None and not matches(d, partial_filter):
                        continue
                    for cell_bytes in _doc_geo_cells(
                        d, geo_field, geo_type, options, index_name=name
                    ):
                        entries.append((cell_bytes, id_k))
                payload = bson.encode({"key": dict(key_spec), "options": options})
                c.reset()
                c[db, coll, name] = payload
                entry_cur = self._cursor(_IDX_ENTRIES_TABLE)
                for kb, id_k in entries:
                    entry_cur.reset()
                    entry_cur[db, coll, name, _pack_entry(kb, id_k)] = b""
            else:
                # Single doc-table walk: decode each blob once and fold all
                # three checks (uniqueness, multikey detection, entry build)
                # into one pass. Uniqueness is probed against the canonical
                # whole-doc key (``_index_key``); index entries are written
                # for every key variant (``_index_key_variants``) so per-
                # element multikey lookups land at IXSCAN.
                seen: dict[bytes, Any] | None = {} if unique else None
                multikey = False
                entries = []
                coll_opt = _parse_index_collation(options.get("collation"))
                for id_k, blob in self._scan_docs(db, coll):
                    d = bson.decode(blob)
                    if partial_filter is not None and not matches(d, partial_filter):
                        continue
                    if not multikey and _doc_makes_multikey(d, key_spec_dict):
                        multikey = True
                    if seen is not None:
                        canonical = _index_key(d, key_spec_dict, sparse=sparse, collation=coll_opt)
                        if canonical is not None:
                            if canonical in seen:
                                raise IndexConflict(name, d.get("_id"), namespace=f"{db}.{coll}")
                            seen[canonical] = d.get("_id")
                    for kb in _index_key_variants(
                        d, key_spec_dict, sparse=sparse, collation=coll_opt
                    ):
                        entries.append((kb, id_k))
                if multikey:
                    options["multikey"] = True
                payload = bson.encode({"key": dict(key_spec), "options": options})
                c.reset()
                c[db, coll, name] = payload
                entry_cur = self._cursor(_IDX_ENTRIES_TABLE)
                for kb, id_k in entries:
                    entry_cur.reset()
                    entry_cur[db, coll, name, _pack_entry(kb, id_k)] = b""
            ui = self._collection_uuid(db, coll)
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {
                            "createIndexes": coll,
                            "indexes": [{"v": 2, "key": dict(key_spec), "name": name, **options}],
                        },
                    }
                ]
            )
            return True

    def list_indexes(self, db: str, coll: str) -> list[dict[str, Any]]:
        self._refresh_read_snapshot()
        with self._lock:
            if self._coll_options(db, coll) is None:
                return []
            out: list[dict[str, Any]] = [{"v": 2, "key": {"_id": 1}, "name": _ID_INDEX_NAME}]
            for name, key_spec, opts in self._iter_indexes(db, coll):
                entry: dict[str, Any] = {"v": 2, "key": key_spec, "name": name}
                for k, v in opts.items():
                    entry[k] = v
                out.append(entry)
            out.sort(key=lambda e: e["name"])
            return out

    def _iter_indexes(
        self, db: str, coll: str
    ) -> Iterable[tuple[str, dict[str, Any], dict[str, Any]]]:
        c = self._cursor(_IDX_TABLE)
        c.set_key(db, coll, "")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll:
                return
            payload = bson.decode(bytes(c.get_value()))
            yield k[2], payload.get("key", {}), payload.get("options", {})
            if c.next() != 0:
                return

    def drop_index(self, db: str, coll: str, name: str) -> bool:
        if name == _ID_INDEX_NAME:
            return False
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() != 0:
                return False
            c.remove()
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll, name))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            ui = self._collection_uuid(db, coll)
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {"dropIndexes": coll, "index": name},
                    }
                ]
            )
            return True

    def set_index_expiry(self, db: str, coll: str, name: str, seconds: int) -> bool:
        """Update an existing index's ``expireAfterSeconds`` option in place.

        Read-modify-write of the index's stored ``{key, options}`` blob. The
        new value is honoured by ``prune_ttl``, which reads ``expireAfterSeconds``
        from the same options map. Returns ``True`` when the index existed and
        was updated, ``False`` otherwise. Backs ``collMod {index: {...},
        expireAfterSeconds: N}``.
        """
        with self._lock:
            self._refresh_read_snapshot()
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() != 0:
                return False
            payload = bson.decode(bytes(c.get_value()))
            options = payload.get("options", {})
            options["expireAfterSeconds"] = seconds
            payload["options"] = options
            c.reset()
            c[db, coll, name] = bson.encode(payload)
            return True

    def drop_all_indexes(self, db: str, coll: str) -> int:
        with self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            rows = self._collect_prefix(_IDX_TABLE, (db, coll))
            names = [k[2] for k, _ in rows]
            self._delete_keys(_IDX_TABLE, [k for k, _ in rows])
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            if names:
                ui = self._collection_uuid(db, coll)
                self._emit_oplog(
                    [
                        {
                            "op": "c",
                            "ns": f"{db}.$cmd",
                            "ui": bson.Binary(ui.bytes, subtype=4),
                            "o": {"dropIndexes": coll, "index": n},
                        }
                        for n in names
                    ]
                )
            return len(rows)

    def _all_indexes(self, db: str, coll: str) -> list[tuple[str, dict[str, Any], bool, bool]]:
        """Every non-_id_ index: (name, key_spec, sparse, unique)."""
        out: list[tuple[str, dict[str, Any], bool, bool]] = []
        for name, key_spec, opts in list(self._iter_indexes(db, coll)):
            out.append((name, key_spec, bool(opts.get("sparse")), bool(opts.get("unique"))))
        return out

    def _partial_filters(self, db: str, coll: str) -> dict[str, dict[str, Any]]:
        """Map of index name → ``partialFilterExpression`` for indexes that have one.

        Indexes without a partial filter are absent from the dict.
        """
        out: dict[str, dict[str, Any]] = {}
        for name, _key_spec, opts in self._iter_indexes(db, coll):
            pf = opts.get("partialFilterExpression")
            if isinstance(pf, Mapping) and pf:
                out[name] = dict(pf)
        return out

    @staticmethod
    def _query_implies_partial(query: Mapping[str, Any], partial: Mapping[str, Any]) -> bool:
        """True if every document matching ``query`` is guaranteed to be in
        a partial index whose filter is ``partial`` — i.e. ``query`` is at
        least as restrictive as ``partial`` on every partial-filter field.

        SOUNDNESS is the rule: using a partial index for a query that could
        match documents the index doesn't contain returns wrong results, so
        this errs to ``False`` (skip the index, full scan — correct but
        slower) for anything it can't prove implied. Supports bare-equality
        partial values and the ``$eq``/``$lt``/``$lte``/``$gt``/``$gte``
        range operators on both sides (``{a: {$lte: 1.5}}`` is implied by a
        query equality ``a: 1`` or ``a: {$lt: 1}``).
        """
        for key, pval in partial.items():
            if key not in query:
                return False
            qval = query[key]
            p_is_ops = isinstance(pval, Mapping) and pval and all(k.startswith("$") for k in pval)
            q_is_ops = isinstance(qval, Mapping) and qval and all(k.startswith("$") for k in qval)
            if p_is_ops:
                if not _clause_implies_bounds(qval, pval):
                    return False
            elif q_is_ops:
                # bare-value partial, operator-form query: only an exact
                # ``$eq`` of the same value implies it.
                if qval.get("$eq") != pval:
                    return False
            elif qval != pval:
                return False
        return True

    def _multikey_index_names(self, db: str, coll: str) -> set[str]:
        """Names of indexes flagged ``multikey`` (must fall back to scan).

        Without true multi-key indexing, an index where any doc has a
        list-valued field can't serve scalar-element matches — so the
        pickers skip these names and ``find_matching`` falls back to a
        full scan.
        """
        return {
            name for name, _key_spec, opts in self._iter_indexes(db, coll) if opts.get("multikey")
        }

    def _maybe_mark_multikey(
        self,
        db: str,
        coll: str,
        doc: Mapping[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        already_multikey: set[str],
    ) -> set[str]:
        """For each non-multikey index, flag it if ``doc`` has an array
        value on any indexed field. Returns the (possibly grown) set of
        multikey index names so the caller can avoid re-checking.
        """
        c = self._cursor(_IDX_TABLE)
        for name, key_spec, _sparse, _unique in indexes:
            if name in already_multikey:
                continue
            if not _doc_makes_multikey(doc, key_spec):
                continue
            c.reset()
            c.set_key(db, coll, name)
            if c.search() != 0:
                continue
            payload = bson.decode(bytes(c.get_value()))
            opts = dict(payload.get("options") or {})
            if opts.get("multikey"):
                already_multikey.add(name)
                continue
            opts["multikey"] = True
            payload["options"] = opts
            c.reset()
            c[db, coll, name] = bson.encode(payload)
            already_multikey.add(name)
        return already_multikey

    def _write_index_entries(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        partials: dict[str, dict[str, Any]] | None = None,
        *,
        id_key_override: bytes | None = None,
    ) -> None:
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
        # Timeseries doc-table keys carry a uniqueness suffix; entries must
        # point at the row's ACTUAL key or index lookups would miss it.
        id_k = id_key_override if id_key_override is not None else _id_key(doc["_id"])
        if partials is None:
            partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        for name, key_spec, sparse, _unique in indexes:
            pf = partials.get(name)
            if pf is not None and not matches(doc, pf):
                continue
            geo = _geo_type_of(key_spec)
            if geo is not None:
                geo_field, geo_type = geo
                opts = index_options.get(name, {})
                for cell_bytes in _doc_geo_cells(doc, geo_field, geo_type, opts, index_name=name):
                    c.reset()
                    c[db, coll, name, _pack_entry(cell_bytes, id_k)] = b""
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            for kb in _index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt):
                c.reset()
                c[db, coll, name, _pack_entry(kb, id_k)] = b""

    def _delete_index_entries(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        partials: dict[str, dict[str, Any]] | None = None,
        *,
        id_key_override: bytes | None = None,
    ) -> None:
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
        # Timeseries doc-table keys carry a uniqueness suffix; entries must
        # point at the row's ACTUAL key or index lookups would miss it.
        id_k = id_key_override if id_key_override is not None else _id_key(doc["_id"])
        if partials is None:
            partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        for name, key_spec, sparse, _unique in indexes:
            pf = partials.get(name)
            if pf is not None and not matches(doc, pf):
                continue
            geo = _geo_type_of(key_spec)
            if geo is not None:
                geo_field, geo_type = geo
                opts = index_options.get(name, {})
                # On the delete path, swallow GeoExtractError. A doc that
                # was inserted before geo validation became strict might
                # have bad geometry; we still need to allow it to be
                # deleted. The index may end up with stale entries we
                # can't match, but the next compact / drop_index cleans
                # those up. Insert/update remain strict.
                try:
                    cells = _doc_geo_cells(doc, geo_field, geo_type, opts, index_name=name)
                except GeoExtractError:
                    continue
                for cell_bytes in cells:
                    c.reset()
                    c.set_key(db, coll, name, _pack_entry(cell_bytes, id_k))
                    if c.search() == 0:
                        c.remove()
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            for kb in _index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt):
                c.reset()
                c.set_key(db, coll, name, _pack_entry(kb, id_k))
                if c.search() == 0:
                    c.remove()

    def _validate_geo_indexes(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        partials: dict[str, dict[str, Any]] | None = None,
    ) -> None:
        """Pre-flight every geo index for ``doc``: raise on bad geometry.

        Used by the insert / update paths to reject docs *before* writing
        them, so a single bad geo coordinate doesn't leave a half-indexed
        document behind. The work duplicates ``_write_index_entries``'s
        cell computation but is cheap (one Shapely parse + bounds check
        per indexed field).
        """
        if not indexes:
            return
        if partials is None:
            partials = self._partial_filters(db, coll)
        options_map = self._index_options_map(db, coll)
        for name, key_spec, _sparse, _unique in indexes:
            geo = _geo_type_of(key_spec)
            if geo is None:
                continue
            pf = partials.get(name)
            if pf is not None and not matches(doc, pf):
                continue
            geo_field, geo_type = geo
            opts = options_map.get(name, {})
            # Compute & discard — `_doc_geo_cells` raises GeoExtractError
            # on bad shape or out-of-bounds coords; that's the signal we
            # want to bubble up.
            _doc_geo_cells(doc, geo_field, geo_type, opts, index_name=name)

    def _index_options_map(self, db: str, coll: str) -> dict[str, dict[str, Any]]:
        """Map of index name → its full options blob.

        Used by the geo write/delete paths: 2d indexes carry per-index
        ``bits`` / ``min`` / ``max`` settings that affect the cell encoder,
        so we need the option blob to compute the right bucket. Cached
        per call (the caller handles per-doc loops).
        """
        return {name: dict(opts) for name, _key_spec, opts in self._iter_indexes(db, coll)}

    def _unique_conflict(
        self,
        db: str,
        coll: str,
        candidate_doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        *,
        exclude_id_key: bytes | None,
        partials: dict[str, dict[str, Any]] | None = None,
    ) -> tuple[str, dict[str, Any], dict[str, Any]] | None:
        # Returns ``(index_name, key_pattern, key_value)`` so callers
        # can build a mongod-shaped dup-key error response with the
        # ``keyPattern`` + ``keyValue`` fields drivers' errorResponse
        # tests assert on. ``None`` when no conflict.
        if not indexes:
            return None
        c = self._cursor(_IDX_ENTRIES_TABLE)
        if partials is None:
            partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        for name, key_spec, sparse, unique in indexes:
            if not unique:
                continue
            pf = partials.get(name)
            if pf is not None and not matches(candidate_doc, pf):
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            kb = _index_key(candidate_doc, key_spec, sparse=sparse, collation=coll_opt)
            if kb is None:
                continue
            esc_kb = _escape_kb(kb)
            seed = esc_kb + _ENTRY_SEP
            c.reset()
            c.set_key(db, coll, name, seed)
            rc = c.search_near()
            if rc == wt.WT_NOTFOUND:
                continue
            if rc < 0 and c.next() != 0:
                continue
            while True:
                k = c.get_key()
                if (k[0], k[1], k[2]) != (db, coll, name):
                    break
                packed = bytes(k[3])
                row_esc, row_id = _unpack_entry(packed)
                if row_esc != esc_kb:
                    break
                if exclude_id_key is None or row_id != exclude_id_key:
                    key_value = {
                        field: get_path(candidate_doc, field, default=None) for field in key_spec
                    }
                    return name, dict(key_spec), key_value
                if c.next() != 0:
                    break
        return None

    def _scan_index_for_id_keys(
        self, db: str, coll: str, name: str, kb: bytes, *, prefix: bool = False
    ) -> list[bytes]:
        """Walk the index entries for ``name`` matching ``kb``.

        With ``prefix=False`` (default), only rows whose ``escaped_kb`` is
        exactly equal to ``escape(kb)`` are returned — equality lookup.
        With ``prefix=True``, any row whose ``escaped_kb`` starts with
        ``escape(kb)`` is returned — compound-prefix lookup.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_kb = _escape_kb(kb)
        seed = esc_kb if prefix else esc_kb + _ENTRY_SEP
        c.set_key(db, coll, name, seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if prefix:
                if not row_esc.startswith(esc_kb):
                    break
            elif row_esc != esc_kb:
                break
            out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _all_id_keys_for_index(self, db: str, coll: str, name: str) -> list[bytes]:
        """Every id_key with an entry in index ``name`` — a full index scan.

        Serves ``{field: {$exists: true}}`` via a sparse index: a sparse
        index's entries table holds an entry for exactly the docs where the
        indexed field is present (missing-field docs are omitted; present-
        but-null keeps an entry), so the complete set of entries *is* the
        ``$exists: true`` match set. id_keys can repeat for multikey arrays
        (one entry per element); the caller's ``_docs_by_id_keys`` dedups.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        c.set_key(db, coll, name, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            _row_esc, row_id = _unpack_entry(bytes(k[3]))
            out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _docs_by_id_keys(self, db: str, coll: str, id_keys: list[bytes]) -> list[dict[str, Any]]:
        if not id_keys:
            return []
        c = self._cursor(_DOC_TABLE)
        # Two-stage: WT cursor walk first (raw bytes), then ``bson.decode``
        # outside that loop. The cursor work is what needs lock scope;
        # decode is pure CPU and benefits from running unsynchronised.
        # Multikey indexes write per-element entries, so the same doc's
        # id_key can appear more than once for queries that match
        # multiple elements. Dedupe while preserving order.
        raw: list[bytes] = []
        for id_k in dict.fromkeys(id_keys):
            c.reset()
            c.set_key(db, coll, id_k)
            if c.search() == 0:
                raw.append(bytes(c.get_value()))
        return [bson.decode(b) for b in raw]

    _RANGE_OPS: tuple[str, ...] = ("$eq", "$gt", "$gte", "$lt", "$lte", "$in")
    _GEO_OPS: tuple[str, ...] = ("$geoWithin", "$geoIntersects", "$near", "$nearSphere")

    def _try_geo_index_id_keys(
        self, db: str, coll: str, filter: dict[str, Any]
    ) -> list[bytes] | None:
        """If ``filter`` contains a geo operator on a geo-indexed field,
        scan that index's covering cells and return candidate id_keys.

        Returns ``None`` when no geo operator is present or no matching
        geo index exists — caller falls through to regular pickers and
        eventually a full scan. Returns a list (possibly empty) when a
        geo index covers the query — caller short-circuits the regular
        pickers.

        The cell scan over-collects (cell-covering is a superset of the
        true intersection); the caller's ``matches()`` step verifies via
        :func:`secantus.geo.geo_within` / ``geo_intersects`` and removes
        false positives.
        """
        # Find a single field with a geo operator on it.
        geo_field: str | None = None
        geo_op: str | None = None
        geo_arg: Any = None
        geo_siblings: Mapping[str, Any] | None = None
        for field, value in filter.items():
            if not isinstance(value, dict):
                continue
            for op in self._GEO_OPS:
                if op in value:
                    geo_field = field
                    geo_op = op
                    geo_arg = value[op]
                    # Capture the whole condition so `$near` /
                    # `$nearSphere` legacy 2d form (sibling
                    # `$maxDistance` / `$minDistance`) can scope the
                    # scan; without this the picker can't know the
                    # distance bound and falls back to full-scan.
                    geo_siblings = value
                    break
            if geo_field is not None:
                break
        if geo_field is None:
            return None
        # Locate a geo index on that field.
        chosen_name: str | None = None
        chosen_type: str | None = None
        chosen_opts: dict[str, Any] = {}
        for name, key_spec, opts in self._iter_indexes(db, coll):
            geo = _geo_type_of(key_spec)
            if geo is None:
                continue
            if geo[0] == geo_field:
                chosen_name = name
                chosen_type = geo[1]
                chosen_opts = dict(opts)
                break
        if chosen_name is None or chosen_type is None:
            return None
        # Build the query geometry from the operator arg.
        cells = self._geo_query_cells(
            geo_op, geo_arg, chosen_type, chosen_opts, siblings=geo_siblings
        )
        if cells is None:
            # Couldn't compute a covering — defer to full scan.
            return None
        return self._collect_geo_candidates(db, coll, chosen_name, cells)

    def _geo_query_cells(
        self,
        op: str,
        arg: Any,
        geo_type: str,
        options: Mapping[str, Any],
        *,
        siblings: Mapping[str, Any] | None = None,
    ) -> list[tuple[bytes, bytes]] | None:
        """Byte ranges covering the query geometry, one per covering cell.

        Both 2dsphere and 2d return ``list[tuple[bytes, bytes]]`` — for
        2dsphere each entry is the (range_min, range_max) byte pair of
        an S2 covering cell expanded to its leaf descendants; for 2d
        it's the single (lo, hi) bbox range from `planar_2d_covering`.
        Callers use :meth:`_scan_geo_range` for both.
        """
        from secantus.geo import GeoError

        try:
            if op in ("$geoWithin", "$geoIntersects"):
                if not isinstance(arg, Mapping):
                    return None
                geom, _ = parse_query_geometry(arg)
            elif op in ("$near", "$nearSphere"):
                # `$near` without a max distance: caller falls through to
                # full scan (signal None). With a max, expand into a cap
                # (2dsphere) or planar disk (2d).
                (
                    center,
                    max_d,
                    _min_d,
                    spherical,
                    legacy_form,
                ) = self._near_query_geom(
                    arg,
                    default_spherical=(op == "$nearSphere"),
                    siblings=siblings,
                )
                if max_d is None:
                    return None
                # Unit normalisation: legacy+spherical gives max in
                # radians-on-unit-sphere; legacy+planar gives max in
                # input units; GeoJSON gives max in meters. Index
                # picker for 2dsphere wants radians (so / EARTH_R);
                # picker for 2d wants planar (so leave alone for
                # legacy+planar; convert rad→degrees for
                # legacy+spherical via *180/π).
                import math as _math

                from shapely.geometry import Point as _Point

                from secantus.geo import EARTH_RADIUS_METERS, _SphericalCircle

                if geo_type == _GEO_2DSPHERE:
                    # legacy+spherical → max already radians; otherwise
                    # → meters → divide by Earth radius for radians.
                    radius_rad = (
                        max_d if (legacy_form and spherical) else max_d / EARTH_RADIUS_METERS
                    )
                    geom = _SphericalCircle(center[0], center[1], radius_rad)
                else:  # 2d planar — circular disk
                    # legacy+spherical → radians-on-unit-sphere → degrees
                    # in planar input space (the conventional geographic
                    # mapping that matches mongod's behaviour against a
                    # 2d index). Otherwise the bound is already in input
                    # units.
                    planar_radius = (
                        max_d * 180.0 / _math.pi if (legacy_form and spherical) else max_d
                    )
                    geom = _Point(*center).buffer(planar_radius, quad_segs=16)
            else:
                return None
        except GeoError:
            return None
        if geo_type == _GEO_2DSPHERE:
            # Each cell becomes a degenerate (cell, cell) range so the
            # storage scanner does an exact point-lookup. Treating
            # 2dsphere uniformly as a list-of-ranges keeps the storage
            # path single-shaped.
            return [(encode_cell(c), encode_cell(c)) for c in s2_query_covering(geom)]
        # 2d: shape must be planar; convert to a list of tight Z-order
        # ranges via quadtree decomposition. For small bboxes this is
        # one range, same as the single-range path; for wider bboxes
        # the decomposition tightens the scan vs the old single coarse
        # range.
        from shapely.geometry.base import BaseGeometry as _BG

        if not isinstance(geom, _BG):
            return None
        return [
            (encode_cell(lo), encode_cell(hi))
            for lo, hi in planar_2d_covering_ranges(geom, options)
        ]

    def _near_query_geom(
        self,
        arg: Any,
        *,
        default_spherical: bool = False,
        siblings: Mapping[str, Any] | None = None,
    ) -> tuple[tuple[float, float], float | None, float | None, bool, bool]:
        """Reuse :mod:`secantus.query`'s ``$near`` parser for the picker.

        Returns ``(center, max_d, min_d, spherical, legacy_form)`` —
        legacy_form lets the picker pick the right unit conversion
        (radians-on-unit-sphere vs meters vs input units) when building
        the index-side geometry.

        ``default_spherical`` must match the operator: ``$near`` →
        False, ``$nearSphere`` → True. Without this, a legacy-form
        ``$nearSphere`` would be misread as planar and the picker
        would build the wrong geometry.

        Routing through `_parse_near_spec` keeps the spec semantics in
        one place — the operator handler and the picker agree on what
        a ``$near`` arg means. ``siblings`` carries the parent
        condition dict so the legacy 2d shape ``{geo: {$near: [x, y],
        $maxDistance: r}}`` works.
        """
        from secantus.query import _parse_near_spec  # type: ignore[attr-defined]

        return _parse_near_spec(arg, default_spherical=default_spherical, siblings=siblings)

    def _collect_geo_candidates(
        self,
        db: str,
        coll: str,
        index_name: str,
        cells: list[tuple[bytes, bytes]],
    ) -> list[bytes]:
        """Walk index entries in each (lo, hi) range; return deduplicated id_keys.

        A doc with N covering cells produces N index entries; we collect
        just one ``_id`` per doc. The post-fetch verifier (in
        ``find_matching``'s ``matches()`` step) discards docs whose
        actual geometry doesn't match the query.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        seen: set[bytes] = set()
        out: list[bytes] = []
        for lo_bytes, hi_bytes in cells:
            self._scan_geo_range(c, db, coll, index_name, lo_bytes, hi_bytes, seen, out)
        return out

    def _scan_geo_range(
        self,
        c: Any,
        db: str,
        coll: str,
        name: str,
        lo_bytes: bytes,
        hi_bytes: bytes,
        seen: set[bytes],
        out: list[bytes],
    ) -> None:
        """Walk every index entry whose escaped cell-id is in [lo_bytes, hi_bytes].

        Lex byte order over `_escape_kb`-escaped fixed-width cell IDs is
        the same as numeric cell-id order, so a forward WT cursor walk
        between the two escaped boundary keys visits every entry inside
        the range exactly once. Cell IDs are packed as fixed 8-byte
        big-endian, so escaping never changes their relative order.
        """
        lo_prefix = _escape_kb(lo_bytes)
        hi_prefix = _escape_kb(hi_bytes)
        c.reset()
        c.set_key(db, coll, name, lo_prefix)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll or k[2] != name:
                return
            packed = bytes(k[3])
            sep_pos = packed.find(_ENTRY_SEP)
            if sep_pos < 0:
                if c.next() != 0:
                    return
                continue
            kb_part = packed[:sep_pos]
            if kb_part > hi_prefix:
                return
            id_key = packed[sep_pos + len(_ENTRY_SEP) :]
            if id_key not in seen:
                seen.add(id_key)
                out.append(id_key)
            if c.next() != 0:
                return

    def _try_index_lookup(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        collation: Any = None,
    ) -> list[dict[str, Any]] | None:
        id_keys = self._try_index_id_keys(db, coll, filter, collation=collation)
        if id_keys is None:
            return None
        return self._docs_by_id_keys(db, coll, id_keys)

    def _single_field_partial_residual_match(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        collation: Any = None,
    ) -> tuple[str, Any, tuple[str, int, bool]] | None:
        """For a *multi-field* filter, find a single-field index whose leading
        field serves one clause while every **other** filter field is absorbed
        by the index's (implied) partial filter.

        e.g. ``find({x: {$gt: 1}, a: 1})`` against an index on ``x`` partial on
        ``{a: {$lte: 1.5}}``: ``x``'s range rides the index, the ``a: 1`` clause
        is partial-implied (so the index's very existence guarantees it) and is
        rechecked by the exact ``matches()`` pass in ``find_matching``. Returns
        ``(field, value, idx_match)`` or ``None``.

        Conservative by design: only *partial* indexes get this treatment, and
        only when the residual fields are exactly partial-filter fields — a
        non-partial residual keeps the query on COLLSCAN, mirroring the
        bare-equality path's ``eff_fields - set(pf)`` philosophy. Shared by the
        lookup (``_try_index_id_keys``) and explain (``_pick_index_for_filter``)
        dispatchers so they never diverge.
        """
        partials = self._partial_filters(db, coll)
        for field, value in filter.items():
            if isinstance(value, dict) and (
                not value or not all(op in self._RANGE_OPS for op in value)
            ):
                continue
            idx_match = self._find_leading_field_index(db, coll, field, filter, collation=collation)
            if idx_match is None:
                continue
            name = idx_match[0]
            pf = partials.get(name)
            if pf is None:
                continue
            if not (set(filter) - {field}).issubset(set(pf)):
                continue
            return field, value, idx_match
        return None

    def _try_index_id_keys(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        collation: Any = None,
    ) -> list[bytes] | None:
        """Same dispatch as ``_try_index_lookup`` but returns id_keys instead
        of materialised docs. Used by the write paths (update / delete) so
        only matching docs pay ``bson.decode``.

        ``collation`` propagates from the query. When set, only indexes
        whose stored ``collation`` option matches are considered;
        non-matching indexes are skipped so the caller falls back to
        COLLSCAN (the safe semantics). Single-field equality / range /
        ``$in`` and compound bare-eq / compound prefix + trailing
        operator all thread collation through. ``numericOrdering``
        collations never match any index (parse to None at the gate)
        and fall through to COLLSCAN.
        """
        if not filter:
            return None
        if any(f.startswith("$") for f in filter):
            return None
        # Fast path: equality on _id alone is a direct primary-key point
        # lookup on the documents table (keyed by encode_value(_id)), not a
        # COLLSCAN — the _id_ index is virtual (no entries table), so the
        # generic pickers below never match it. Timeseries collections are
        # excluded: their doc keys carry a uniqueness suffix (duplicate
        # _ids are legal there), so the reconstructed unsuffixed key would
        # never match a row.
        if len(filter) == 1 and "_id" in filter and not self._is_timeseries(db, coll):
            id_keys = _id_point_lookup_keys(filter["_id"])
            if id_keys is not None:
                return id_keys
        # Geo dispatch first — a $geoWithin / $geoIntersects / $near clause
        # on a field with a 2dsphere or 2d index uses the cell-covering
        # path. The picker returns None if no geo index covers the query,
        # and we fall through to the regular pickers below.
        geo_ids = self._try_geo_index_id_keys(db, coll, filter)
        if geo_ids is not None:
            return geo_ids
        # Bare-equality filters of any size can use a compound index whose
        # leading fields cover the filter set.
        if all(not isinstance(v, dict) for v in filter.values()):
            result = self._try_compound_eq_id_keys(db, coll, filter, collation=collation)
            if result is not None:
                return result
        # Compound prefix + trailing operator field (eq fields then range/in).
        if len(filter) >= 2:
            result = self._try_compound_range_id_keys(db, coll, filter, collation=collation)
            if result is not None:
                return result
        if len(filter) == 1:
            field, value = next(iter(filter.items()))
            # {field: {$exists: true}} rides a sparse single-field index on
            # ``field`` — every sparse entry is a doc where the field is
            # present, exactly the $exists:true match set. No value bound:
            # the whole index scans.
            if isinstance(value, dict) and len(value) == 1 and value.get("$exists"):
                name = self._sparse_index_for_exists(db, coll, field)
                if name is None:
                    return None
                return self._all_id_keys_for_index(db, coll, name)
            idx_match = self._find_leading_field_index(db, coll, field, filter, collation=collation)
            if idx_match is None:
                return None
            return self._lookup_id_keys_via_leading_field(
                db, coll, idx_match, value, collation=collation
            )
        # Multi-field filter: a single-field index can still serve it when every
        # other filter field is absorbed by the index's (implied) partial filter.
        match = self._single_field_partial_residual_match(db, coll, filter, collation=collation)
        if match is None:
            return None
        _field, value, idx_match = match
        return self._lookup_id_keys_via_leading_field(
            db, coll, idx_match, value, collation=collation
        )

    def _candidates_iter(
        self, db: str, coll: str, filter: dict[str, Any] | None
    ) -> list[tuple[bytes, bytes]]:
        """Return (id_key, blob) pairs that the write paths should consider.
        If an index covers the filter, only the indexed candidates are
        fetched; otherwise the full doc table is scanned. Either way,
        BSON decode is left to the caller so non-matching docs don't pay
        for it. Caller still applies ``matches()`` to the decoded doc —
        index lookups can produce false-positive candidates for partial
        scans (multikey, prefix overlap, etc).
        """
        if filter:
            id_keys = self._try_index_id_keys(db, coll, filter)
            if id_keys is not None:
                c = self._cursor(_DOC_TABLE)
                out: list[tuple[bytes, bytes]] = []
                # Same dedup contract as ``_docs_by_id_keys``: multikey
                # indexes can yield duplicate id_keys for one doc.
                for id_k in dict.fromkeys(id_keys):
                    c.reset()
                    c.set_key(db, coll, id_k)
                    if c.search() == 0:
                        out.append((id_k, bytes(c.get_value())))
                return out
        return list(self._scan_docs(db, coll))

    def _find_leading_field_index(
        self,
        db: str,
        coll: str,
        field: str,
        query: Mapping[str, Any] | None = None,
        *,
        collation: Any = None,
    ) -> tuple[str, int, bool] | None:
        """Best index whose leading field is ``field``.

        Returns ``(name, direction, is_compound)``. Single-field indexes
        win over compound (tighter scan, no separator math). All fields
        must be ASC or DESC. Partial indexes are skipped unless ``query``
        implies their ``partialFilterExpression``.

        Multikey indexes are not skipped — ``_index_key_variants`` writes
        per-element entries, so equality / range / ``$in`` lookups on
        the leading field hit at least all true matches. The geo
        ``2dsphere`` / ``2d`` indexes have non-numeric direction values
        and are excluded by the ASC/DESC check below.

        ``collation``: when set, an index is only considered if its
        stored ``collation`` option produces the same :class:`Collation`
        as the query's (or both are None). Mismatched indexes are
        skipped — the caller falls back to COLLSCAN, which uses
        ``matches()`` with the query's collation. Matches mongod's
        per-index collation semantics.
        """
        partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        query = query or {}
        compound_fallback: tuple[str, int, bool] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            pf = partials.get(name)
            if pf is not None and not self._query_implies_partial(query, pf):
                continue
            idx_fields = list(key_spec)
            if not idx_fields or idx_fields[0] != field:
                continue
            # Geo / hashed / text indexes carry string direction values
            # ("2dsphere", "2d", "hashed", "text"); the bare equality
            # picker can't drive them. Real numeric direction values are
            # 1 / -1.
            if any(key_spec[f] not in (1, -1) for f in idx_fields):
                continue
            # Collation gate: the index's stored collation must equal
            # the query's effective collation (both None counts as a
            # match). Indexes with a collation that doesn't support
            # byte encoding (numericOrdering) parse to None here, so
            # they're treated as "no collation" — correct for queries
            # that also don't carry collation, wrong for queries that
            # do. Conservative: gate by None-vs-None or exact match.
            idx_coll = _parse_index_collation(index_options.get(name, {}).get("collation"))
            if idx_coll != collation:
                continue
            d = int(key_spec[field])
            if len(idx_fields) == 1:
                return name, d, False
            if compound_fallback is None:
                compound_fallback = (name, d, True)
        return compound_fallback

    def _sparse_index_for_exists(self, db: str, coll: str, field: str) -> str | None:
        """Name of a sparse single-field index on ``field`` that can serve
        ``{field: {$exists: true}}`` at IXSCAN, or ``None``.

        Only a **sparse** index qualifies: it omits docs missing the field,
        so a full scan of its entries yields exactly the ``$exists: true``
        matches. A non-sparse index has an entry per doc (missing fields
        included), so it can't distinguish presence. Restricted to
        single-field indexes — a compound sparse index in mongod drops a
        doc only when *every* indexed field is missing, so its entries
        don't line up with ``{leadingField: {$exists: true}}``. Collation-
        independent: presence doesn't depend on string normalisation, so an
        index of any collation serves the query (the post-scan ``matches()``
        is the final arbiter regardless).
        """
        for name, key_spec, sparse, _unique in self._all_indexes(db, coll):
            if not sparse:
                continue
            idx_fields = list(key_spec)
            if len(idx_fields) != 1 or idx_fields[0] != field:
                continue
            if key_spec[field] not in (1, -1):
                continue
            return name
        return None

    def _lookup_id_keys_via_leading_field(
        self,
        db: str,
        coll: str,
        idx_match: tuple[str, int, bool],
        value: Any,
        *,
        collation: Any = None,
    ) -> list[bytes] | None:
        name, direction, is_compound = idx_match
        if not isinstance(value, dict):
            return self._eq_id_keys_via_leading(
                db, coll, name, direction, is_compound, value, collation=collation
            )
        if not value or not all(k.startswith("$") for k in value):
            return None
        if not all(op in self._RANGE_OPS for op in value):
            return None
        if "$in" in value:
            if len(value) != 1 or not isinstance(value["$in"], list):
                return None
            seen: set[bytes] = set()
            id_keys: list[bytes] = []
            for v in value["$in"]:
                if isinstance(v, dict):
                    return None
                for id_k in self._eq_id_keys_via_leading(
                    db, coll, name, direction, is_compound, v, collation=collation
                ):
                    if id_k not in seen:
                        seen.add(id_k)
                        id_keys.append(id_k)
            return id_keys
        lower: bytes | None = None
        lower_inclusive = True
        upper: bytes | None = None
        upper_inclusive = True
        for op, bound in value.items():
            if isinstance(bound, dict):
                return None
            if op == "$eq":
                return self._eq_id_keys_via_leading(
                    db, coll, name, direction, is_compound, bound, collation=collation
                )
            kb = encode_value_directed(bound, direction, collation=collation)
            # Operator semantics flip when stored bytes are inverted: in a
            # DESC index, "x > 5" means we want stored bytes < enc_desc(5).
            effective_op = op
            if direction == -1:
                effective_op = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte"}[op]
            if effective_op == "$gt":
                lower, lower_inclusive = kb, False
            elif effective_op == "$gte":
                lower, lower_inclusive = kb, True
            elif effective_op == "$lt":
                upper, upper_inclusive = kb, False
            elif effective_op == "$lte":
                upper, upper_inclusive = kb, True
        if is_compound:
            return self._range_scan_index_leading(
                db, coll, name, lower, lower_inclusive, upper, upper_inclusive
            )
        return self._range_scan_index(
            db, coll, name, lower, lower_inclusive, upper, upper_inclusive
        )

    def _eq_id_keys_via_leading(
        self,
        db: str,
        coll: str,
        name: str,
        direction: int,
        is_compound: bool,
        value: Any,
        *,
        collation: Any = None,
    ) -> list[bytes]:
        kb = encode_value_directed(value, direction, collation=collation)
        if is_compound:
            return self._scan_index_for_id_keys(db, coll, name, kb + COMPOUND_SEP, prefix=True)
        return self._scan_index_for_id_keys(db, coll, name, kb)

    def _pick_compound_eq_index(
        self, db: str, coll: str, filter: dict[str, Any], *, collation: Any = None
    ) -> tuple[str, dict[str, Any]] | None:
        """Find the index that ``_try_compound_eq_id_keys`` would walk for ``filter``.

        Returns ``(name, key_spec)`` of the chosen index, or ``None`` if no
        index covers the filter as a leading prefix. Pure picker — does
        not scan. Multikey indexes are eligible (per-element entries
        cover equality lookups); the ASC/DESC direction check excludes
        geo indexes.

        ``collation``: an index is only considered if its stored
        ``collation`` parses to the same :class:`Collation` as the
        query's (or both None). Same exact-match gate as
        ``_find_leading_field_index``. Indexes whose stored collation
        is ``numericOrdering`` parse to None here, so they look like
        no-collation indexes — correct for no-collation queries,
        wrong for numericOrdering queries; the latter fall through to
        COLLSCAN regardless.
        """
        filter_fields = set(filter)
        partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        best: tuple[str, dict[str, Any]] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            pf = partials.get(name)
            if pf is not None:
                if not self._query_implies_partial(filter, pf):
                    continue
                # Partial-filter clauses are guaranteed by the index itself,
                # so they don't have to appear in the index key.
                eff_fields = filter_fields - set(pf)
            else:
                eff_fields = filter_fields
            idx_fields = list(key_spec.keys())
            # Geo / hashed / text indexes (string direction values) can't
            # serve a bare-equality compound lookup.
            if any(key_spec[f] not in (1, -1) for f in idx_fields):
                continue
            idx_coll = _parse_index_collation(index_options.get(name, {}).get("collation"))
            if idx_coll != collation:
                continue
            if len(idx_fields) < len(eff_fields):
                continue
            if set(idx_fields[: len(eff_fields)]) != eff_fields:
                continue
            if best is None or (len(list(best[1])) > len(idx_fields)):
                best = (name, dict(key_spec))
            if len(idx_fields) == len(eff_fields):
                break
        return best

    def _try_compound_eq_id_keys(
        self, db: str, coll: str, filter: dict[str, Any], *, collation: Any = None
    ) -> list[bytes] | None:
        """Bare-equality filter against a compound (or single-field) index prefix.

        Picks an index whose leading fields (set-wise) match the filter's
        fields, and runs an equality (full-cover) or prefix
        (strict-leading-prefix) scan against it. Per-field index direction
        is honoured by encoding each value with ``encode_value_directed``.

        ``collation`` propagates from the query: only collation-matching
        indexes are picked, and the lookup bytes are built under the same
        collation so they hit the same row as the index-write side.
        """
        picked = self._pick_compound_eq_index(db, coll, filter, collation=collation)
        if picked is None:
            return None
        name, key_spec = picked
        idx_fields = list(key_spec)
        # Build kb from the filter fields that are in the index (partial-filter
        # clauses live outside the key and are guaranteed by index population).
        prefix_fields = [f for f in idx_fields if f in filter]
        parts = [
            encode_value_directed(filter[f], int(key_spec[f]), collation=collation)
            for f in prefix_fields
        ]
        kb = COMPOUND_SEP.join(parts) if len(parts) > 1 else parts[0]
        if len(prefix_fields) == len(idx_fields):
            return self._scan_index_for_id_keys(db, coll, name, kb)
        kb = kb + COMPOUND_SEP
        return self._scan_index_for_id_keys(db, coll, name, kb, prefix=True)

    def _partition_compound_range_filter(
        self, filter: dict[str, Any]
    ) -> tuple[dict[str, Any], str, dict[str, Any]] | None:
        """Split a filter into ``(eq_fields, operator_field, operator_ops)``.

        Returns ``None`` if the filter doesn't fit the compound-range
        shape (any number of bare-equality fields plus exactly one
        operator-form field whose ops are all in ``_RANGE_OPS``).
        """
        eq_fields: dict[str, Any] = {}
        operator_field: str | None = None
        operator_ops: dict[str, Any] | None = None
        for f, v in filter.items():
            if isinstance(v, dict):
                if not v or not all(k.startswith("$") for k in v):
                    return None
                if not all(op in self._RANGE_OPS for op in v):
                    return None
                if operator_field is not None:
                    return None
                operator_field = f
                operator_ops = v
            else:
                eq_fields[f] = v
        if operator_field is None or not eq_fields:
            return None
        if operator_field in eq_fields:
            return None
        return eq_fields, operator_field, operator_ops or {}

    def _pick_compound_range_index(
        self, db: str, coll: str, filter: dict[str, Any], *, collation: Any = None
    ) -> tuple[str, dict[str, Any]] | None:
        """Find the index that ``_try_compound_range_id_keys`` would walk.

        ``collation``: an index is only considered if its stored
        collation parses to the same :class:`Collation` as the query's
        (or both None). Same exact-match gate as
        ``_pick_compound_eq_index`` and ``_find_leading_field_index``.
        """
        parts = self._partition_compound_range_filter(filter)
        if parts is None:
            return None
        eq_fields, operator_field, _operator_ops = parts
        eq_set = set(eq_fields)
        target_eq_count = len(eq_set)
        partials = self._partial_filters(db, coll)
        index_options = self._index_options_map(db, coll)
        best: tuple[str, dict[str, Any]] | None = None
        for name, key_spec, _sparse, _unique in self._all_indexes(db, coll):
            pf = partials.get(name)
            if pf is not None and not self._query_implies_partial(filter, pf):
                continue
            idx_fields = list(key_spec.keys())
            # Geo / hashed / text indexes (string direction values) can't
            # serve a compound prefix + trailing-operator lookup.
            if any(key_spec[f] not in (1, -1) for f in idx_fields):
                continue
            idx_coll = _parse_index_collation(index_options.get(name, {}).get("collation"))
            if idx_coll != collation:
                continue
            if len(idx_fields) <= target_eq_count:
                continue
            if set(idx_fields[:target_eq_count]) != eq_set:
                continue
            if idx_fields[target_eq_count] != operator_field:
                continue
            if best is None or len(list(best[1])) > len(idx_fields):
                best = (name, dict(key_spec))
            if len(idx_fields) == target_eq_count + 1:
                break
        return best

    def _try_compound_range_id_keys(
        self, db: str, coll: str, filter: dict[str, Any], *, collation: Any = None
    ) -> list[bytes] | None:
        """Compound-prefix lookup with a trailing operator field.

        Filters of the form ``{a: 5, b: 10, c: {$gt: 20}}`` (any number of
        leading bare-equality fields followed by exactly one operator-form
        field) walk the compound index by pinning the prefix from the
        equalities and applying the operator's bounds to the next field.

        ``collation`` propagates from the query: only collation-matching
        indexes are picked, and every encoded value (prefix equalities
        and trailing-operator bound) is built under the same collation.
        """
        parts = self._partition_compound_range_filter(filter)
        if parts is None:
            return None
        eq_fields, operator_field, operator_ops = parts
        picked = self._pick_compound_range_index(db, coll, filter, collation=collation)
        if picked is None:
            return None
        name, key_spec = picked
        idx_fields = list(key_spec)
        target_eq_count = len(eq_fields)
        eq_field_names = idx_fields[:target_eq_count]
        op_dir = int(key_spec[operator_field])
        eq_parts = [
            encode_value_directed(eq_fields[f], int(key_spec[f]), collation=collation)
            for f in eq_field_names
        ]
        prefix_kb = COMPOUND_SEP.join(eq_parts) if len(eq_parts) > 1 else eq_parts[0]
        prefix_with_sep = prefix_kb + COMPOUND_SEP
        if "$in" in operator_ops:
            if len(operator_ops) != 1 or not isinstance(operator_ops["$in"], list):
                return None
            seen: set[bytes] = set()
            id_keys: list[bytes] = []
            for v in operator_ops["$in"]:
                if isinstance(v, dict):
                    return None
                kb = prefix_with_sep + encode_value_directed(v, op_dir, collation=collation)
                use_prefix = len(idx_fields) > target_eq_count + 1
                inner_kb = kb + COMPOUND_SEP if use_prefix else kb
                for id_k in self._scan_index_for_id_keys(
                    db, coll, name, inner_kb, prefix=use_prefix
                ):
                    if id_k not in seen:
                        seen.add(id_k)
                        id_keys.append(id_k)
            return id_keys
        if "$eq" in operator_ops:
            if len(operator_ops) != 1:
                return None
            kb = prefix_with_sep + encode_value_directed(
                operator_ops["$eq"], op_dir, collation=collation
            )
            use_prefix = len(idx_fields) > target_eq_count + 1
            inner_kb = kb + COMPOUND_SEP if use_prefix else kb
            return self._scan_index_for_id_keys(db, coll, name, inner_kb, prefix=use_prefix)
        lower: bytes | None = None
        lower_inclusive = True
        upper: bytes | None = None
        upper_inclusive = True
        for op, bound in operator_ops.items():
            if isinstance(bound, dict):
                return None
            full = prefix_with_sep + encode_value_directed(bound, op_dir, collation=collation)
            effective_op = op
            if op_dir == -1:
                effective_op = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte"}[op]
            if effective_op == "$gt":
                lower, lower_inclusive = full, False
            elif effective_op == "$gte":
                lower, lower_inclusive = full, True
            elif effective_op == "$lt":
                upper, upper_inclusive = full, False
            elif effective_op == "$lte":
                upper, upper_inclusive = full, True
            else:
                return None
        return self._range_scan_index(
            db,
            coll,
            name,
            lower,
            lower_inclusive,
            upper,
            upper_inclusive,
            prefix=prefix_with_sep,
        )

    def _range_scan_index(
        self,
        db: str,
        coll: str,
        name: str,
        lower: bytes | None,
        lower_inclusive: bool,
        upper: bytes | None,
        upper_inclusive: bool,
        *,
        prefix: bytes | None = None,
    ) -> list[bytes]:
        """Range-scan the index entries for ``name``.

        Optional ``prefix`` constrains the scan to entries whose escaped
        kb starts with ``escape(prefix)`` — used by compound-index
        prefix+range queries where leading equalities pin part of the kb.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_prefix = _escape_kb(prefix) if prefix is not None else None
        esc_lower = _escape_kb(lower) if lower is not None else None
        esc_upper = _escape_kb(upper) if upper is not None else None
        if esc_lower is not None:
            seed = esc_lower + _ENTRY_SEP
        elif esc_prefix is not None:
            seed = esc_prefix
        else:
            seed = b""
        c.set_key(db, coll, name, seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if esc_prefix is not None and not row_esc.startswith(esc_prefix):
                break
            if esc_lower is not None and not lower_inclusive and row_esc == esc_lower:
                if c.next() != 0:
                    break
                continue
            if esc_upper is not None:
                if upper_inclusive:
                    if row_esc > esc_upper:
                        break
                elif row_esc >= esc_upper:
                    break
            out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _range_scan_index_leading(
        self,
        db: str,
        coll: str,
        name: str,
        lower: bytes | None,
        lower_inclusive: bool,
        upper: bytes | None,
        upper_inclusive: bool,
    ) -> list[bytes]:
        """Range-scan a compound index using only its leading field.

        Each row's escaped kb is
        ``escape(enc(leading)) + escape(COMPOUND_SEP) + escape(enc(trailing...))``.
        Boundary detection uses ``startswith(esc_X + esc_compound_sep)`` to
        identify rows whose leading field equals ``X`` — the terminator
        bytes of an escaped numeric encoding can overlap with the start of
        the escaped compound separator, so a literal find/split on the
        separator is unreliable.
        """
        esc_compound_sep = _escape_kb(COMPOUND_SEP)
        c = self._cursor(_IDX_ENTRIES_TABLE)
        esc_lower = _escape_kb(lower) if lower is not None else None
        esc_upper = _escape_kb(upper) if upper is not None else None
        seed = esc_lower if esc_lower is not None else b""
        c.set_key(db, coll, name, seed)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        lower_eq_prefix = esc_lower + esc_compound_sep if esc_lower is not None else None
        upper_eq_prefix = esc_upper + esc_compound_sep if esc_upper is not None else None
        out: list[bytes] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            row_esc, row_id = _unpack_entry(packed)
            if (
                lower_eq_prefix is not None
                and not lower_inclusive
                and row_esc.startswith(lower_eq_prefix)
            ):
                if c.next() != 0:
                    break
                continue
            if esc_upper is not None:
                if upper_inclusive:
                    if row_esc > esc_upper and not row_esc.startswith(upper_eq_prefix):
                        break
                elif row_esc >= esc_upper:
                    break
            out.append(row_id)
            if c.next() != 0:
                break
        return out
