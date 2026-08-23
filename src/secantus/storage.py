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
import logging
import os
import re
import shutil
import tempfile
import threading
import time as _time
import uuid as _uuid
from collections.abc import Callable, Iterable, Iterator, Mapping
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
from secantus.paths import get_path, get_path_values
from secantus.projection import apply_projection_batch
from secantus.query import matches
from secantus.sortkey import (
    COMPOUND_SEP,
    RANK_ARRAY,
    encode_value,
    encode_value_directed,
)
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
# Legacy single documents table. Retained for the one-time on-open migration of a
# pre-shard store; new rows go to the per-collection shards below.
_DOC_TABLE = "table:secantus_documents"
# The documents table is sharded across ``_DOC_SHARDS`` WT tables, routed by a
# deterministic hash of ``(db, coll)`` so concurrent writers to different
# collections land on different WT files (different block-manager locks + cache
# regions) instead of one shared ``secantus_documents`` file — measured ~+19%
# aggregate throughput at 4 concurrent writers (see the Rust ``DOC_SHARDS`` and
# ``tasks/rust-mongodb-parity-redesign.md``). Every collection lives entirely in
# one shard, so per-collection ops touch a single shard with no merge. The Python
# and Rust servers MUST route identically (same hash) so a collection resolves to
# the same shard in both — cross-server backup / PITR portability.
_DOC_SHARDS = 16


def _doc_shard_hash(db: str, coll: str) -> int:
    """FNV-1a (64-bit) over ``db + b"\\x00" + coll`` — byte-for-byte identical to
    the Rust ``doc_shard_hash`` so both servers route a collection to the same
    shard. ``std``'s hash is randomised per run and cannot be used here."""
    h = 0xCBF29CE484222325
    for b in db.encode() + b"\x00" + coll.encode():
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _doc_shard_name(s: int) -> str:
    return f"table:secantus_documents_sh{s}"


@functools.lru_cache(maxsize=4096)
def _doc_table_for(db: str, coll: str) -> str:
    # Memoised: the pure-Python FNV byte loop ran on every storage op for an
    # immutable (db, coll) -> shard-name mapping.
    return _doc_shard_name(_doc_shard_hash(db, coll) % _DOC_SHARDS)


# Every documents shard table + the legacy single table (for migration / purge /
# rename table-set operations).
_DOC_ALL_TABLES = [_doc_shard_name(s) for s in range(_DOC_SHARDS)] + [_DOC_TABLE]


class IncompatibleStorageFormatError(RuntimeError):
    """The on-disk store was written by a build with an incompatible format and
    cannot be opened. Raised at ``Storage.__init__`` — there is deliberately no
    in-place migration (see ``_reject_pre_recordid_doc_format``)."""


def _migrate_legacy_docs(session: Any) -> None:
    """One-time on-open migration: move every row in the legacy single
    ``secantus_documents`` table to its per-collection shard (a store written
    before doc-sharding). A born-sharded store's legacy table is empty, so this is
    a quick no-op scan. Mirrors the Rust ``migrate_legacy_docs``."""
    src = session.open_cursor(_DOC_TABLE, None)
    rows: list[tuple[str, str, bytes, bytes]] = []
    try:
        rc = src.next()
        while rc == 0:
            k = src.get_key()
            rows.append((k[0], k[1], bytes(k[2]), bytes(src.get_value())))
            rc = src.next()
    finally:
        src.close()
    if not rows:
        return
    # The legacy single table predates both sharding AND RecordId keying: its rows
    # are ``(db, coll, id_key) -> raw blob``. Re-key each into its shard by a fresh
    # RecordId (the framed value carries the id_key), and write the ``_id`` index
    # row (id_key -> RecordId). ``_scan_max_nat_seq`` (run just after, from
    # ``_load_oplog_meta``) recovers the counter from the shards so freshly minted
    # RecordIds stay strictly greater.
    idx = session.open_cursor(_NAT_SEQ_TABLE, None)
    made_shards: set[str] = set()
    try:
        for recordid, (db, coll, id_key, blob) in enumerate(rows, start=1):
            shard = _doc_table_for(db, coll)
            # Lazy shards: the target shard isn't created eagerly at open, so make
            # it before folding a legacy row into it (idempotent per shard).
            if shard not in made_shards:
                session.create(shard, _DOC_SHARD_CFG)
                made_shards.add(shard)
            dst = session.open_cursor(shard, None)
            try:
                dst.set_key(db, coll, recordid)
                dst.set_value(_frame_doc_value(id_key, blob))
                dst.insert()
            finally:
                dst.close()
            idx.reset()
            idx.set_key(db, coll, id_key)
            idx.set_value(recordid)
            idx.insert()
    finally:
        idx.close()
    delc = session.open_cursor(_DOC_TABLE, None)
    try:
        for db, coll, id_key, _ in rows:
            delc.set_key(db, coll, id_key)
            with contextlib.suppress(Exception):
                delc.remove()
            delc.reset()
    finally:
        delc.close()


def _extract_key_format(cfg: str) -> str | None:
    """Parse the ``key_format=<fmt>`` clause out of a WiredTiger config /
    ``metadata:`` string (``fmt`` is a simple token — ``SSq``, ``SSu``, … — so it
    stops at the next comma). ``None`` if the string has no ``key_format``.
    Mirrors the Rust ``extract_key_format``."""
    marker = "key_format="
    start = cfg.find(marker)
    if start < 0:
        return None
    rest = cfg[start + len(marker) :]
    end = rest.find(",")
    return (rest if end < 0 else rest[:end]).strip()


def _reject_pre_recordid_doc_format(session: Any) -> None:
    """Refuse to open a store whose document shards were written by a build BEFORE
    the RecordId doc-table change. Those tables are keyed ``SSu``
    (``(db, coll, id_key)``) with unframed blob values; this build keys them
    ``SSq`` (``(db, coll, RecordId)``) with framed values (see
    ``_frame_doc_value``). WiredTiger fixes a table's ``key_format`` at CREATE
    time and preserves it across reopen — the bootstrap ``create`` is a no-op for
    an existing table, which is exactly what lets ``_migrate_legacy_docs`` read
    the legacy table as ``SSu`` — so the on-disk schema read here from the
    ``metadata:`` cursor is the ground truth.

    There is deliberately **no in-place migration** (decision on record,
    2026-07-24): SecantusDB is pre-1.0 beta, so the correct response to an
    incompatible on-disk format is to refuse to open rather than silently
    mis-read stored data with ``SSq`` cursor ops against an ``SSu`` btree. Mirrors
    the Rust ``reject_pre_recordid_doc_format``. (A pre-*shard* store is the
    separate, supported case — its legacy single ``secantus_documents`` table is
    folded in by ``_migrate_legacy_docs`` — so only the sharded doc tables are
    inspected here.)
    """
    meta = session.open_cursor("metadata:", None)
    try:
        for s in range(_DOC_SHARDS):
            name = _doc_shard_name(s)
            meta.reset()
            meta.set_key(name)
            if meta.search() != 0:
                continue
            if _extract_key_format(str(meta.get_value())) == "SSu":
                raise IncompatibleStorageFormatError(
                    f"SecantusDB storage at this path was written by a build before "
                    f"the RecordId doc-table change: '{name}' is keyed 'SSu' but this "
                    f"build requires 'SSq'. There is no in-place upgrade (pre-1.0 "
                    f"beta, no migration) — start from a fresh data directory or "
                    f"downgrade to the build that wrote it."
                )
    finally:
        meta.close()


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
#: ``(db, coll, index) + escape(sortkey) -> RecordId``. Unique indexes ONLY.
#: The index-entries table above keys by ``sortkey + RecordId``, so two docs
#: sharing an indexed value produce DIFFERENT keys and never collide — which is
#: why uniqueness had to be a snapshot-read probe, and why that probe could not
#: see a value another transaction committed after your snapshot. Here the key
#: IS the indexed value, so WiredTiger enforces it: a duplicate is WT's own
#: WT_DUPLICATE_KEY, and two concurrent inserts of the same value are a
#: write-write conflict rather than two silent successes.
_UNIQ_TABLE = "table:secantus_unique_keys"
#: Pending-drop tombstones written by the RUST server's chunked two-phase drop
#: (phase 1 unregisters the collection and writes `(db, coll) -> b""` here;
#: phase 2 purges the rows in bounded batches and clears the row). The Python
#: server never writes tombstones — its drop is autocommit per-row, so it has
#: no unbounded purge transaction — but it must FINISH one left by a Rust
#: crash mid-purge (cross-server portability: the layouts are byte-identical),
#: or the orphan rows resurface inside a re-created collection.
_TOMB_TABLE = "table:secantus_drop_tombstones"
_OPLOG_TABLE = "table:secantus_oplog"
# The oplog is sharded across ``_OPLOG_SHARDS`` btrees in the Rust server so
# concurrent writers don't all rendezvous on one table's rightmost append page
# (the scaling fix; see ``tasks/rust-mongodb-parity-redesign.md`` and the Rust
# ``OPLOG_SHARDS``). The Rust server routes each *batch* (one emit) to one shard
# by ``start_seq % N`` — per-batch, not per-entry, so a batch stays a contiguous
# sequential append (per-entry scatter destroys that locality). A seq's shard is
# therefore NOT a function of the seq: ordered reads use a k-way merge and per-seq
# point-ops probe every table. The **Python** server writes only the legacy single
# table above (its global lock serialises everything, so sharding buys it
# nothing), but it must still *read + recover + prune* a Rust-written store's
# sharded oplog for cross-server PITR / backup portability — so every oplog reader
# here merges the shard tables with the legacy table.
_OPLOG_SHARDS = 16


def _oplog_shard_name(shard: int) -> str:
    return f"table:secantus_oplog_sh{shard}"


# Every table an oplog reader / point-op must consider: the N shards + legacy.
_OPLOG_ALL_TABLES = [_oplog_shard_name(s) for s in range(_OPLOG_SHARDS)] + [_OPLOG_TABLE]
_PREIMAGE_TABLE = "table:secantus_preimages"

# WT create configs for the on-demand shard tables (byte-identical to the eager
# bootstrap creates and to the Rust ``DOC_TABLE_CFG`` / oplog-shard cfg).
_DOC_SHARD_CFG = "key_format=SSq,value_format=u"
_OPLOG_SHARD_CFG = "key_format=q,value_format=u"


def _is_missing_table(exc: BaseException) -> bool:
    """True if a WiredTiger error is "table does not exist" (ENOENT).

    Under lazy shard creation a documents / oplog shard table exists only once
    something has been written to it, so read / scan / merge paths must treat a
    cursor-open failure on an absent shard as "empty shard", not an error. WT
    surfaces this as ``No such file or directory``."""
    return "No such file or directory" in str(exc)


_OPLOG_META_TABLE = "table:secantus_oplog_meta"
_USERS_TABLE = "table:secantus_users"
_ROLES_TABLE = "table:secantus_roles"
_PROFILE_TABLE = "table:secantus_profile_settings"

_OPLOG_PRUNE_INTERVAL = 1000  # call prune_oplog every N emits

# Name of the advisory point-in-time-recovery manifest embedded in a backup
# archive (see Storage._pitr_manifest). Not a WiredTiger file; WT ignores it.
_PITR_MANIFEST_NAME = "pitr-manifest.json"

_ENTRY_SEP = b"\x00\x00"

# On-disk index-ENTRY format version, recorded per index as ``options.entryFormat``
# in the index catalog. 1 (implicit, absent) = entries whose trailing half is the
# doc's ``id_key``; 2 = entries whose trailing half is the 8-byte RecordId. The
# catalog is the only place this is visible — the WT ``key_format`` is ``SSSu``
# either way — so an absent marker is how a legacy store is detected (see
# ``_reject_legacy_index_entry_format``). Mirrors the Rust ``ENTRY_FORMAT_RECORDID``.
_ENTRY_FORMAT_RECORDID = 2


def _escape_kb(kb: bytes) -> bytes:
    """Order-preserving escape so ``\\x00\\x00`` is unambiguous as a separator."""
    return kb.replace(b"\x00", b"\x00\xff")


def _pack_entry(kb: bytes, recordid: int) -> bytes:
    """Pack a sortable index-entry payload into a single ``u`` column:
    ``escape(kb) + b"\\x00\\x00" + RecordId (8 bytes, big-endian)``.

    WiredTiger length-prefixes ``u`` columns when they're not last in the
    key, which breaks lexicographic comparison. Packing both fields into
    one trailing ``u`` column lets the B-tree do the sort for us — by
    ``escape(kb)`` first, then by RecordId.

    **RecordId entry format (``_ENTRY_FORMAT_RECORDID``).** The trailing half
    used to be the doc's ``id_key``, which made an IXSCAN fetch pay
    ``id_key → _id index → RecordId → doc``. Storing the RecordId directly drops
    that hop. Big-endian is deliberate: it keeps the ordering within one key in
    RecordId (insertion) order, and it is fixed-width, so the trailing half needs
    no escaping even though a RecordId's bytes may themselves contain
    ``\\x00\\x00`` (``_unpack_entry`` splits at the FIRST separator, and the
    escaped ``kb`` half cannot contain one). Byte-identical to the Rust
    ``pack_entry``.
    """
    return _escape_kb(kb) + _ENTRY_SEP + recordid.to_bytes(8, "big", signed=True)


def _is_whole_array_key(escaped_key: bytes, idx_dir: int) -> bool:
    """Whether an index entry's key is a whole-array key rather than an element.

    The first byte of an encoded value is its type rank (see `sortkey`), and
    escaping only rewrites `\x00`, which no rank byte is. A descending column is
    encoded byte-inverted, so the rank arrives as `0xFF - rank` there.
    """
    if not escaped_key:
        return False
    first = escaped_key[0]
    expected = RANK_ARRAY if idx_dir >= 0 else 0xFF - RANK_ARRAY
    return first == expected


def _unpack_entry(packed: bytes) -> tuple[bytes, int | None]:
    """Return ``(escaped_kb, RecordId)`` from a packed entry, splitting at the
    FIRST ``\\x00\\x00`` — correct because the ``kb`` half is escaped.

    A trailing half that is not exactly 8 bytes is a pre-RecordId entry; callers
    must never see one (``_reject_legacy_index_entry_format`` refuses such a
    store at open), so it is reported as ``None`` rather than silently mis-read
    as some other document's RecordId."""
    sep = packed.find(_ENTRY_SEP)
    if sep < 0:
        return packed, None
    tail = packed[sep + 2 :]
    if len(tail) != 8:
        return packed[:sep], None
    return packed[:sep], int.from_bytes(tail, "big", signed=True)


def _frame_doc_value(id_key: bytes, blob: bytes) -> bytes:
    """Frame a doc-table value as ``[u32-LE id_key_len][id_key][blob]``.

    RecordId step 4a: the doc table is keyed by the monotonic RecordId, not the
    ``id_key``, so the ``id_key`` (which a scan / delete still needs — and which a
    timeseries doc carries suffixed, not derivable from ``_id``) is stored *in* the
    value alongside the BSON blob. Byte-identical to the Rust server's
    ``frame_doc_value`` (``crates/secantus-storage/src/lib.rs``) so a store written
    by one server reads on the other — cross-server backup / PITR portability.
    """
    return len(id_key).to_bytes(4, "little") + id_key + blob


def _unframe_doc_value(value: bytes) -> tuple[bytes, bytes]:
    """Split a framed doc-table value into ``(id_key, blob)``. Inverse of
    [`_frame_doc_value`]; mirrors the Rust ``unframe_doc_value``."""
    if len(value) < 4:
        raise ValueError("doc-table value shorter than 4-byte frame header")
    n = int.from_bytes(value[:4], "little")
    rest = value[4:]
    if len(rest) < n:
        raise ValueError("doc-table value id_key length exceeds frame")
    return rest[:n], rest[n:]


def _reject_legacy_index_entry_format(session: Any) -> None:
    """Refuse to open a store whose index entries predate the RecordId entry
    format. Those entries carry the doc's ``id_key`` in their trailing half; this
    build reads that half as an 8-byte RecordId. Unlike the doc-table change this
    is **not** visible in WiredTiger's ``key_format`` (``SSSu`` either way) — the
    difference is inside the value bytes — so the index catalog carries an
    explicit ``options.entryFormat`` marker and its absence is the signal.

    There is deliberately **no migration**: refusing to open beats re-packing
    every index entry on a path that has to be perfect. (``_unpack_entry`` already
    returns ``None`` for a legacy entry rather than mis-reading it, so nothing
    fetches the wrong document even before this fires — this turns a silent
    nothing-matches into a loud refusal.) Mirrors the Rust
    ``reject_legacy_index_entry_format``.
    """
    try:
        c = session.open_cursor(_IDX_TABLE, None)
    except Exception:  # table absent on a virgin store
        return
    try:
        rc = c.next()
        while rc == 0:
            db, coll, name = c.get_key()
            blob = bytes(c.get_value())
            if blob:
                opts = bson.decode(blob).get("options") or {}
                fmt = opts.get("entryFormat", 1)
                if not isinstance(fmt, int) or fmt < _ENTRY_FORMAT_RECORDID:
                    raise IncompatibleStorageFormatError(
                        f"SecantusDB storage at this path has index entries written "
                        f"by a build before the RecordId index-entry change: index "
                        f"'{name}' on '{db}.{coll}' is entryFormat {fmt}, but this "
                        f"build requires {_ENTRY_FORMAT_RECORDID}. There is no "
                        f"in-place upgrade (pre-1.0 beta, no migration) — start from "
                        f"a fresh data directory, drop and recreate the indexes, or "
                        f"downgrade to the build that wrote it."
                    )
            rc = c.next()
    finally:
        c.close()


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
        # `filter="data"` (PEP 706 path-traversal hardening) is only accepted on
        # 3.12+ and the 3.10.12 / 3.11.4 backports — not on older 3.10/3.11 patch
        # releases (e.g. python.org's last 3.10 Windows binary, 3.10.11). Use it
        # when available; otherwise extract without it.
        if hasattr(tarfile, "data_filter"):
            tar.extractall(abs_target, filter="data")
        else:
            tar.extractall(abs_target)

    return {
        "targetDir": abs_target,
        "fileCount": len(names),
        "archive": abs_archive,
    }


class UniqueKeyTaken(Exception):
    """A unique-index key WiredTiger refused because another row holds it.

    Raised while index entries are written, which is AFTER the snapshot-read
    probe has passed — so this is precisely the case that probe cannot see: a
    value another transaction committed after our snapshot, or a concurrent
    insert of the same value. Callers turn it into the same duplicate-key write
    error the probe produces, so clients see one behaviour either way.
    """

    def __init__(self, index: str, key_pattern: dict[str, Any], key_value: dict[str, Any]) -> None:
        super().__init__(f"duplicate key on {index}: {key_value!r}")
        self.index = index
        self.key_pattern = key_pattern
        self.key_value = key_value


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


def _conflict_key_value(
    doc: Mapping[str, Any],
    key_spec: Mapping[str, Any],
    kb: bytes,
    *,
    collation: Any = None,
) -> dict[str, Any]:
    """Per-field values behind the entry ``kb`` — the ``keyValue`` of a
    dup-key error.

    A multikey doc contributes several keys, so the conflicting one
    isn't necessarily what ``get_path`` returns for the field (for a
    path descending through an array it never is). Re-walks the
    candidate values to find the combination that encodes to ``kb``.
    Only called once a duplicate has been found, so the walk costs
    nothing on the happy path.
    """
    from itertools import product

    fields = list(key_spec)
    per_field = [_index_field_values(doc, f)[0] for f in fields]
    for combo in product(*per_field):
        parts = [
            encode_value_directed(combo[i], int(key_spec[fields[i]]), collation=collation)
            for i in range(len(fields))
        ]
        cand = parts[0] if len(fields) == 1 else COMPOUND_SEP.join(parts)
        if cand == kb:
            return dict(zip(fields, combo, strict=True))
    return {field: get_path(dict(doc), field, default=None) for field in fields}


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


def _index_field_values(doc: Mapping[str, Any], field: str) -> tuple[list[Any], bool]:
    """Candidate index values for ``field`` in ``doc``, plus multikey-ness.

    Mirrors mongod's key generation:

    * a scalar leaf contributes itself;
    * an array-valued *leaf* contributes one value per element **plus**
      the whole array (the key a whole-array equality query probes);
    * a path that descends *through* an array — ``prices.owner_id``
      against ``{"prices": [{"owner_id": x}, ...]}`` — contributes one
      value per element's leaf, and no whole-array key (there is no
      single array to compare against).

    A missing path contributes ``None``, matching ``get_path``'s
    default: mongod indexes a missing field as null.

    The second element is True when this field makes the index
    multikey — either because the leaf is an array or because the path
    walked through one.
    """
    values, descended = get_path_values(doc, field)
    if not values:
        return [None], descended
    out: list[Any] = []
    multikey = descended
    for v in values:
        if isinstance(v, list):
            multikey = True
            out.extend(v)
            out.append(v)
        else:
            out.append(v)
    return out, multikey


def _index_field_exists(doc: Mapping[str, Any], field: str) -> bool:
    """``has_path`` that descends into arrays — the sparse-index gate.

    A sparse index must cover ``{"prices": [{"owner_id": x}]}`` for the
    path ``prices.owner_id``; plain :func:`has_path` reports that path
    missing because it won't walk array elements.
    """
    return bool(get_path_values(doc, field)[0])


def _doc_makes_multikey(doc: Mapping[str, Any], key_spec: Mapping[str, Any]) -> bool:
    """True if any field in ``key_spec`` is array-valued in ``doc`` — either
    an array leaf or a dotted path that descends through an array.

    Such a doc contributes more than one entry to the index, which is
    what mongod calls multikey: it disqualifies the index from
    sort-by-index walks (one doc → many keys breaks the natural-order
    walk) and is reported as ``isMultiKey`` in ``explain``.
    """
    return any(_index_field_values(doc, field)[1] for field in key_spec)


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

    One key per doc: an array-valued field encodes as the whole array,
    and a path descending through an array resolves to null. That makes
    this the wrong tool for anything index-entry- or uniqueness-shaped —
    those go through :func:`_index_key_variants`, which enumerates every
    key a doc contributes. What's left here is encoding synthetic
    min/max bound specs, which have no array shape to lose.
    """
    if sparse:
        for field in key_spec:
            if not _index_field_exists(doc, field):
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
    the whole-array key; for a dotted path that descends *through* an
    array (``prices.owner_id`` over an array of subdocuments) one key
    per element's leaf value. Mirrors real ``mongod``'s multikey index
    layout. This makes:

    * ``{tags: "python"}`` against ``{tags: ["python", "go"]}`` light
      up via the per-element entry for ``"python"``.
    * ``{tags: ["python", "go"]}`` (whole-array equality) light up via
      the whole-array entry — without this, the equality lookup would
      false-negative.
    * ``{"prices.owner_id": x}`` against
      ``{prices: [{owner_id: x}, ...]}`` light up via the per-element
      entry for ``x`` — the ODM array-of-subdocuments pattern.
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
            if not _index_field_exists(doc, field):
                return []

    # Per-field candidate values (see ``_index_field_values``), deduped
    # on their encoded bytes so a repeated array element doesn't inflate
    # the compound cartesian product below.
    per_field: list[list[Any]] = []
    for field in fields:
        cands, _multikey = _index_field_values(doc, field)
        if len(cands) == 1:
            per_field.append(cands)
            continue
        d = int(key_spec[field])
        seen: set[bytes] = set()
        uniq: list[Any] = []
        for cand in cands:
            eb = encode_value_directed(cand, d, collation=collation)
            if eb in seen:
                continue
            seen.add(eb)
            uniq.append(cand)
        per_field.append(uniq)

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


def _parse_cache_bytes(cache_size: str) -> int:
    """Parse a WiredTiger cache-size string ("128M", "1G", "512K", plain
    bytes) to bytes. Unknown forms fall back to the 1G default rather than
    failing storage open over a tuning knob."""
    s = cache_size.strip().upper()
    mult = 1
    for suffix, m in (("K", 1024), ("M", 1024**2), ("G", 1024**3), ("T", 1024**4)):
        if s.endswith(suffix):
            s = s[: -len(suffix)]
            mult = m
            break
    try:
        return int(float(s) * mult)
    except ValueError:
        return 1024**3


class TransactionTooLargeError(Exception):
    """A multi-document transaction's buffered write volume exceeded the
    storage cache's dirty budget. Raised BEFORE the transaction can pin
    enough unevictable dirty content to livelock WiredTiger (the same
    engine-stall class the chunked-insert work closed for plain batch
    writes — a user transaction's statements all join one WT transaction,
    so chunking cannot apply and the guard must be explicit). Caught at
    the command layer and surfaced as mongod's ``TransactionTooLargeForCache``
    (313), which mongod introduced for exactly this condition; the failed
    statement aborts the transaction and carries NO transient label —
    retrying an oversized transaction would hit the same wall."""


class WriteConflictError(Exception):
    """A WiredTiger WT_ROLLBACK: two transactions touched the same item.

    Inside a user (multi-document) transaction this surfaces to the
    client as mongod's statement-time ``WriteConflict`` (code 112) with
    the ``TransientTransactionError`` label, and the transaction is
    aborted server-side. Outside a transaction the storage layer
    retries the write until it goes through (matching mongod's
    unbounded ``writeConflictRetry`` — a user transaction holds its
    uncommitted writes until commit/abort, so the competitor always
    resolves), logging a warning during long retry stretches.
    """


def _is_wt_duplicate_key(exc: BaseException) -> bool:
    """True when a ``WiredTigerError`` is the WT_DUPLICATE_KEY signal from an
    ``overwrite=false`` insert. Distinguishing it matters: every other WT error
    on that path (rollback, I/O, panic) is a storage failure, and reporting one
    as a duplicate-key write error would tell the client its data was rejected
    when in fact the write broke."""
    return "WT_DUPLICATE_KEY" in str(exc)


def _is_wt_rollback(exc: BaseException) -> bool:
    """True when a ``WiredTigerError`` is the WT_ROLLBACK conflict signal
    (as opposed to e.g. WT_DUPLICATE_KEY). The SWIG binding raises a
    typed ``WiredTigerRollbackError`` subclass; the message match is a
    fallback for raise-sites that re-wrap into the base class."""
    if isinstance(exc, wt.WiredTigerRollbackError):
        return True
    msg = str(exc)
    return "WT_ROLLBACK" in msg or "conflict between concurrent operations" in msg


def _commit_batch_transaction(session: Any, sync: bool) -> None:
    """Commit a batch transaction, mapping a commit-time conflict to
    ``WriteConflictError``.

    A concurrent transaction can mark this one rollback-only after its
    last operation ran; WiredTiger then fails the ``commit_transaction``
    call itself. That failure surfaces as a bare ``WiredTigerError``
    whose message is just ``"Invalid argument"`` — no ``WT_ROLLBACK``
    marker — so without this mapping it escapes the write-conflict
    retry wrapper and reaches the client as a generic internal error
    (found by ``bench.concurrency`` under 2+ concurrent writers).
    ``get_rollback_reason()`` carries the real cause; a commit failure
    with NO rollback reason is a genuine durability error and stays
    loud.
    """
    try:
        session.commit_transaction("sync=on" if sync else None)
    except wt.WiredTigerError as exc:
        reason = None
        with contextlib.suppress(Exception):
            reason = session.get_rollback_reason()
        # WT rolls a failed commit back itself; the explicit rollback is
        # belt-and-braces for binding versions that leave the txn open,
        # and raises (suppressed) when there is no transaction to roll
        # back.
        with contextlib.suppress(Exception):
            session.rollback_transaction()
        # Empirically (WT 7.0 binding): when the transaction was marked
        # rollback-only internally, commit's auto-rollback CLEARS the
        # rollback reason before the exception reaches us, and the
        # exception is the bare errno string "Invalid argument" — WT's
        # documented EINVAL for committing a rollback-required
        # transaction ("failed transaction requires rollback: conflict
        # between concurrent operations" goes only to the event
        # handler/stderr). Our commit config is a fixed literal
        # (``sync=on``/None, exercised by every test run), so EINVAL
        # here has exactly one remaining cause. The SWIG binding has no
        # panic subclass (only WiredTigerError / WiredTigerRollbackError),
        # so panics are excluded by their WT_PANIC message.
        msg = str(exc).strip()
        is_rollback_einval = "WT_PANIC" not in msg and msg == "Invalid argument"
        if reason or is_rollback_einval or _is_wt_rollback(exc):
            raise WriteConflictError(reason or str(exc)) from exc
        raise


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


_conflict_log = logging.getLogger("secantus.storage.conflict")

_WRITE_CONFLICT_RETRY_DELAY_S = 0.005
_WRITE_CONFLICT_RETRY_DELAY_MAX_S = 0.02
_WRITE_CONFLICT_RETRY_LOG_EVERY_S = 5.0


def _retry_write_conflicts(fn: Callable[..., Any]) -> Callable[..., Any]:
    """Retry a whole public write method on WT_ROLLBACK.

    Safe because the failed attempt's ``_batch_transaction`` already
    rolled everything back and the per-collection lock is released on
    the way out — the retry re-runs from scratch. Retries are
    UNBOUNDED, matching mongod's ``writeConflictRetry``: a client of
    mongod never sees ``WriteConflict`` for a plain write outside a
    multi-document transaction, so neither should ours (the previous 5s
    deadline turned saturated contention into client-visible errors).
    A WARNING is logged every few seconds of continuous retrying so a
    pathological livelock is visible in the server log. Inside a user
    transaction the conflict is NOT retried: it surfaces immediately so
    the command layer can abort the transaction with mongod's
    statement-time ``WriteConflict``.
    """

    @functools.wraps(fn)
    def wrapper(self: Storage, *args: Any, **kwargs: Any) -> Any:
        started: float | None = None
        last_log: float | None = None
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
                if started is None:
                    started = last_log = now
                elif now - (last_log or now) >= _WRITE_CONFLICT_RETRY_LOG_EVERY_S:
                    last_log = now
                    _conflict_log.warning(
                        "%s retrying on write conflicts for %.1fs (%s)",
                        fn.__name__,
                        now - started,
                        exc,
                    )
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

    __slots__ = (
        "session",
        "cursors",
        "began",
        "closed",
        "oplog_entries",
        "pre_images",
        "written",
        "dirty_bytes",
    )

    def __init__(self, session: Any) -> None:
        self.session = session
        self.cursors: dict[tuple[str, bool], Any] = {}
        self.began = False
        self.closed = False
        self.oplog_entries: list[dict[str, Any]] = []
        self.pre_images: list[bytes | None] = []
        # (db, coll) this transaction has written to. A committed-state read
        # (``find_matching_committed``) is only authoritative for a collection
        # the transaction has NOT touched: once it has deleted or rewritten a
        # row, the committed view of that row is stale and would report a
        # conflict against a value the transaction has already freed.
        self.written: set[tuple[str, str]] = set()
        # Approximate bytes this transaction has written, accumulated from
        # its buffered oplog entries (which carry the full documents). The
        # engine-side dirty footprint is roughly twice this (doc rows +
        # oplog rows) plus index entries; ``_emit_oplog``'s buffering
        # branch enforces the cache-derived budget against it.
        self.dirty_bytes = 0


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


class IndexKeySpecsConflict(Exception):
    """``create_index`` was called with a name that already exists in the
    collection but for a **different key spec** (e.g. ``{a: 1}`` vs
    ``{a: -1}``). Real mongod rejects with ``IndexKeySpecsConflict`` (code
    86); mongo-cxx-driver's ``create_index tests/fails`` and
    ``index_view/fails for same name`` pin the rejection."""


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
        oplog_archive_dir: str | None = None,
        durable: bool | None = None,
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
        # Public, read-only view of the above for callers outside storage
        # (``serverStatus.storageEngine.persistent``). Exposed as a property
        # below rather than a second attribute so it cannot drift.
        # Dirty budget for one multi-document transaction, derived from the
        # cache: WT starts stalling application threads around its dirty
        # trigger (~20% of cache), and dirty content belonging to an OPEN
        # transaction is unevictable — a transaction allowed to fill that
        # budget livelocks the engine (only its own commit could free the
        # cache). mongod guards the same hazard with
        # ``TransactionTooLargeForCache``; 0.75 of the dirty trigger mirrors
        # its threshold default. The estimate compared against it is
        # 2 x buffered-entry bytes (doc rows + oplog rows).
        self._txn_dirty_limit = int(_parse_cache_bytes(cache_size) * 0.20 * 0.75)
        self.session_max = session_max
        self.sync_on_commit = sync_on_commit
        # ``durable`` (I2a test-mode fast storage). Resolution precedence:
        #   1. ``SECANTUS_FORCE_DURABLE=1`` — always durable, overriding any
        #      caller/default. This is the switch that runs the WHOLE test suite
        #      against real journal + close-checkpoint durability (the "real
        #      durable disk testing" path): `SECANTUS_FORCE_DURABLE=1 invoke
        #      test`, and a dedicated CI lane.
        #   2. explicit ``durable=`` argument (persistence / reopen / PITR /
        #      backup fixtures pass ``durable=True`` so they always exercise the
        #      journal + checkpoint, regardless of the test-fast default).
        #   3. ``durable=None`` (unset) — durable UNLESS
        #      ``SECANTUS_TEST_FAST_STORAGE=1`` is set (the test conftest sets it
        #      so the default suite runs fast). Production never sets it, so the
        #      shipped ``SecantusDBServer`` defaults to fully durable.
        # ``durable=False`` opens the on-disk engine with the journal disabled
        # and skips the checkpoint on ``close()``. All 12 tables are still
        # created on disk (schema / B-tree / within-session persistence stay
        # real), but the instance is NOT crash- or reopen-durable — correct only
        # for ephemeral test instances whose storage dir is discarded. It cuts
        # open+close from ~245 ms to ~52 ms and, more importantly, removes the
        # fsync that serialises across parallel test workers (see
        # tasks/test-performance-plan.md §I2a and the scaling curve).
        if os.environ.get("SECANTUS_FORCE_DURABLE") == "1":
            durable = True
        elif durable is None:
            durable = os.environ.get("SECANTUS_TEST_FAST_STORAGE") != "1"
        self._durable = durable
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
            # 10 MB matches mongod's WT default. (Kept at 10 MB, not
            # smaller: a single log record must fit in one segment, and a
            # write of a near-``maxBsonObjectSize`` (16 MB) document needs
            # headroom.)
            #
            # ``prealloc=false`` disables WT's log-file pre-allocation.
            # By default WT's log server keeps two ``file_max``-sized
            # ``WiredTigerPreplog`` files ready ahead of the active log, so
            # every on-disk instance costs ~3x ``file_max`` (~30 MB here)
            # of log space even for a database holding a few KB. That
            # pre-allocation is a write-latency optimisation for
            # sustained-throughput servers; SecantusDB is an ephemeral
            # in-process test database whose instances are small and
            # short-lived, so the latency win is irrelevant and the disk
            # cost is not — a full test run spins up thousands of
            # instances. Disabling prealloc drops each instance's log
            # footprint from ~30 MB to ~10 MB with no durability change
            # (recovery still replays the same log records); WT just
            # allocates each segment on demand instead of ahead of time.
            if durable:
                sync_part = (
                    "transaction_sync=(enabled=true,method=fsync)"
                    if sync_on_commit
                    else "transaction_sync=(enabled=false,method=fsync)"
                )
                config = (
                    f"create,session_max={session_max},cache_size={cache_size},"
                    f"log=(enabled=true,file_max=10MB,prealloc=false),"
                    f"{sync_part}"
                )
            else:
                # Fast test mode. Keep the journal ENABLED but skip the explicit
                # close-checkpoint (see ``close``). Counter-intuitively, keeping
                # logging on is the fast path: ``WT_CONNECTION->close`` only
                # implicit-checkpoints (an fsync) when logging is *off*, so
                # ``log=off`` is actually SLOWER on close once real data has been
                # written. With logging on and no explicit checkpoint, close does
                # no fsync — that removed fsync is what avoids the per-worker disk
                # serialisation under xdist. (Data stays recoverable via log
                # replay on reopen; the checkpoint the durable path adds bounds
                # recovery time and truncates the log — see FORCE_DURABLE.)
                config = (
                    f"create,session_max={session_max},cache_size={cache_size},"
                    f"log=(enabled=true,file_max=10MB,prealloc=false),"
                    f"transaction_sync=(enabled=false,method=fsync)"
                )
        # The on-disk WT home is stashed so ``create_archive`` can tar
        # it after a checkpoint without re-deriving the path.
        self.home_path = home
        self._conn = wt.wiredtiger_open(home, config)
        self._tls = threading.local()
        self._all_sessions: list[Any] = []
        # Documents shards created so far (lazy shard creation): a shard table is
        # made on first write to a collection that hashes to it, not all 16 at
        # open — so an ephemeral single-collection store creates one shard, not
        # 16 (cutting open-time table creation, the dominant open cost). Tracks
        # what this instance has created so the create is attempted once per
        # shard; a reopened on-disk store re-populates it lazily (WT create is
        # idempotent, preserving an existing table).
        self._created_doc_shards: set[str] = set()
        boot = self._conn.open_session()
        try:
            boot.create(_COLL_TABLE, "key_format=SS,value_format=u")
            boot.create(_DOC_TABLE, "key_format=SSu,value_format=u")
            # Per-collection documents shards (see ``_DOC_SHARDS``, keyed ``SSq``)
            # and the oplog shards below are NO LONGER created eagerly here. Each
            # doc shard is made on first write to a collection that hashes to it
            # (``_ensure_doc_shard``); the oplog shards are written only by the
            # Rust server, so a pure-Python store never creates them at all. This
            # is the open-cost cut: a fresh store created ~37 tables (16 doc + 16
            # oplog shards dominating), most unused by an ephemeral test server;
            # now it creates only the base tables plus the shards actually
            # touched. Every read / merge / scan path tolerates an absent shard
            # (``_is_missing_table`` / ``_cursor_optional``), so a store written
            # with a subset of shards stays byte-compatible with the Rust server
            # (cross-server backup / PITR): a missing shard reads as empty.
            boot.create(_NAT_TABLE, "key_format=SSq,value_format=u")
            boot.create(_NAT_SEQ_TABLE, "key_format=SSu,value_format=q")
            boot.create(_IDX_TABLE, "key_format=SSS,value_format=u")
            boot.create(_IDX_ENTRIES_TABLE, "key_format=SSSu,value_format=u")
            boot.create(_UNIQ_TABLE, "key_format=SSSu,value_format=q")
            boot.create(_TOMB_TABLE, "key_format=SS,value_format=u")
            boot.create(_OPLOG_TABLE, "key_format=q,value_format=u")
            boot.create(_PREIMAGE_TABLE, "key_format=q,value_format=u")
            boot.create(_OPLOG_META_TABLE, "key_format=S,value_format=u")
            boot.create(_USERS_TABLE, "key_format=SS,value_format=u")
            boot.create(_ROLES_TABLE, "key_format=SS,value_format=u")
            boot.create(_PROFILE_TABLE, "key_format=S,value_format=u")
            # Fail fast on a store whose doc shards predate the RecordId keying
            # change (``SSu`` on disk vs the ``SSq`` this build needs): no
            # in-place upgrade, refuse to open rather than mis-read. The
            # ``create`` above preserves an existing table's key_format, so the
            # on-disk schema is intact for this check. (Runs before
            # ``_migrate_legacy_docs`` — that path is the pre-*shard* case, which
            # this check does not touch.)
            _reject_pre_recordid_doc_format(boot)
            # Same fail-fast for the index-ENTRY format. Runs after the doc-table
            # check so the more fundamental mismatch is reported first.
            _reject_legacy_index_entry_format(boot)
            # One-time: fold a pre-shard store's legacy documents rows into the
            # per-collection shards (no-op for a born-sharded store).
            _migrate_legacy_docs(boot)
        except IncompatibleStorageFormatError:
            # Refusing to open: tear the connection down rather than leave a live
            # WT handle (and its home-directory lock) behind for a Storage that
            # never comes into existence.
            boot.close()
            with contextlib.suppress(Exception):
                self._conn.close()
            raise
        finally:
            with contextlib.suppress(Exception):
                boot.close()

        # Oplog state — durable across restart via _OPLOG_META_TABLE.
        self.oplog_retention_seconds = float(oplog_retention_seconds)
        self.oplog_max_entries = int(oplog_max_entries)
        # When set, ``prune_oplog`` writes the rows it is about to drop into a
        # durable oplog segment in this directory first (PITR v2), so recovery
        # can reach a time before the live oplog floor. See
        # :mod:`secantus.pitr_archive`.
        self.oplog_archive_dir = oplog_archive_dir
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
        # In-flight mint window: ``start_seq -> end_seq`` (exclusive) for every
        # minted batch whose transaction has not yet committed or rolled back.
        # Guarded by ``_oplog_seq_lock``. The **visible tail** — the largest
        # seq below which nothing can still appear — is ``min(window) - 1``
        # when non-empty, else ``_next_seq - 1`` (the analogue of WiredTiger /
        # mongod's ``all_durable`` timestamp, and the twin of the Rust
        # server's ``OplogState.in_flight``). Since the Phase-2.4
        # per-collection lock split, writers on different collections mint
        # and commit independently — a reader that advanced past a
        # minted-but-uncommitted seq would permanently lose the event when
        # its transaction commits, so every tail readers consume is bounded
        # by this window's floor. A rolled-back batch simply deregisters:
        # the abandoned range vanishes and ``min`` moves on (a permanent seq
        # hole, which the oplog merge already tolerates).
        self._oplog_in_flight: dict[int, int] = {}
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
            # Cluster-time mints are not persisted per call (see
            # ``current_cluster_time``), so the recovered
            # ``(last_ts_secs, last_ts_ord)`` can lag mints issued right
            # before a crash. Bump one full second past everything we
            # recovered: any unpersisted mint carried the wall-clock
            # second it was issued in, which is <= the second we
            # recovered from the meta row / oplog tail / this restart's
            # own wall clock — so +1s is strictly greater than all of
            # them. Costs at most a 1s forward jump of the (already
            # logical) cluster clock per restart; never applied to a
            # virgin store.
            if self._last_ts_secs > 0:
                self._last_ts_secs = max(self._last_ts_secs, int(self._time())) + 1
                self._last_ts_ord = 0
            # Live oplog row count, kept in memory so ``_prune_oplog_locked``
            # never has to walk the whole oplog to decide whether the entry cap
            # is exceeded. Seeded here by a one-time key-only count (cheap, and
            # only on open); maintained incrementally on every emit / prune.
            self._oplog_live_count = self._count_oplog_rows()
            # Finish any chunked drop the Rust server's crash interrupted
            # (registry row already gone; the tombstoned rows must not
            # resurface under a re-created name). See ``_TOMB_TABLE``.
            self._recover_pending_drops_locked()

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
                # The persisted counters are a *hint*, not the source of
                # truth: ``_emit_oplog`` no longer re-persists the meta row
                # on every write (it WT-rollbacks under concurrent writers —
                # see ``_emit_oplog``), so the on-disk ``next_seq`` /
                # ``next_nat_seq`` lag behind the actual tables whenever a
                # checkpoint (e.g. ``backupArchive``) lands between the last
                # meta persist and the next one. Trusting a stale value would
                # re-mint an already-used seq: a duplicate oplog seq (lost
                # change events) or — for the natural-order index — a seq
                # collision that overwrites a live doc's nat entry and
                # corrupts capped-collection FIFO eviction after restore.
                # So clamp each counter UP to what the tables actually
                # contain; the hint only ever saves a scan, never lowers us.
                next_seq = max(
                    int(state.get("next_seq", 1)),
                    self._scan_max_oplog_seq() + 1,
                )
                nat = state.get("next_nat_seq")
                next_nat = max(
                    self._scan_max_nat_seq() + 1,
                    1 if nat is None else int(nat),
                )
                return (
                    next_seq,
                    int(state.get("last_ts_secs", 0)),
                    int(state.get("last_ts_ord", 0)),
                    next_nat,
                )
        # Fallback: reconstruct from the newest oplog row. Sharded — the max seq
        # can be in any shard (or the legacy table); routing is per-batch so its
        # table isn't a function of the seq — probe each table for that seq's ts.
        last_seq = self._scan_max_oplog_seq()
        last_secs = 0
        last_ord = 0
        if last_seq > 0:
            for table in _OPLOG_ALL_TABLES:
                c2 = self._cursor_optional(table)
                if c2 is None:
                    continue  # lazy shards: absent oplog shard reads as empty
                c2.set_key(last_seq)
                if c2.search() == 0:
                    blob = bytes(c2.get_value())
                    c2.reset()
                    if blob:
                        entry = bson.decode(blob)
                        ts = entry.get("ts")
                        if isinstance(ts, Timestamp):
                            last_secs, last_ord = ts.time, ts.inc
                    break
                c2.reset()
        return last_seq + 1, last_secs, last_ord, self._scan_max_nat_seq() + 1

    def _scan_max_oplog_seq(self) -> int:
        """Largest ``seq`` present across all oplog shards + the legacy table
        (0 if all empty).

        Each table is keyed on the bare ``seq`` (``key_format=q``), so a single
        ``prev()`` from a table's end yields its maximum; the answer is the max of
        those. Used to clamp the recovered ``next_seq`` up past any stale persisted
        hint so a reopen can never re-mint an already-used oplog seq — the scan
        must include the shards or a Rust-written store's tail would be missed.
        """
        max_seq = 0
        for table in _OPLOG_ALL_TABLES:
            c = self._cursor_optional(table)
            if c is None:
                continue  # lazy shards: absent oplog shard reads as empty
            if c.prev() == 0:
                seq = int(c.get_key())
                if seq > max_seq:
                    max_seq = seq
            c.reset()
        return max_seq

    def _count_oplog_rows(self) -> int:
        """Number of live rows across all oplog shards + the legacy table.

        Key-only walk (never decodes a value), used once on open to seed
        ``self._oplog_live_count``. For a Python store every row lives in the
        legacy table; the shard tables are present-but-empty, so this is a
        single btree traversal in the common case.
        """
        total = 0
        for table in _OPLOG_ALL_TABLES:
            c = self._cursor_optional(table)
            if c is None:
                continue  # lazy shards: absent oplog shard reads as empty
            while c.next() == 0:
                total += 1
            c.reset()
        return total

    def _iter_oplog_oldest(self, session: Any) -> Iterable[tuple[int, dict[str, Any], str]]:
        """Yield ``(seq, entry, table)`` in ascending seq order, decoding each
        entry lazily and only as it is produced.

        The k-way merge mirrors ``_merge_oplog_on_session`` but as a generator:
        a consumer that ``break``s after the rows it cares about stops the walk
        there, so a prune that deletes D rows reads ~D+1 entries rather than the
        whole oplog. Opens its own cursors on ``session`` and closes them when
        the generator is exhausted or the consumer stops iterating.
        """
        cursors: list[Any] = []
        heads: list[int | None] = []
        try:
            for table in _OPLOG_ALL_TABLES:
                # Lazy shards: an absent oplog shard reads as empty. Keep the
                # cursors / heads / _OPLOG_ALL_TABLES index alignment by parking a
                # None (never selected, since its head stays None).
                try:
                    c = session.open_cursor(table, None)
                except Exception as exc:
                    if _is_missing_table(exc):
                        cursors.append(None)
                        heads.append(None)
                        continue
                    raise
                cursors.append(c)
                heads.append(int(c.get_key()) if c.next() == 0 else None)
            while True:
                best_i = None
                best_seq = None
                for i, h in enumerate(heads):
                    if h is not None and (best_seq is None or h < best_seq):
                        best_seq = h
                        best_i = i
                if best_i is None:
                    return
                c = cursors[best_i]
                blob = bytes(c.get_value())
                entry = bson.decode(blob) if blob else {}
                heads[best_i] = int(c.get_key()) if c.next() == 0 else None
                yield best_seq, entry, _OPLOG_ALL_TABLES[best_i]
        finally:
            for c in cursors:
                with contextlib.suppress(Exception):
                    c.close()

    def _scan_max_nat_seq(self) -> int:
        """Largest RecordId present across the document shards (0 if empty).

        Used to recover the insertion counter on reopen when the persisted
        ``next_nat_seq`` is absent (legacy DBs / runs with the oplog disabled),
        so minted RecordIds stay strictly greater than any existing doc-table
        key. RecordIds are global-monotonic, so any row in any shard could hold
        the max — scan every shard. Mirrors the Rust ``scan_max_nat_seq``.
        """
        max_seq = 0
        for s in range(_DOC_SHARDS):
            c = self._cursor_optional(_doc_shard_name(s))
            if c is None:
                continue  # lazy shard creation: this shard was never written
            c.reset()
            rc = c.prev()  # last row = highest (db, coll, RecordId) in this shard
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

    def _write_nat_entry(self, db: str, coll: str, id_key: bytes) -> int | None:
        """Assign the doc a RecordId and write the ``_id`` index row
        (``id_key -> RecordId``). Returns the RecordId — the caller keys the doc
        table by it — or ``None`` when the ``_id`` is already present (a duplicate
        key), which is where dup-``_id`` is now caught: the doc table is keyed by
        the unique RecordId, so it can no longer reject a dup itself.

        The forward ``_NAT_TABLE`` row (seq -> id_key) is gone: the doc table is
        itself in RecordId (= insertion) order, which is the 4->3 write-amp cut.
        Mirrors the Rust ``write_nat_entry``.
        """
        recordid = self._mint_nat_seq()
        # overwrite=False so a second write of the same ``_id`` fails instead of
        # silently replacing the first doc's RecordId. A RecordId wasted on the
        # dup path is harmless — they only need to be unique + monotonic.
        rev = self._cursor(_NAT_SEQ_TABLE, overwrite=False)
        rev.reset()
        rev.set_key(db, coll, id_key)
        rev.set_value(recordid)
        try:
            rev.insert()
        except wt.WiredTigerError as exc:
            if _is_wt_rollback(exc):
                raise WriteConflictError(str(exc)) from exc
            if _is_wt_duplicate_key(exc):
                return None
            # Any other storage error is a real failure — never dress it up as a
            # duplicate key, which would tell the client its document was
            # rejected when the write actually broke.
            raise
        return recordid

    def _doc_recordid(self, db: str, coll: str, id_key: bytes) -> int | None:
        """The ``_id`` index lookup: resolve a doc's ``id_key`` to its RecordId
        (the doc-table key). ``None`` if the doc doesn't exist. Mirrors the Rust
        ``doc_recordid``."""
        rev = self._cursor(_NAT_SEQ_TABLE)
        rev.reset()
        rev.set_key(db, coll, id_key)
        if rev.search() != 0:
            return None
        return int(rev.get_value())

    def _delete_nat_entry(self, db: str, coll: str, id_key: bytes) -> int | None:
        """Remove the doc's ``_id``-index row and return the RecordId it mapped
        to, so the caller can delete the doc-table row keyed by it. ``None`` if
        absent. Mirrors the Rust ``delete_nat_entry``."""
        rev = self._cursor(_NAT_SEQ_TABLE)
        rev.reset()
        rev.set_key(db, coll, id_key)
        if rev.search() != 0:
            return None
        recordid = int(rev.get_value())
        rev.remove()
        return recordid

    def _delete_doc_row(self, db: str, coll: str, recordid: int) -> None:
        """Remove the doc-table row for ``recordid``. No-op if already gone."""
        doc_cur = self._cursor_optional(_doc_table_for(db, coll))
        if doc_cur is None:
            return  # lazy shards: no shard → nothing to remove
        doc_cur.reset()
        doc_cur.set_key(db, coll, recordid)
        if doc_cur.search() == 0:
            doc_cur.remove()

    def _scan_docs_natural(self, db: str, coll: str) -> Iterable[tuple[int, bytes, bytes]]:
        """Yield ``(recordid, id_key, blob)`` in **insertion order**.

        The doc table is keyed by the monotonic RecordId, so a forward walk of it
        IS insertion order — this is now just ``_scan_docs``. Kept as a named
        alias because the capped-eviction / unsorted-``find`` call sites read
        better spelled "natural".
        """
        return self._scan_docs(db, coll)

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

        **LOCK ORDER — always acquire ``_coll_lock`` BEFORE ``self._lock``.**
        There is exactly one legal order and every path must follow it. The two
        ways a thread can end up holding both:

        * CRUD (``insert`` / ``update_matching`` / ``delete_matching``) takes
          ``_coll_lock``, then reaches ``_lock`` inside ``_session`` the first
          time a connection thread opens its WT session.
        * DDL that must exclude in-flight writes (``create_index``) takes
          ``_coll_lock`` and then ``_lock`` explicitly.

        Anything holding ``_lock`` must therefore NOT reach down for
        ``_coll_lock`` — that inverted order is an AB-BA deadlock against CRUD.
        ``_collection_uuid``'s mint path used to do exactly that and now takes
        ``_lock`` instead; see the note there.
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
            # Register the range in the in-flight window (same lock
            # acquisition — zero extra locking). The emitting scope parks it
            # on ``_tls.pending_minted`` for its resolution point (batch-txn
            # exit, user-txn commit/abort, or end-of-emit for bare
            # autocommit writes) to deregister via ``_deregister_minted``.
            self._oplog_in_flight[start] = start + n
            timestamps = [self._mint_ts() for _ in range(n)]
            return start, timestamps

    def _deregister_minted(self, ranges: list[tuple[int, int]]) -> None:
        """Remove minted seq ranges from the in-flight window and wake
        tailable waiters — their transaction committed (rows visible) or
        rolled back (rows can never appear); either way the visible tail
        may have advanced."""
        if not ranges:
            return
        with self._oplog_seq_lock:
            for start, _end in ranges:
                self._oplog_in_flight.pop(start, None)
        with self._oplog_cv:
            self._oplog_cv.notify_all()

    def _drain_pending_minted(self) -> list[tuple[int, int]]:
        """Take (and clear) the ranges the current thread's scope minted."""
        pending = getattr(self._tls, "pending_minted", None)
        if not pending:
            return []
        self._tls.pending_minted = []
        return pending

    def oplog_visible_tail_seq(self) -> int:
        """The highest seq a reader may consume or name in a resume
        position: everything at or below it is committed-and-visible or a
        permanent hole. Tail readers, resume-token high-water marks, and
        ``read_oplog``'s bound all use THIS, never the minted
        ``oplog_tail_seq`` — a minted-but-uncommitted seq below the minted
        tail is an event a reader would otherwise permanently skip."""
        with self._oplog_seq_lock:
            if self._oplog_in_flight:
                return min(self._oplog_in_flight) - 1
            return self._next_seq - 1

    def oplog_visible_tail_seq_nolock(self) -> int:
        """Lock-free ``oplog_visible_tail_seq`` for the tailable-getMore
        wake predicate (same deadlock-avoidance contract as
        ``oplog_tail_seq_nolock``: a waiter holding ``_oplog_cv`` must not
        take other locks). Dict reads are atomic under the GIL; a
        momentarily stale value self-corrects on the next predicate check
        because every deregistration notifies the condvar."""
        inflight = self._oplog_in_flight
        if inflight:
            try:
                return min(inflight) - 1
            except ValueError:  # raced to empty between check and min
                pass
        return self._next_seq - 1

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
        # Mint path — serialise the mint, then re-read after acquiring so a
        # racer that won the mint race is observed.
        #
        # LOCK ORDER (see the note on ``_coll_lock``): this takes ``_lock``, NOT
        # ``_coll_lock``. Nine DDL methods (create_collection / drop_collection /
        # drop_database / rename_collection / record_collmod / prune_ttl /
        # create_index / drop_index / drop_all_indexes) call this *while already
        # holding* ``_lock``; if the mint reached down for ``_coll_lock`` those
        # paths would run ``_lock`` → ``_coll_lock``, the exact inverse of the
        # ``_coll_lock`` → ``_lock`` order that CRUD takes (insert / update /
        # delete hold ``_coll_lock`` and then hit ``_lock`` inside ``_session``
        # on a connection thread's first use). That is a textbook AB-BA
        # deadlock. Using ``_lock`` here is reentrant for every DDL caller and
        # keeps CRUD callers on the single canonical order.
        with self._lock:
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
        """Return a strictly-monotonic ``Timestamp`` advancing the cluster clock.

        Deliberately does NOT persist the oplog meta row: this runs on
        every ``hello`` reply under the replica-set persona (driver
        heartbeats) and on change-stream high-water-mark minting, so a
        per-call meta write is a single-row hotspot every concurrent
        writer conflicts on (the same storm ``_emit_oplog`` was cured
        of). Restart monotonicity is guaranteed structurally instead:
        recovery bumps the clock one second past everything it can see
        (see ``_load_oplog_meta``), which covers any mint that was never
        persisted.
        """
        with self._oplog_seq_lock:
            return self._mint_ts()

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
        with self._lock:
            if self._closed:
                return []
            session = self._conn.open_session()
            try:
                # Sharded: merge every shard + the legacy table in seq order.
                merged, _scan_high = self._merge_oplog_on_session(session, 0, 2**63 - 1)
                return [entry for _seq, entry in merged]
            finally:
                with contextlib.suppress(Exception):
                    session.close()

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
            rows = apply_projection_batch(rows, projection, filter)
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

    @staticmethod
    def _without_credentials(record: dict[str, Any]) -> dict[str, Any]:
        """Return ``record`` minus its SCRAM ``credentials`` blob.

        The generic CRUD read path onto ``admin.system.users`` is
        reachable with only the ordinary collection-read action
        (``A_FIND``), but the SCRAM ``storedKey`` / ``serverKey`` / salt /
        iteration-count is the sensitive artifact — the ``/etc/shadow``
        equivalent that enables offline cracking and server
        impersonation. ``usersInfo`` gates it behind ``A_VIEW_USER`` +
        ``showCredentials``; the generic ``find`` / ``count`` /
        ``aggregate`` view must never surface it. See issue #167.
        """
        if "credentials" not in record:
            return record
        stripped = dict(record)
        stripped.pop("credentials", None)
        return stripped

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
        carry the mongod-shaped fields (``_id`` = ``<db>.<user>``,
        ``user``, ``db``, ``roles``, ``mechanisms``), so the view is the
        row set plus the usual filter / sort / skip / limit / projection
        pipeline — but with the SCRAM ``credentials`` blob stripped
        first (see :meth:`_without_credentials`), so it is never returned
        and can't be used as a filter match-oracle."""
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        rows = [self._without_credentials(r) for r in self._scan_user_records()]
        if filter:
            rows = [r for r in rows if matches(r, filter, vars=let, collation=collation_obj)]
        if sort:
            rows = sort_docs(rows, sort)
        if skip:
            rows = rows[skip:]
        if limit > 0:
            rows = rows[:limit]
        if projection:
            rows = apply_projection_batch(rows, projection, filter)
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
        # Strip credentials before counting too, so a filter on
        # ``credentials.*`` can't be used as a match-oracle (see #167).
        rows = [self._without_credentials(r) for r in self._scan_user_records()]
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
            rows = apply_projection_batch(rows, projection, filter)
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

    @contextlib.contextmanager
    def replay_mode(self) -> Iterator[None]:
        """Suppress oplog emission on the calling thread for the duration.

        Point-in-time recovery replays an existing oplog into a fresh store by
        driving the ordinary write paths (insert / update / delete / DDL) so
        the documents, indexes, and natural order are rebuilt exactly as they
        were produced live. Those paths normally append to the oplog and mint
        fresh timestamps — wrong here, because the oplog is the *input*, not
        something to regenerate. Inside this context ``_emit_oplog`` is a
        no-op. See :mod:`secantus.oplog_replay`.
        """
        prev = getattr(self._tls, "replay_silent", False)
        self._tls.replay_silent = True
        try:
            yield
        finally:
            self._tls.replay_silent = prev

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
        if getattr(self._tls, "replay_silent", False):
            # Oplog replay (PITR) drives the real write paths for their
            # storage / index / natural-order effects but must not
            # regenerate the oplog it is replaying. See ``replay_mode``.
            return 0
        handle = getattr(self._tls, "user_txn", None)
        if handle is not None:
            handle.dirty_bytes += sum(len(bson.encode(e)) for e in entries)
            if 2 * handle.dirty_bytes > self._txn_dirty_limit:
                # The statement's partial writes roll back with the
                # transaction (any failed in-txn statement aborts it
                # server-side, mongod parity).
                raise TransactionTooLargeError(
                    "Transaction is too large and will not fit in the storage engine cache"
                )
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
        # Whose commit resolves this mint? Inside a batch transaction (or the
        # user-txn commit flush, which sets the same flag) the rows commit
        # later — park the range for the transaction's resolution point to
        # deregister. Outside (a bare emit whose cursor writes autocommit)
        # the range deregisters at the end of this method.
        deferred = getattr(self._tls, "defer_minted", False)
        if deferred:
            pending = getattr(self._tls, "pending_minted", None)
            if pending is None:
                pending = self._tls.pending_minted = []
            pending.append((start_seq, start_seq + n))
        # Mint-to-deregister must be exception-safe on the bare autocommit
        # path: if the cursor-write loop or the opportunistic prune raises (a
        # WT write error / WT_ROLLBACK under contention — expected, not
        # exotic), the minted range must still leave ``_oplog_in_flight``.
        # Otherwise ``oplog_visible_tail_seq`` clamps at that seq for the life
        # of the process and change streams server-wide silently freeze — a
        # self-inflicted DoS (#714). A deferred emit's range is owned by its
        # transaction's commit/rollback, so it is NOT deregistered here.
        last_seq = 0
        try:
            op_cur = self._cursor(_OPLOG_TABLE)
            pre_cur = None
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
            self._oplog_live_count += len(entries)
            self._oplog_emit_count += len(entries)
            if self._oplog_emit_count >= _OPLOG_PRUNE_INTERVAL:
                self._oplog_emit_count = 0
                self._prune_oplog_locked(now=self._time())
        finally:
            if not deferred:
                # Bare autocommit emit: the cursor writes above committed on
                # their own, so the minted range resolves here (deregister +
                # notify) even if the body raised. A deferred emit's range
                # resolves at its transaction's commit/rollback instead.
                self._deregister_minted([(start_seq, start_seq + n)])
            with self._oplog_cv:
                self._oplog_cv.notify_all()
        return last_seq

    @staticmethod
    def _merge_oplog_on_session(
        session: Any,
        start_seq: int,
        limit: int,
        ns_filter: Callable[[str], bool] | None = None,
    ) -> list[tuple[int, dict[str, Any]]]:
        """K-way merge across the oplog shards + the legacy table on ``session``,
        yielding ``(seq, entry)`` in ascending seq order from the first seq >=
        ``start_seq``, up to ``limit`` non-empty entries.

        Each table is seq-sorted; the merge repeatedly emits the smallest head
        seq and advances that cursor. Handles gaps (prune removes a low prefix)
        so a missing seq can't truncate a change-stream read. Mirrors the Rust
        ``read_oplog_shards``. Opens + closes its own cursors on ``session``.

        Also reports ``scan_high``: the highest seq this scan actually EXAMINED,
        including entries the ``ns_filter`` rejected. A caller that wants to skip
        its cursor forward past uninteresting activity must bound the skip by
        this, not by the oplog's tail — the tail is the highest seq *minted*, and
        a writer mints before it commits, so the tail can name an entry that no
        reader can see yet. Skipping to it would step over that entry for good.
        """
        out: list[tuple[int, dict[str, Any]]] = []
        scan_high = start_seq - 1
        cursors: list[Any] = []
        heads: list[int | None] = []
        try:
            for table in _OPLOG_ALL_TABLES:
                try:
                    c = session.open_cursor(table, None)
                except Exception as exc:
                    if _is_missing_table(exc):
                        continue  # lazy shards: absent oplog shard reads as empty
                    raise
                cursors.append(c)
                c.set_key(int(start_seq))
                rc = c.search_near()
                if rc == wt.WT_NOTFOUND:
                    heads.append(None)
                elif rc < 0:
                    heads.append(int(c.get_key()) if c.next() == 0 else None)
                else:
                    heads.append(int(c.get_key()))
            while len(out) < limit:
                best_i = None
                best_seq = None
                for i, h in enumerate(heads):
                    if h is not None and (best_seq is None or h < best_seq):
                        best_seq = h
                        best_i = i
                if best_i is None:
                    break
                c = cursors[best_i]
                blob = bytes(c.get_value())
                if blob:
                    entry = bson.decode(blob)
                    if ns_filter is None or ns_filter(str(entry.get("ns", ""))):
                        out.append((best_seq, entry))
                scan_high = max(scan_high, best_seq)
                heads[best_i] = int(c.get_key()) if c.next() == 0 else None
        finally:
            for c in cursors:
                with contextlib.suppress(Exception):
                    c.close()
        return out, scan_high

    def read_oplog(
        self,
        *,
        start_seq: int,
        limit: int,
        ns_filter: Callable[[str], bool] | None = None,
    ) -> list[tuple[int, dict[str, Any]]]:
        """Forward-scan the oplog from ``start_seq`` (inclusive), merging shards.

        Uses a private short-lived session so the read view always reflects
        rows committed by other sessions. The cached per-thread session's
        snapshot is sticky — under WiredTiger's MVCC, reusing it across
        getMore polls would never observe oplog rows produced by a writer
        running on a different connection thread.
        """
        return self.read_oplog_scan(start_seq=start_seq, limit=limit, ns_filter=ns_filter)[0]

    def read_oplog_scan(
        self,
        *,
        start_seq: int,
        limit: int,
        ns_filter: Callable[[str], bool] | None = None,
    ) -> tuple[list[tuple[int, dict[str, Any]]], int]:
        """``read_oplog`` plus the highest seq the scan examined.

        The second element is what a change-stream cursor may safely skip to when
        the scan produced no matching events: entries this read actually saw and
        rejected. Bounding the skip by the oplog *tail* instead loses events —
        the tail counts minted seqs, and an entry minted but not yet committed is
        invisible to this scan, so skipping to the tail steps over it forever.

        Both the rows and ``scan_high`` are additionally clamped at the
        **visible tail** (the in-flight window's floor): a committed row past a
        still-in-flight lower mint is real data, but serving it — or letting
        ``scan_high`` pass it — would advance a change-stream position over the
        hole, permanently losing the in-flight event when its transaction
        commits (the same minted-vs-committed race the Rust server's
        visibility point closed; per-collection-locked writers commit out of
        mint order across collections).
        """
        # Read the bound BEFORE opening the read session: commit →
        # deregister → this read → session open, so the session's snapshot
        # necessarily contains every seq <= the bound.
        max_seq = self.oplog_visible_tail_seq()
        if start_seq > max_seq:
            return [], start_seq - 1
        with self._lock:
            if self._closed:
                return [], start_seq - 1
            session = self._conn.open_session()
            try:
                rows, scan_high = self._merge_oplog_on_session(session, start_seq, limit, ns_filter)
                if rows and rows[-1][0] > max_seq:
                    rows = [r for r in rows if r[0] <= max_seq]
                return rows, min(scan_high, max_seq)
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def read_preimage(self, seq: int) -> dict[str, Any] | None:
        """Return the pre-image doc for ``seq`` if one was stored, else ``None``.

        Uses a private session for cross-thread visibility (see ``read_oplog``).
        """
        with self._lock:
            if self._closed:
                return None
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

    def import_oplog_segment(
        self,
        rows: Iterable[tuple[int, Mapping[str, Any]]],
        pre_images: Mapping[int, Mapping[str, Any]] | None = None,
    ) -> int:
        """Write **verbatim** oplog rows ``(seq, entry)`` into the oplog table,
        carrying their original seq / ts / wall, and advance the seq + cluster
        clock past them so subsequent live writes mint strictly-greater values.

        This is the seam point-in-time recovery uses to preserve the source's
        oplog timeline on the restored store (``carry_oplog`` /
        ``--preserve-oplog``): a change stream on the restored server can then
        resume from a token minted *before* the restore point, because the rows
        that token references are present. ``pre_images`` maps seq -> pre-image
        document for the seqs that had one stored.

        Unlike ``_emit_oplog`` this does NOT mint new seqs — the rows keep their
        identity, so resume tokens stay valid. Returns the highest seq written
        (0 if ``rows`` is empty). No-op when ``enable_oplog`` is False.
        """
        if not self.enable_oplog:
            return 0
        pre_images = pre_images or {}
        max_seq = 0
        best_secs = 0
        best_ord = 0
        with self._lock:
            op_cur = self._cursor(_OPLOG_TABLE)
            pre_cur = None
            wrote = False
            for seq, entry in rows:
                wrote = True
                iseq = int(seq)
                op_cur[iseq] = bson.encode(dict(entry))
                pre = pre_images.get(iseq)
                if pre is not None:
                    if pre_cur is None:
                        pre_cur = self._cursor(_PREIMAGE_TABLE)
                    pre_cur[iseq] = bson.encode(dict(pre))
                if iseq > max_seq:
                    max_seq = iseq
                ts = entry.get("ts")
                if isinstance(ts, Timestamp) and (ts.time, ts.inc) > (best_secs, best_ord):
                    best_secs, best_ord = ts.time, ts.inc
            if not wrote:
                return 0
            with self._oplog_seq_lock:
                if max_seq + 1 > self._next_seq:
                    self._next_seq = max_seq + 1
                if (best_secs, best_ord) > (self._last_ts_secs, self._last_ts_ord):
                    self._last_ts_secs = best_secs
                    self._last_ts_ord = best_ord
            self._persist_oplog_meta()
        with self._oplog_cv:
            self._oplog_cv.notify_all()
        return max_seq

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
            if self._closed:
                return 0
            session = self._conn.open_session()
            try:
                # Sharded: the global floor is the smallest first-key across all
                # shards + the legacy table. Each table's ``next()`` from the start
                # lands on its minimum.
                floor: int | None = None
                for table in _OPLOG_ALL_TABLES:
                    try:
                        c = session.open_cursor(table, None)
                    except Exception as exc:
                        if _is_missing_table(exc):
                            continue  # lazy shards: absent oplog shard is empty
                        raise
                    try:
                        if c.next() == 0:
                            seq = int(c.get_key())
                            floor = seq if floor is None else min(floor, seq)
                    finally:
                        with contextlib.suppress(Exception):
                            c.close()
                return floor or 0
            finally:
                with contextlib.suppress(Exception):
                    session.close()

    def find_seq_for_ts(self, ts: Timestamp, *, max_wait_seconds: float = 0.5) -> int:
        """Smallest seq whose entry ``ts >= target``. Tail+1 if none qualify.

        The committed-view scan can name a seq above a minted-but-uncommitted
        entry whose ts also qualifies (ts is minted monotonically with seq) —
        a ``startAtOperationTime`` position finalised there would permanently
        skip that entry when its transaction commits. So the answer is
        accepted only once the visible tail covers it (no in-flight seq can
        then exist below it); otherwise wait briefly for the window to drain
        and rescan. Batch transactions resolve in microseconds; a long-open
        user transaction hits the bounded deadline and falls back to the
        committed-view answer — the pre-fix behaviour. Twin of the Rust
        server's bounded wait.

        ``max_wait_seconds`` is that bound. It is a parameter only so tests can
        widen it.

        The visible tail is sampled BEFORE the scan, and the order is load
        bearing. Sampling it after left a window in which an in-flight mint
        committed between the two reads: the scan still returned the answer
        from before the commit (naming the seq *above* the in-flight one),
        while the tail read afterwards had already advanced to cover it, so
        the stale answer passed the check and the entry was skipped for good.
        Sampling first is conservative in the safe direction — the tail only
        ever grows, so an earlier reading is no larger than the true one at
        scan time, and everything at or below it is resolved (committed or a
        permanent hole) and therefore visible to the scan that follows.
        """
        deadline = _time.monotonic() + max_wait_seconds
        while True:
            vis = self.oplog_visible_tail_seq()
            r = self._find_seq_for_ts_scan(ts)
            if r - 1 <= vis or _time.monotonic() >= deadline:
                return r
            with self._oplog_cv:
                self._oplog_cv.wait(0.05)

    def _find_seq_for_ts_scan(self, ts: Timestamp) -> int:
        """One committed-view scan for ``find_seq_for_ts``. Uses a private
        session for cross-thread visibility. Sharded: ts is monotone in the
        *global* seq order, so the shard merge yields entries in ts order —
        the first one at/after ``target`` is the answer."""
        with self._lock:
            if self._closed:
                return 0
            session = self._conn.open_session()
            try:
                rows, _scan_high = self._merge_oplog_on_session(session, 0, 2**63 - 1)
                for seq, entry in rows:
                    entry_ts = entry.get("ts")
                    if isinstance(entry_ts, Timestamp) and (
                        entry_ts.time > ts.time
                        or (entry_ts.time == ts.time and entry_ts.inc >= ts.inc)
                    ):
                        return seq
                return self._next_seq
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
        # Both prune criteria act on the OLDEST entries only:
        #   * time retention — seq is minted jointly-monotonic with ts (see
        #     ``_mint_oplog_seq_and_ts``), so every entry older than the cutoff
        #     is an oldest-prefix by seq; the first entry we reach that is
        #     within retention ends the time-doom.
        #   * entry cap — trims the oldest surplus (``live_count - max``).
        # So we stream the oldest entries in seq order and stop at the first one
        # that neither criterion dooms: a prune that deletes D rows reads ~D+1
        # entries, never the whole oplog. ``_oplog_live_count`` (maintained on
        # every emit) gives the cap decision without a counting scan.
        live = self._oplog_live_count
        doomed: list[tuple[int, str]] = []
        scan_session = self._conn.open_session()
        try:
            for seq, entry, table in self._iter_oplog_oldest(scan_session):
                ts = entry.get("ts")
                too_old = isinstance(ts, Timestamp) and ts.time < cutoff_secs
                over_cap = (live - len(doomed)) > self.oplog_max_entries
                if too_old or over_cap:
                    doomed.append((seq, table))
                else:
                    break
        finally:
            with contextlib.suppress(Exception):
                scan_session.close()
        if not doomed:
            return 0
        self._oplog_live_count = live - len(doomed)
        # PITR v2: archive the doomed rows to a durable segment *before* deleting
        # them, so recovery can still reach a time before the new oplog floor.
        if self.oplog_archive_dir is not None:
            self._archive_doomed_oplog(sorted(seq for seq, _ in doomed))
        # The oldest-first walk already told us which table each doomed seq lives
        # in, so we delete straight from that one table (not all 17). Pre-images
        # live in a single table keyed by the same seq.
        pre_del = self._cursor(_PREIMAGE_TABLE)
        for seq, table in doomed:
            cur = self._cursor(table)
            cur.set_key(seq)
            with contextlib.suppress(wt.WiredTigerError):
                cur.remove()
            cur.reset()
            pre_del.set_key(seq)
            with contextlib.suppress(wt.WiredTigerError):
                pre_del.remove()
            pre_del.reset()
        return len(doomed)

    def _archive_doomed_oplog(self, doomed_sorted: list[int]) -> None:
        """Write the soon-to-be-pruned oplog rows (and their pre-images) into a
        durable segment in ``oplog_archive_dir``. Called under ``self._lock``
        from ``_prune_oplog_locked`` before the rows are deleted."""
        from . import pitr_archive

        pre_cur = self._cursor(_PREIMAGE_TABLE)
        rows: list[tuple[int, dict[str, Any], dict[str, Any] | None]] = []
        for seq in doomed_sorted:
            # Routing is per-batch, so a doomed seq's table isn't a function of the
            # seq — probe every table (all shards + legacy) for the row.
            blob = None
            for table in _OPLOG_ALL_TABLES:
                op_cur = self._cursor_optional(table)
                if op_cur is None:
                    continue  # lazy shards: absent oplog shard reads as empty
                op_cur.set_key(seq)
                if op_cur.search() == 0:
                    blob = bytes(op_cur.get_value())
                    op_cur.reset()
                    break
                op_cur.reset()
            if blob is None:
                continue
            entry = bson.decode(blob)
            pre = None
            pre_cur.set_key(seq)
            if pre_cur.search() == 0:
                pre_blob = bytes(pre_cur.get_value())
                if pre_blob:
                    pre = bson.decode(pre_blob)
            pre_cur.reset()
            rows.append((seq, entry, pre))
        pitr_archive.write_segment(self.oplog_archive_dir, rows)

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

    def __enter__(self) -> Storage:
        """Support ``with Storage(path) as store:`` — the block's exit calls
        ``close()`` so WiredTiger is torn down (threads joined, oplog meta
        persisted, connection closed) even if the body raises. ``close()`` is
        idempotent, so an explicit ``close()`` inside the block is still safe."""
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: object,
    ) -> None:
        self.close()

    @property
    def in_memory(self) -> bool:
        """True when this store is the ``:memory:`` (non-persistent) variant."""
        return self._in_memory

    def close(self) -> None:
        # Stop background threads before tearing down WT — both the
        # TTL sweeper and the noop heartbeat acquire ``self._lock``,
        # so racing them against close would deadlock or
        # use-after-close.
        # Join the background threads to completion — NOT with a short timeout —
        # before any WiredTiger teardown below. Each sweeper owns a thread-local
        # WT session (registered in ``_all_sessions``); if a bounded join gives up
        # while a sweeper is still alive, close() then closes that session from
        # THIS (foreign) thread and calls ``conn.close()`` while the sweeper still
        # references it. A WT session is not safe to close cross-thread while its
        # owner lives, and ``WT_CONNECTION->close`` then blocks forever waiting to
        # quiesce it — the macOS shutdown wedge (worker hung in ``conn.close``,
        # SIGKILLed with no diagnostics; load, e.g. the rust-server suite or
        # mimalloc, made the old 2s join miss often enough to cascade). The joins
        # run BEFORE ``with self._lock`` so a sweeper mid-iteration can take the
        # lock, finish, drop its own session (see the loops' ``finally``), and
        # exit; the stop event wakes a parked sweeper immediately, so this is
        # bounded by at most one in-flight iteration in practice.
        self._ttl_stop.set()
        if self._ttl_thread is not None and self._ttl_thread.is_alive():
            self._ttl_thread.join()
            self._ttl_thread = None
        self._noop_stop.set()
        if self._noop_thread is not None and self._noop_thread.is_alive():
            self._noop_thread.join()
            self._noop_thread = None
        import logging

        log = logging.getLogger("secantus.storage.close")
        with self._lock:
            if self._closed:
                return
            self._closed = True
            # Persist the oplog meta one last time. We dropped the
            # per-emit persist in Phase 2.4 (it caused WT-rollback
            # storms under concurrent writers), so this is the
            # canonical place to write the in-memory ``_next_seq``
            # and timestamp counters down to disk before shutdown.
            #
            # Teardown continues past any single failure so close()
            # stays idempotent and releases as many resources as it
            # can — but every failure is *logged*, never swallowed
            # silently. In a database a checkpoint or connection-close
            # error is a durability signal (see CLAUDE.md "Never ignore
            # an error"), so the embedder gets a trace telling them the
            # last durable image may be incomplete.
            try:
                self._persist_oplog_meta()
            except Exception:
                log.exception("failed to persist oplog meta during close")
            # Force a checkpoint before tearing the connection down.
            # ``WT_CONNECTION->close`` does this implicitly, but only
            # when logging is off (or hits the connection's
            # close-time flush window). Driving it explicitly here
            # gives a durable on-disk image of the dataset at the
            # moment of shutdown regardless of journal state — the
            # behaviour callers reasonably expect from ``close()``.
            # Skip for in-memory backends: WT's in_memory engine
            # rejects checkpoint() with a noisy stderr log
            # (``__wt_inmem_unsupported_op``) on every call. Also skip when
            # ``durable=False`` (fast test mode): the journal is off and the
            # storage dir is discarded, so there is nothing to make durable —
            # this is where the ~5x open/close saving comes from (no fsync).
            if not self._in_memory and self._durable:
                # Use a dedicated session opened directly on the connection —
                # NOT ``self._session()`` — because ``_closed`` is already True
                # and ``_session()`` now refuses to open on a closed store.
                try:
                    ck = self._conn.open_session()
                    try:
                        ck.checkpoint()
                    finally:
                        ck.close()
                except Exception:
                    log.exception("final checkpoint failed during close")
            for s in self._all_sessions:
                try:
                    s.close()
                except Exception:
                    log.exception("WT session close failed during close")
            self._all_sessions.clear()
            try:
                self._conn.close()
            except Exception:
                log.exception("WT connection close failed during close")
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
        try:
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
        finally:
            # Close this thread's WT session on the OWNING thread before the
            # thread dies. Between iterations the loop parks in ``wait()`` still
            # holding the session it opened, and on exit it would otherwise leave
            # it open for Storage.close() to close cross-thread — which hangs
            # ``WT_CONNECTION->close``. ``_reset_thread_session`` is a no-op once
            # ``_closed`` is set (close() already owns the sessions then).
            self._reset_thread_session()

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
        try:
            while not self._noop_stop.wait(self._noop_heartbeat_seconds):
                if self._closed:
                    return
                try:
                    self.emit_noop_heartbeat()
                except Exception:
                    log.exception("noop heartbeat failed")
        finally:
            # Drop this thread's WT session on the owning thread before exit —
            # same rationale as the TTL sweeper: never leave a session open for
            # Storage.close() to tear down cross-thread. See close()'s join note.
            self._reset_thread_session()

    def release_thread_snapshot(self) -> None:
        """Release the calling thread's WT read snapshot and cursor positions.

        Call at the END of every request/statement (both wire servers do).
        ``_refresh_read_snapshot`` releases a stale snapshot at the *start* of
        the next read — but a connection that goes idle after its last
        statement never reaches that point, and a cached session left with a
        positioned cursor holds an implicit transaction that pins WiredTiger's
        oldest-transaction horizon indefinitely. Every write after that pin
        keeps its history unevictable, so per-operation cost grows linearly
        with churn until page reads stall the whole server (the pgjdbc gauge's
        CopyLargeFileTest wedge: one idle connection's pinned snapshot turned a
        4-minute test into a 2-hour lane timeout). ``WT_SESSION.reset()``
        releases the snapshot and resets every cursor position in one call;
        cached cursor handles stay valid (``_cursor`` re-``reset()``s before
        each reuse).

        No-op inside a user transaction — its pinned snapshot is the
        transaction's semantics, bounded by the servers' transaction-lifetime
        / idle-in-transaction timeouts."""
        if getattr(self._tls, "user_txn", None) is not None:
            return
        s = getattr(self._tls, "session", None)
        if s is None:
            return
        with self._lock:
            if not self._closed:
                with contextlib.suppress(Exception):
                    s.reset()

    def _reset_thread_session(self) -> None:
        """Close the calling thread's cached WT session + cursors so
        the next ``_session()`` call opens fresh ones. Needed when a
        thread reads in a loop and must observe writes from other
        threads (snapshot is otherwise sticky)."""
        s = getattr(self._tls, "session", None)
        if s is None:
            return
        cursors = getattr(self._tls, "cursors", {}) or {}
        # Close the cursors + session UNDER THE LOCK and only while the store is
        # open. Without the lock this races Storage.close()'s ``_all_sessions``
        # teardown → double-close of the same WT session → use-after-free
        # segfault. When ``_closed`` is set, close() has already closed every
        # session in ``_all_sessions`` (including this one), so we must not touch
        # WT — just drop the thread-local references.
        with self._lock:
            if not self._closed:
                for c in cursors.values():
                    with contextlib.suppress(Exception):
                        c.close()
                with contextlib.suppress(Exception):
                    s.close()
                with contextlib.suppress(ValueError):
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
        import io
        import json
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
            # Advisory PITR metadata describing the oplog range this archive
            # can recover to. Computed under the lock, before the backup
            # cursor opens; embedded in the tar (WiredTiger ignores it).
            manifest = self._pitr_manifest()
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
                        data = json.dumps(manifest, default=str).encode()
                        info = tarfile.TarInfo(name=_PITR_MANIFEST_NAME)
                        info.size = len(data)
                        tar.addfile(info, io.BytesIO(data))
                finally:
                    cursor.close()
            finally:
                backup_session.close()
        return {"path": abs_out, "sizeBytes": os.path.getsize(abs_out)}

    def archive_base_snapshot(self, archive_dir: str) -> dict[str, int | str]:
        """Take a base snapshot into ``archive_dir`` for PITR v2, named by its
        oplog head seq (``base-<head>.tar.gz``) so the restore path can order and
        select snapshots. Thin wrapper over :meth:`create_archive`.

        Pair with ``oplog_archive_dir=<archive_dir>`` (so pruned oplog rows are
        archived as segments into the same directory) and call this periodically
        — there is no background scheduler, matching ``prune_ttl`` / ``prune_oplog``.
        """
        from . import pitr_archive

        with self._lock:
            head = self.oplog_visible_tail_seq_nolock()
        out = os.path.join(archive_dir, pitr_archive.base_name(head))
        result = self.create_archive(out)
        result["headSeq"] = head
        return result

    def _pitr_manifest(self) -> dict[str, Any]:
        """Build the point-in-time-recovery manifest embedded in a backup
        archive: the oplog seq range and timestamps it can recover to, plus
        whether the oplog still reaches genesis (an un-pruned front, which v1
        empty-base replay requires). Advisory only — restore reads the oplog
        directly; the manifest lets tooling report a backup's recoverable
        range without opening WiredTiger. Must be called under ``self._lock``."""
        floor = self.oplog_floor_seq()
        head = self.oplog_visible_tail_seq_nolock()

        def _row(seq: int) -> dict[str, Any] | None:
            if seq <= 0:
                return None
            rows = self.read_oplog(start_seq=seq, limit=1)
            return rows[0][1] if rows else None

        def _ts_of(row: dict[str, Any] | None) -> list[int] | None:
            ts = row.get("ts") if row else None
            return [ts.time, ts.inc] if isinstance(ts, Timestamp) else None

        def _wall_of(row: dict[str, Any] | None) -> str | None:
            wall = row.get("wall") if row else None
            return wall.isoformat() if isinstance(wall, _dt.datetime) else None

        floor_row = _row(floor)
        head_row = _row(head)
        return {
            "secantusPitrManifest": 1,
            "oplogEnabled": bool(self.enable_oplog),
            "oplogFloorSeq": floor,
            "oplogHeadSeq": head,
            "genesisIntact": floor == 1,
            "oplogFloorTs": _ts_of(floor_row),
            "oplogHeadTs": _ts_of(head_row),
            "oplogFloorWall": _wall_of(floor_row),
            "oplogHeadWall": _wall_of(head_row),
        }

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
        # Emits inside this transaction park their minted seq ranges on
        # ``_tls.pending_minted``; the ``finally`` deregisters them from the
        # in-flight window on EVERY exit — after the commit (rows visible:
        # the visible tail may advance and tailable waiters wake) and after
        # a rollback (rows can never appear: the abandoned range must not
        # pin the tail forever).
        prev_defer = getattr(self._tls, "defer_minted", False)
        self._tls.defer_minted = True
        try:
            yield session
        except Exception:
            with contextlib.suppress(Exception):
                session.rollback_transaction()
            raise
        else:
            _commit_batch_transaction(session, sync)
        finally:
            self._tls.defer_minted = prev_defer
            self._deregister_minted(self._drain_pending_minted())

    def _session(self) -> Any:
        s = getattr(self._tls, "session", None)
        if s is None:
            # Open the session UNDER THE LOCK, and refuse if the store is
            # closed. Without this fence, a connection thread opening its first
            # session races Storage.close()'s ``conn.close()`` — ``open_session``
            # on a torn-down connection is a use-after-free (segfault). ``_lock``
            # is an RLock, so callers that already hold it (public methods) are
            # unaffected.
            with self._lock:
                if self._closed:
                    raise RuntimeError("Storage is closed")
                s = self._conn.open_session()
                self._tls.session = s
                self._tls.cursors = {}
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

    @contextlib.contextmanager
    def _committed_read_scope(self) -> Iterator[None]:
        """Run reads on this thread against the latest COMMITTED state.

        Swaps a fresh session (and its own cursor cache) into ``_tls`` for the
        duration, with ``user_txn`` cleared, so every existing read path
        transparently sees committed data instead of the transaction's pinned
        snapshot. READ-ONLY: a write inside this scope would land outside the
        caller's transaction and escape its rollback.
        """
        tls = self._tls
        saved = (
            getattr(tls, "session", None),
            getattr(tls, "cursors", None),
            getattr(tls, "user_txn", None),
        )
        with self._lock:
            if self._closed:
                raise RuntimeError("Storage is closed")
            session = self._conn.open_session()
        tls.session, tls.cursors, tls.user_txn = session, {}, None
        try:
            yield
        finally:
            for cur in list(tls.cursors.values()):
                with contextlib.suppress(Exception):
                    cur.close()
            tls.session, tls.cursors, tls.user_txn = saved
            with contextlib.suppress(Exception):
                session.close()

    def _note_write(self, db: str, coll: str) -> None:
        """Record that the in-flight user transaction has written to this
        collection, which disqualifies the committed-state probe for it (see
        ``find_matching_committed``)."""
        txn = getattr(self._tls, "user_txn", None)
        if txn is not None:
            txn.written.add((db, coll))

    def find_matching_committed(
        self, db: str, coll: str, filter: dict[str, Any] | None = None, *, limit: int = 0
    ) -> list[dict[str, Any]]:
        """``find_matching`` against the latest COMMITTED state, ignoring a user
        transaction's pinned snapshot.

        For CONSTRAINT ENFORCEMENT only, not for user-visible reads — those must
        keep the transaction's view. A uniqueness probe is not an ordinary read:
        Postgres pins your read snapshot too, yet still checks a unique index
        against committed data, so a value another transaction committed after
        your snapshot conflicts. Probing through the snapshot instead let the
        duplicate through and stored it.

        Outside a user transaction this is plain ``find_matching`` — the
        session's snapshot is already refreshed per read — so the common path
        costs nothing extra.

        Returns nothing once the transaction has WRITTEN to the collection: the
        committed view of a row this transaction has deleted or rewritten is
        stale, and reporting it would reject a value the transaction has
        legitimately freed (delete-then-reinsert inside one transaction is
        valid, and Postgres allows it). The caller's own snapshot probe still
        covers everything visible to the transaction; what is given up is
        catching a *late* outside commit in a transaction that has already
        written to the same table.
        """
        txn = getattr(self._tls, "user_txn", None)
        if txn is None:
            return self.find_matching(db, coll, filter, limit=limit)
        if (db, coll) in txn.written:
            return []
        with self._committed_read_scope():
            return self.find_matching(db, coll, filter, limit=limit)

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
            # Save/restore rather than clear: a nested entry (a scalar function
            # like ``lo_creat`` running storage ops inside a statement that is
            # itself inside the transaction) must not strip the outer entry's
            # marker — that made the enclosing INSERT run ``_batch_transaction``
            # against the transaction's session and hit WT's "begin_transaction
            # not permitted in a running transaction".
            prev_user_txn = getattr(self._tls, "user_txn", None)
            self._tls.user_txn = handle
            try:
                yield
            finally:
                self._tls.user_txn = prev_user_txn

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
                    # transaction's session instead of re-buffering. The
                    # flush's minted range is deferred (rows are not visible
                    # until ``commit_transaction`` below) and deregistered in
                    # the ``finally`` — after the commit on success, and on
                    # the exception path before ``abort_user_transaction``
                    # rolls back, so a failed commit can't pin the visible
                    # tail on a corpse.
                    prev_defer = getattr(self._tls, "defer_minted", False)
                    self._tls.defer_minted = True
                    try:
                        if entries:
                            last_seq = self._emit_oplog(entries, pre_images)
                        handle.session.commit_transaction()
                    finally:
                        self._tls.defer_minted = prev_defer
                        self._deregister_minted(self._drain_pending_minted())
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

    def _cursor_optional(self, table: str, *, overwrite: bool = True) -> Any | None:
        """Like :meth:`_cursor` but returns ``None`` if ``table`` doesn't exist.

        Under lazy shard creation a documents / oplog shard table is present only
        once written to, so read / scan / merge paths use this to treat an absent
        shard as an empty one instead of erroring."""
        try:
            return self._cursor(table, overwrite=overwrite)
        except Exception as exc:
            if _is_missing_table(exc):
                return None
            raise

    def _ensure_doc_shard(self, db: str, coll: str) -> None:
        """Create ``db.coll``'s documents shard table if it isn't there yet.

        Called on the write path (collection create / auto-create) so the shard a
        collection routes to exists before any doc is written to it. Idempotent
        and tracked per-instance, so it costs one WT ``create`` per distinct
        shard for this store's lifetime (WT ``create`` preserves an existing
        table, so a reopened on-disk store's shards are re-adopted harmlessly)."""
        name = _doc_table_for(db, coll)
        if name in self._created_doc_shards:
            return
        self._session()
        self._tls.session.create(name, _DOC_SHARD_CFG)
        self._created_doc_shards.add(name)

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
        # New collection → make its documents shard now (lazy shard creation).
        self._ensure_doc_shard(db, coll)

    def collection_exists(self, db: str, coll: str) -> bool:
        with self._lock:
            return self._coll_options(db, coll) is not None

    def create_collection(
        self, db: str, coll: str, options: Mapping[str, Any] | None = None
    ) -> bool:
        """Create ``db.coll`` (no-op-False if it already exists).

        ``options`` is the collection-options blob (``capped`` / ``size`` /
        ``max`` / ``validator`` / ``viewOn`` / … — everything the ``create``
        command persists). It is written to the options blob *and* carried as
        siblings of ``create`` in the ``c`` oplog entry's ``o``, so PITR replay
        and ``show_expanded_events`` create events reconstruct the options
        rather than seeing a bare ``{create, idIndex}``.
        """
        with self._lock:
            c = self._cursor(_COLL_TABLE)
            c.set_key(db, coll)
            if c.search() == 0:
                return False
            c.reset()
            c[db, coll] = b""
            # New collection → make its documents shard now (lazy shard creation).
            self._ensure_doc_shard(db, coll)
            if options:
                # Persist before minting the UUID — ``_collection_uuid``'s
                # mint path re-reads and merges, so the options survive.
                self._write_coll_options(db, coll, dict(options))
            ui = self._collection_uuid(db, coll)  # mint and persist
            o: dict[str, Any] = {"create": coll}
            if options:
                o.update(options)
            o["idIndex"] = {"v": 2, "key": {"_id": 1}, "name": "_id_"}
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": o,
                    }
                ]
            )
            return True

    def _scan_docs(self, db: str, coll: str) -> Iterable[tuple[int, bytes, bytes]]:
        """Yield ``(recordid, id_key, blob)`` for every doc in ``(db, coll)``, in
        natural (insertion) order.

        The doc table is keyed by the monotonic RecordId so a forward walk IS
        insertion order; the ``id_key`` and blob come from unframing each value
        (see ``_frame_doc_value``) — no ``_id`` decode needed, and it recovers a
        timeseries doc's non-derivable suffixed ``id_key`` too. Mirrors the Rust
        ``scan_docs``.
        """
        c = self._cursor_optional(_doc_table_for(db, coll))
        if c is None:
            # Lazy shard creation: the collection's shard was never written
            # (empty / never-existed collection) → no docs.
            return
        c.set_key(db, coll, _INT64_MIN)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return
        if rc < 0 and c.next() != 0:
            return
        while True:
            k = c.get_key()
            if k[0] != db or k[1] != coll:
                return
            id_key, blob = _unframe_doc_value(bytes(c.get_value()))
            yield int(k[2]), id_key, blob
            if c.next() != 0:
                return

    def _all_docs(self, db: str, coll: str) -> list[dict[str, Any]]:
        # Two-stage to keep ``bson.decode`` out of ``self._lock`` —
        # otherwise an N-doc scan blocks every other thread for the
        # whole decode loop. Lock owns the WT cursor walk; decode
        # happens after release.
        with self._lock:
            blobs = [blob for _rid, _id_k, blob in self._scan_docs(db, coll)]
        return [bson.decode(blob) for blob in blobs]

    def _all_docs_with_id_key(self, db: str, coll: str) -> list[tuple[dict[str, Any], bytes]]:
        with self._lock:
            raw = [(id_k, blob) for _rid, id_k, blob in self._scan_docs(db, coll)]
        return [(bson.decode(blob), id_k) for id_k, blob in raw]

    def scan_docs_after_recordid(
        self, db: str, coll: str, after: int | None
    ) -> list[tuple[int, dict[str, Any]]]:
        """Scan the collection in natural (insertion) order, returning only rows
        whose **RecordId** is strictly greater than ``after``. ``after`` of
        ``None`` returns the entire collection.

        Used by the tailable-cursor producer to emit only the docs inserted since
        the last poll. Returns ``[(recordid, doc), ...]`` — callers update their
        ``after`` checkpoint to the last returned RecordId for the next poll.
        RecordId is the right basis: it is what mongod's tailable cursors follow
        and what capped FIFO eviction uses. (An ``id_key`` watermark only tracks
        insertion order for monotonic ``_id``s — with custom non-monotonic ones a
        later insert carrying a smaller ``_id`` would sort *below* the watermark
        and be silently dropped from the stream.)
        """
        # Two-stage: collect raw bytes under the lock, decode after.
        with self._lock:
            raw = [
                (rid, blob)
                for rid, _id_k, blob in self._scan_docs(db, coll)
                if after is None or rid > after
            ]
        return [(rid, bson.decode(blob)) for rid, blob in raw]

    def collection_min_recordid(self, db: str, coll: str) -> int | None:
        """Smallest RecordId in the collection — the oldest-inserted doc — or
        ``None`` if empty.

        Used to detect capped-collection rollover for tailable cursors: capped
        eviction is FIFO by RecordId, so if the min RecordId now exceeds a
        cursor's anchor, the document it was anchored on has been evicted and
        mongod kills the cursor with ``CappedPositionLost``. ``_scan_docs`` yields
        RecordId-ascending, so the first row is the minimum.
        """
        with self._lock:
            for recordid, _id_k, _blob in self._scan_docs(db, coll):
                return recordid
            return None

    def collection_max_recordid(self, db: str, coll: str) -> int | None:
        """Largest RecordId in the collection (``None`` if empty) — the position a
        tailable cursor sits at once it has handed out the collection's current
        contents. ``_scan_docs`` yields RecordId-ascending, so it's the last row.
        """
        with self._lock:
            last: int | None = None
            for recordid, _id_k, _blob in self._scan_docs(db, coll):
                last = recordid
            return last

    def recordid_for_id(self, db: str, coll: str, doc_id: Any) -> int | None:
        """The RecordId of the doc with ``_id == doc_id``, or ``None``.

        Public accessor over the ``_id`` index, for callers that hold a decoded
        document and need its position — the tailable cursor seeds its watermark
        with the last document it handed out.
        """
        with self._lock:
            return self._doc_recordid(db, coll, _id_key(doc_id))

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

    # Bounds for one insert chunk's statement transaction. A single wire
    # message can carry up to 48MB of documents, and writing them all in ONE
    # WT transaction pins ~2-3x that as UNEVICTABLE dirty cache (doc rows +
    # full-doc oplog entries + index entries). Once that approaches WT's
    # dirty-stall fraction of the cache, the engine livelocks: every thread
    # is drafted into eviction, but uncommitted content cannot be evicted and
    # only this transaction's own commit could free it. Reproduced with
    # 35k x 1.2KB docs against the 1G default cache (the mongo-rust-driver
    # ``large_insert`` weekly-CI wedge); the same insert against a 4G cache
    # takes 1.6s, and an 11MB insert against a 128M cache wedges identically.
    # mongod never writes a whole client batch in one storage transaction
    # either — it chunks internal insert batches — and batch inserts are
    # per-document atomic only, so the extra commit points are invisible to
    # clients.
    _INSERT_CHUNK_MAX_DOCS = 1000
    _INSERT_CHUNK_MAX_BYTES = 4 * 1024 * 1024

    def insert(
        self,
        db: str,
        coll: str,
        docs: Iterable[dict[str, Any]],
        *,
        ordered: bool = True,
        journal: bool = False,
    ) -> tuple[int, list[dict[str, Any]]]:
        self._note_write(db, coll)
        docs = list(docs)
        # Encode every doc once, up front: the blob feeds both the per-chunk
        # byte budget and the doc-table write (the chunk body reuses it, so
        # this is the same one-encode-per-doc as before). Server-side ``_id``
        # minting moves here too; a doc past an ordered stop may therefore
        # gain an ``_id`` it wouldn't have before, which is invisible on the
        # wire — drivers mint ``_id`` client-side.
        prepared: list[tuple[dict[str, Any], bytes]] = []
        for doc in docs:
            if "_id" not in doc:
                doc["_id"] = bson.ObjectId()
            prepared.append((doc, bson.encode(doc)))
        inserted = 0
        errors: list[dict[str, Any]] = []
        fresh_id_keys: set[bytes] = set()
        if not prepared:
            # Preserve the pre-chunking behaviour for an empty batch: the
            # collection is still created (lazy ensure) even with no docs.
            _, _, _ = self._insert_chunk(
                db,
                coll,
                [],
                base_index=0,
                ordered=ordered,
                sync=journal,
                fresh_id_keys=fresh_id_keys,
            )
            return 0, []
        start = 0
        n = len(prepared)
        while start < n:
            end = start + 1
            chunk_bytes = len(prepared[start][1])
            while (
                end < n
                and end - start < self._INSERT_CHUNK_MAX_DOCS
                and chunk_bytes + len(prepared[end][1]) <= self._INSERT_CHUNK_MAX_BYTES
            ):
                chunk_bytes += len(prepared[end][1])
                end += 1
            chunk_inserted, chunk_errors, stop = self._insert_chunk(
                db,
                coll,
                prepared[start:end],
                base_index=start,
                ordered=ordered,
                sync=journal,
                fresh_id_keys=fresh_id_keys,
            )
            inserted += chunk_inserted
            errors.extend(chunk_errors)
            if stop:
                break
            start = end
        return inserted, errors

    @_retry_write_conflicts
    def _insert_chunk(
        self,
        db: str,
        coll: str,
        prepared: list[tuple[dict[str, Any], bytes]],
        *,
        base_index: int,
        ordered: bool,
        sync: bool,
        fresh_id_keys: set[bytes],
    ) -> tuple[int, list[dict[str, Any]], bool]:
        """One bounded statement transaction of :meth:`insert`.

        ``prepared`` is ``[(doc, blob)]`` with ``_id`` already assigned;
        ``base_index`` offsets per-doc error indexes back into the client's
        batch. ``fresh_id_keys`` carries the *committed* prior chunks' keys so
        capped eviction never evicts documents of the same client batch; this
        chunk's keys are merged in only AFTER its transaction commits, so the
        conflict-retry wrapper (which rolls the chunk back and re-runs it)
        starts each attempt from a clean set. Returns
        ``(inserted, errors, stop)`` — ``stop`` when an ordered batch hit an
        error and the remaining chunks must not run.
        """
        inserted = 0
        errors: list[dict[str, Any]] = []
        oplog_entries: list[dict[str, Any]] = []
        chunk_keys: set[bytes] = set()
        stop = False
        oplog_on = self.enable_oplog
        with self._coll_lock(db, coll), self._batch_transaction(sync=sync):
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
            for offset, (doc, blob) in enumerate(prepared):
                index = base_index + offset
                key = _id_key(doc["_id"])
                if timeseries:
                    # Duplicate _ids are legal in timeseries collections —
                    # see _timeseries_doc_suffix.
                    key += self._timeseries_doc_suffix()
                conflict = self._unique_conflict(
                    db, coll, doc, indexes, exclude_recordid=None, partials=partials
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
                        stop = True
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
                        stop = True
                        break
                    continue
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
                        stop = True
                        break
                    continue
                # ``_id`` index first: it mints the RecordId the doc row is keyed
                # by, and it is where a duplicate ``_id`` is now caught (the doc
                # table keys by the unique RecordId, so it can't reject dups).
                # A WT rollback surfaces as WriteConflictError from inside.
                recordid = self._write_nat_entry(db, coll, key)
                if recordid is None:
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
                        stop = True
                        break
                    continue
                doc_cur = self._cursor(_doc_table_for(db, coll), overwrite=False)
                doc_cur.set_key(db, coll, recordid)
                doc_cur.set_value(_frame_doc_value(key, blob))
                try:
                    doc_cur.insert()
                except wt.WiredTigerError as exc:
                    # The RecordId is freshly minted and unique, so the only
                    # failure left here is a concurrency conflict.
                    raise WriteConflictError(str(exc)) from exc
                try:
                    self._write_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                except UniqueKeyTaken as taken:
                    # WiredTiger refused the key: a duplicate the snapshot-read
                    # probe above could not see. Reported exactly as the probe
                    # would have, so the client sees one behaviour either way.
                    self._undo_partial_insert(db, coll, recordid, key)
                    errors.append(
                        {
                            "index": index,
                            "code": 11000,
                            "errmsg": format_dup_key_errmsg(
                                f"{db}.{coll}", taken.index, taken.key_value
                            ),
                            "keyPattern": taken.key_pattern,
                            "keyValue": taken.key_value,
                        }
                    )
                    if ordered:
                        stop = True
                        break
                    continue
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
                chunk_keys.add(key)
            cap_entries, cap_pre_images = self._enforce_capped_bounds_locked(
                db, coll, fresh_id_keys | chunk_keys, indexes, partials, oplog_on, ns, ui
            )
            if oplog_entries or cap_entries:
                pre_images = [None] * len(oplog_entries) + cap_pre_images
                self._emit_oplog(oplog_entries + cap_entries, pre_images)
        # Only after the chunk's transaction committed: a conflict-retry rolls
        # the chunk back and re-runs it, and must not leave phantom keys that
        # would shield evictable docs from capped enforcement.
        fresh_id_keys |= chunk_keys
        return inserted, errors, stop

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

        "Oldest" is the natural-order (insertion-order) walk via
        ``_scan_docs_natural``, so eviction is strict FIFO regardless of
        ``_id`` monotonicity — user-supplied non-monotonic ``_id`` values
        are evicted in the order they were inserted, matching mongod.
        Freshly inserted docs from the current batch carry the highest
        ``seq`` and so sit at the natural-order tail; reaching one means
        everything left is fresh, which is exactly the break condition
        below.
        """
        raw = self._coll_options(db, coll) or {}
        if not raw.get("capped"):
            return [], []
        size_limit = raw.get("size")
        max_limit = raw.get("max")
        if size_limit is None and max_limit is None:
            return [], []
        scanned = list(self._scan_docs_natural(db, coll))
        total = sum(len(blob) for _rid, _id_k, blob in scanned)
        count = len(scanned)
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
        for recordid, id_k, blob in scanned:
            over_size = size_limit is not None and total > size_limit
            over_max = max_limit is not None and count > max_limit
            if not over_size and not over_max:
                break
            if id_k in fresh_id_keys:
                # Don't evict docs we just inserted in this batch — in
                # natural (insertion) order they always sit at the tail
                # (highest seq), so reaching one means everything left is
                # fresh too.
                break
            doc = bson.decode(blob)
            self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
            self._delete_doc_row(db, coll, recordid)
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
                        candidates = self._walk_index_in_order(
                            db, coll, idx_name, reverse=reverse, idx_dir=idx_dir
                        )
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
                    raw_blobs = [b for _rid, _idk, b in self._scan_docs_natural(db, coll)]
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
            out = apply_projection_batch(out, projection, filter)
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
            return [bson.decode(b) for _rid, _idk, b in self._scan_docs_natural(db, coll)], False
        if resolved == _ID_INDEX_NAME:
            # The doc table is keyed by RecordId (insertion order), so sort the
            # scan by ``id_key`` to reproduce an ``_id_`` index walk — the
            # ``_id`` index (``_NAT_SEQ_TABLE``) is in exactly that order.
            docs = [
                bson.decode(b)
                for _idk, b in sorted(
                    ((idk, b) for _rid, idk, b in self._scan_docs(db, coll)),
                    key=lambda row: row[0],
                )
            ]
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
        self, db: str, coll: str, name: str, *, reverse: bool = False, idx_dir: int = 1
    ) -> list[dict[str, Any]]:
        """Documents in index order, for sort acceleration.

        **Whole-array entries are skipped.** A multikey index writes one entry per
        array element *plus* one for the whole array, and the whole-array key sorts
        in the Array type slot — after every scalar. Walking backward therefore hits
        those first, and the first-occurrence dedup below then picked documents by
        their whole-array key instead of by their maximum element, which is what
        mongod orders by. (Ascending never showed it: the element entries come
        first there, so dedup naturally picked the minimum.)

        Concretely, with `[{x: [5,9]}, {x: [1,100]}, {x: [7]}, {x: 6}]` and an
        ascending index, descending returned insertion order rather than
        `[1,100] < [5,9] < [7] < 6` by maxima. The whole-array entries exist to
        answer equality against a whole array (`{x: [5, 9]}`) and have no business
        deciding sort position, so this walk — used only for ordering — drops them.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        c.set_key(db, coll, name, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        recordids: list[int] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            packed = bytes(k[3])
            esc, row_id = _unpack_entry(packed)
            if row_id is not None and not _is_whole_array_key(esc, idx_dir):
                recordids.append(row_id)
            if c.next() != 0:
                break
        if reverse:
            recordids.reverse()
        return self._docs_by_recordids(db, coll, recordids)

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
        "key_pattern", "direction", "multikey"}``. ``direction`` is
        ``"forward"`` unless a sort spec inverts it relative to the
        chosen index; ``multikey`` is the index's sticky multikey flag,
        surfaced as ``isMultiKey`` in the command's ``winningPlan``.
        """
        plan = self._explain_plan_uncached(
            db, coll, filter, sort=sort, hint=hint, collation=collation
        )
        if plan["kind"] == "IXSCAN":
            plan["multikey"] = self.index_is_multikey(db, coll, plan["index_name"])
        return plan

    def index_is_multikey(self, db: str, coll: str, name: str) -> bool:
        """The sticky multikey flag for index ``name`` (False if unknown).

        The ``_id`` index can never be multikey — ``_id`` is rejected as
        an array by the write path.
        """
        if name == _ID_INDEX_NAME:
            return False
        with self._lock:
            opts = self._index_options_map(db, coll).get(name) or {}
            return bool(opts.get("multikey"))

    def _explain_plan_uncached(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any] | None = None,
        *,
        sort: Mapping[str, Any] | None = None,
        hint: str | Mapping[str, Any] | None = None,
        collation: Any = None,
    ) -> dict[str, Any]:
        """Index-selection half of :meth:`explain_plan` (no multikey annotation).

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
            return sum(len(blob) for _rid, _id_k, blob in self._scan_docs(db, coll))

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
            id_size = sum(len(id_k) for _rid, id_k, _blob in self._scan_docs(db, coll))
            if id_size:
                sizes[_ID_INDEX_NAME] = id_size
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll))
            for k, _v in entry_rows:
                name = k[2]
                packed = bytes(k[3])
                sizes[name] = sizes.get(name, 0) + len(packed)
            return sizes

    # Per-statement-transaction bounds for the chunked multi-document write
    # paths (multi-update / unbounded delete) — the same values and the same
    # rationale as the chunked insert (see _INSERT_CHUNK_MAX_DOCS): one
    # transaction's dirty content is unevictable, and a matched set's rewrite
    # volume is unbounded. mongod's updateMany / deleteMany are per-document
    # write units and documented non-atomic, so the commit points match its
    # semantics. Twin of the Rust WRITE_CHUNK_* consts.
    _WRITE_CHUNK_MAX_DOCS = 1000
    _WRITE_CHUNK_MAX_BYTES = 4 * 1024 * 1024

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
        validator_moderate: bool = False,
        journal: bool = False,
        return_post_images: bool = False,
    ) -> dict[str, Any]:
        # Route: a multi-update outside a user transaction rewrites an
        # unbounded matched set, so it runs CHUNKED (bounded dirty per
        # statement transaction — the livelock class the chunked inserts
        # closed). Single-doc updates, upsert-only outcomes and
        # in-transaction updates keep the single-transaction body.
        if multi and getattr(self._tls, "user_txn", None) is None:
            return self._update_matching_chunked(
                db,
                coll,
                filter,
                update,
                upsert=upsert,
                array_filters=array_filters,
                let=let,
                collation=collation,
                validator=validator,
                validator_moderate=validator_moderate,
                journal=journal,
                return_post_images=return_post_images,
            )
        return self._update_matching_single_txn(
            db,
            coll,
            filter,
            update,
            multi=multi,
            upsert=upsert,
            array_filters=array_filters,
            let=let,
            collation=collation,
            validator=validator,
            validator_moderate=validator_moderate,
            journal=journal,
            return_post_images=return_post_images,
        )

    def _update_matching_chunked(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        update: dict[str, Any],
        *,
        upsert: bool,
        array_filters: list[dict[str, Any]] | None,
        let: dict[str, Any] | None,
        collation: Any,
        validator: dict[str, Any] | None,
        validator_moderate: bool = False,
        journal: bool,
        return_post_images: bool,
    ) -> dict[str, Any]:
        """Chunked updateMany driver — twin of the Rust
        ``update_matching_chunked``. One candidate scan collects matching
        RecordIds; bounded statement transactions then process the
        partitioned list, each chunk RE-FETCHING its doc rows inside its own
        transaction (the scan's blobs must never feed a later chunk's
        transform — a user transaction committing between chunks would be
        silently overwritten from the stale read, with no overlapping WT
        transactions to raise a conflict). A conflict retries only its own
        rolled-back chunk, and the RecordId list is partitioned, so ``$inc``
        applies exactly once per document."""
        self._note_write(db, coll)
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        self._refresh_read_snapshot()
        matched = 0
        modified = 0
        post_images: list[dict[str, Any]] | None = [] if return_post_images else None
        with self._coll_lock(db, coll):
            self._ensure_collection(db, coll)
            if collation_obj is not None:
                candidates = self._scan_docs(db, coll)
            else:
                candidates = self._candidates_iter(db, coll, filter)
            rids = [
                recordid
                for recordid, _id_k, blob in candidates
                if matches(bson.decode(blob), filter, vars=let, collation=collation_obj)
            ]
            idx = 0
            while idx < len(rids):
                consumed, m, w, posts = self._update_chunk(
                    db,
                    coll,
                    rids[idx:],
                    filter,
                    update,
                    array_filters=array_filters,
                    let=let,
                    collation_obj=collation_obj,
                    validator=validator,
                    validator_moderate=validator_moderate,
                    journal=journal,
                    want_posts=post_images is not None,
                )
                assert consumed > 0
                idx += consumed
                matched += m
                modified += w
                if post_images is not None:
                    post_images.extend(posts)
        if matched == 0:
            # Zero matches (or every candidate stopped matching by its
            # chunk's re-check): the single-transaction body rescans and
            # degenerates to its upsert branch or a clean zero outcome. The
            # coll lock is an RLock, but the delegation runs outside our
            # ``with`` anyway.
            return self._update_matching_single_txn(
                db,
                coll,
                filter,
                update,
                multi=True,
                upsert=upsert,
                array_filters=array_filters,
                let=let,
                collation=collation,
                validator=validator,
                validator_moderate=validator_moderate,
                journal=journal,
                return_post_images=return_post_images,
            )
        result: dict[str, Any] = {
            "matched": matched,
            "modified": modified,
            "upserted_id": None,
            "did_upsert": False,
        }
        if post_images is not None:
            result["post_images"] = post_images
        return result

    @_retry_write_conflicts
    def _update_chunk(
        self,
        db: str,
        coll: str,
        rids: list[int],
        filter: dict[str, Any],
        update: dict[str, Any],
        *,
        array_filters: list[dict[str, Any]] | None,
        let: dict[str, Any] | None,
        collation_obj: Any,
        validator: dict[str, Any] | None,
        validator_moderate: bool = False,
        journal: bool,
        want_posts: bool,
    ) -> tuple[int, int, int, list[dict[str, Any]]]:
        """One bounded chunk of the multi-update: process RecordIds from the
        front of ``rids`` until the doc/byte budget closes the transaction.
        ``consumed`` counts every examined RecordId so the driver always
        advances. Caller holds the coll lock."""
        consumed = 0
        matched = 0
        modified = 0
        chunk_bytes = 0
        posts: list[dict[str, Any]] = []
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        oplog_on = self.enable_oplog
        with self._batch_transaction(sync=journal):
            ns = self._ns(db, coll)
            ui = self._collection_uuid(db, coll) if oplog_on else None
            preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            multikey_names = self._multikey_index_names(db, coll)
            is_replacement = not isinstance(update, list) and not any(
                isinstance(k, str) and k.startswith("$") for k in update
            )
            doc_cur = self._cursor(_doc_table_for(db, coll))
            for recordid in rids:
                if (
                    modified >= self._WRITE_CHUNK_MAX_DOCS
                    or chunk_bytes >= self._WRITE_CHUNK_MAX_BYTES
                ):
                    break
                consumed += 1
                # Fresh read inside THIS transaction (see the driver note).
                doc_cur.reset()
                doc_cur.set_key(db, coll, recordid)
                if doc_cur.search() != 0:
                    continue
                id_k, blob = _unframe_doc_value(bytes(doc_cur.get_value()))
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
                    # ``validationLevel: "moderate"`` exempts a document that
                    # ALREADY failed the validator before this update — the level
                    # exists so a validator can be added to a collection with
                    # legacy rows without freezing them. A doc that currently
                    # SATISFIES the validator is still held to it, so an update
                    # cannot break a valid doc.
                    was_already_invalid = validator_moderate and not matches(doc, validator)
                    if (
                        validator is not None
                        and not matches(new, validator)
                        and not was_already_invalid
                    ):
                        raise DocumentValidationError(new.get("_id"))
                    conflict = self._unique_conflict(
                        db, coll, new, indexes, exclude_recordid=recordid, partials=partials
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
                    new_blob = bson.encode(new)
                    if len(new_blob) > MAX_BSON_OBJECT_SIZE:
                        raise DocumentTooLargeError(
                            10334,
                            "Plan executor error during update :: caused by :: "
                            f"Resulting document after update is larger than "
                            f"{MAX_BSON_OBJECT_SIZE}",
                        )
                    modified += 1
                    chunk_bytes += len(new_blob)
                    self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                    doc_cur.reset()
                    doc_cur[db, coll, recordid] = _frame_doc_value(id_k, new_blob)
                    try:
                        self._write_index_entries(
                            db, coll, new, indexes, partials, recordid=recordid
                        )
                    except UniqueKeyTaken as taken:
                        raise self._index_conflict_from(db, coll, new, taken) from taken
                    multikey_names = self._maybe_mark_multikey(
                        db, coll, new, indexes, multikey_names
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
                        if preimages_on:
                            chunk_bytes += len(blob)
                            pre_images.append(blob)
                        else:
                            pre_images.append(None)
                if want_posts:
                    posts.append(new)
            cap_ns = ns if oplog_on else ""
            cap_entries, cap_pre = self._enforce_capped_bounds_locked(
                db, coll, set(), indexes, partials, oplog_on, cap_ns, ui
            )
            if cap_entries:
                oplog_entries.extend(cap_entries)
                pre_images.extend(cap_pre)
            if oplog_entries:
                self._emit_oplog(oplog_entries, pre_images)
        return consumed, matched, modified, posts

    @_retry_write_conflicts
    def _update_matching_single_txn(
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
        validator_moderate: bool = False,
        journal: bool = False,
        return_post_images: bool = False,
    ) -> dict[str, Any]:
        self._note_write(db, coll)
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
        # The post-image of each write, captured while the statement still
        # holds the lock. ``findAndModify new:true`` reads it from here — a
        # post-write re-``find`` is a separate call a concurrent writer can
        # land in front of, handing two clients the same "new" document.
        post_images: list[dict[str, Any]] | None = [] if return_post_images else None
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
            for recordid, id_k, blob in candidates:
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
                    # ``moderate``: see the chunked path above. BOTH update
                    # paths enforce, and patching only one left single-document
                    # updates — the common case — still rejecting.
                    was_already_invalid = validator_moderate and not matches(doc, validator)
                    if (
                        validator is not None
                        and not matches(new, validator)
                        and not was_already_invalid
                    ):
                        raise DocumentValidationError(new.get("_id"))
                    # _id is immutable, so the row's RecordId is the right write
                    # target and its id_key is unchanged. For timeseries the
                    # id_key carries a uniqueness suffix that a recompute would
                    # drop — it comes back from the scan, framed in the value.
                    new_id_key = id_k
                    conflict = self._unique_conflict(
                        db, coll, new, indexes, exclude_recordid=recordid, partials=partials
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
                    self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                    doc_cur = self._cursor(_doc_table_for(db, coll))
                    doc_cur[db, coll, recordid] = _frame_doc_value(new_id_key, new_blob)
                    try:
                        self._write_index_entries(
                            db, coll, new, indexes, partials, recordid=recordid
                        )
                    except UniqueKeyTaken as taken:
                        raise self._index_conflict_from(db, coll, new, taken) from taken
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
                if post_images is not None:
                    post_images.append(new)
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
                    db, coll, new, indexes, exclude_recordid=None, partials=partials
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
                upsert_id_key = _id_key(upserted_id)
                # ``_id`` index first — it mints the RecordId the doc row is
                # keyed by (and would catch a dup, though the no-match branch
                # means there isn't one).
                upsert_recordid = self._write_nat_entry(db, coll, upsert_id_key)
                if upsert_recordid is None:
                    raise IndexConflict(
                        _ID_INDEX_NAME,
                        upserted_id,
                        key_pattern={"_id": 1},
                        key_value={"_id": upserted_id},
                        namespace=f"{db}.{coll}",
                    )
                doc_cur = self._cursor(_doc_table_for(db, coll))
                doc_cur[db, coll, upsert_recordid] = _frame_doc_value(upsert_id_key, upsert_blob)
                try:
                    self._write_index_entries(
                        db, coll, new, indexes, partials, recordid=upsert_recordid
                    )
                except UniqueKeyTaken as taken:
                    raise self._index_conflict_from(db, coll, new, taken) from taken
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
        result = {
            "matched": matched,
            "modified": modified,
            "upserted_id": upserted_id,
            "did_upsert": did_upsert,
        }
        if post_images is not None:
            if did_upsert:
                post_images.append(new)
            result["post_images"] = post_images
        return result

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
        # Route: an unbounded delete (deleteMany) outside a user transaction
        # runs CHUNKED — see ``update_matching``'s router note; same class,
        # same driver shape. Bounded deletes and in-transaction deletes keep
        # the single-transaction body.
        if limit == 0 and getattr(self._tls, "user_txn", None) is None:
            return self._delete_matching_chunked(
                db, coll, filter, let=let, collation=collation, journal=journal
            )
        return self._delete_matching_single_txn(
            db, coll, filter, limit=limit, let=let, collation=collation, journal=journal
        )

    def _delete_matching_chunked(
        self,
        db: str,
        coll: str,
        filter: dict[str, Any],
        *,
        let: dict[str, Any] | None,
        collation: Any,
        journal: bool,
    ) -> int:
        """Chunked deleteMany driver — see ``_update_matching_chunked`` for
        the re-fetch-inside-the-chunk-transaction rationale."""
        self._note_write(db, coll)
        from secantus.collation import parse as _parse_collation

        collation_obj = _parse_collation(collation)
        self._refresh_read_snapshot()
        deleted = 0
        with self._coll_lock(db, coll):
            if collation_obj is not None:
                candidates = self._scan_docs(db, coll)
            else:
                candidates = self._candidates_iter(db, coll, filter)
            rids = [
                recordid
                for recordid, _id_k, blob in candidates
                if matches(bson.decode(blob), filter, vars=let, collation=collation_obj)
            ]
            idx = 0
            while idx < len(rids):
                consumed, d = self._delete_chunk(
                    db,
                    coll,
                    rids[idx:],
                    filter,
                    let=let,
                    collation_obj=collation_obj,
                    journal=journal,
                )
                assert consumed > 0
                idx += consumed
                deleted += d
        return deleted

    @_retry_write_conflicts
    def _delete_chunk(
        self,
        db: str,
        coll: str,
        rids: list[int],
        filter: dict[str, Any],
        *,
        let: dict[str, Any] | None,
        collation_obj: Any,
        journal: bool,
    ) -> tuple[int, int]:
        """One bounded chunk of the deleteMany. Caller holds the coll lock.
        Returns ``(consumed, deleted)``."""
        consumed = 0
        deleted = 0
        chunk_bytes = 0
        oplog_entries: list[dict[str, Any]] = []
        pre_images: list[bytes | None] = []
        oplog_on = self.enable_oplog
        with self._batch_transaction(sync=journal):
            ns = self._ns(db, coll) if oplog_on else ""
            preimages_on = oplog_on and self._pre_post_images_enabled(db, coll)
            ui = (
                self._collection_uuid(db, coll)
                if oplog_on and self._coll_options(db, coll) is not None
                else None
            )
            indexes = self._all_indexes(db, coll)
            partials = self._partial_filters(db, coll)
            doc_cur = self._cursor(_doc_table_for(db, coll))
            for recordid in rids:
                if (
                    deleted >= self._WRITE_CHUNK_MAX_DOCS
                    or chunk_bytes >= self._WRITE_CHUNK_MAX_BYTES
                ):
                    break
                consumed += 1
                doc_cur.reset()
                doc_cur.set_key(db, coll, recordid)
                if doc_cur.search() != 0:
                    continue
                id_k, blob = _unframe_doc_value(bytes(doc_cur.get_value()))
                doc = bson.decode(blob)
                if not matches(doc, filter, vars=let, collation=collation_obj):
                    continue
                self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                self._delete_doc_row(db, coll, recordid)
                self._delete_nat_entry(db, coll, id_k)
                deleted += 1
                chunk_bytes += len(blob)
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
                    if preimages_on:
                        chunk_bytes += len(blob)
                        pre_images.append(blob)
                    else:
                        pre_images.append(None)
            if oplog_entries:
                self._emit_oplog(oplog_entries, pre_images)
        return consumed, deleted

    @_retry_write_conflicts
    def _delete_matching_single_txn(
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
        self._note_write(db, coll)
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
            for recordid, id_k, blob in candidates:
                doc = bson.decode(blob)
                if not matches(doc, filter, vars=let, collation=collation_obj):
                    continue
                self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                self._delete_doc_row(db, coll, recordid)
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
            for recordid, id_k, blob in candidates:
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
                self._delete_index_entries(db, coll, doc, indexes, partials, recordid=recordid)
                self._delete_doc_row(db, coll, recordid)
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
        # The documents shards are keyed (db, coll, RecordId); only the legacy
        # pre-shard single table is still (db, coll, id_key).
        if table == _DOC_TABLE:
            return "SSu"
        if table.startswith("table:secantus_documents"):
            return "SSq"
        return {
            _COLL_TABLE: "SS",
            _NAT_TABLE: "SSq",
            _NAT_SEQ_TABLE: "SSu",
            _IDX_TABLE: "SSS",
            _IDX_ENTRIES_TABLE: "SSSu",
            _UNIQ_TABLE: "SSSu",
            _TOMB_TABLE: "SS",
        }[table]

    @staticmethod
    def _smallest_for_kf(kf: str) -> tuple[Any, ...]:
        # Smallest value per WT column type: ``u`` -> empty bytes, ``q`` ->
        # INT64_MIN (the lowest int64 key), ``S`` -> empty string.
        return tuple(b"" if c == "u" else _INT64_MIN if c == "q" else "" for c in kf)

    def _collect_prefix(
        self, table: str, prefix: tuple[Any, ...]
    ) -> list[tuple[tuple[Any, ...], Any]]:
        c = self._cursor_optional(table)
        if c is None:
            return []  # lazy shards: an absent table holds no rows for this prefix
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

    def _recover_pending_drops_locked(self) -> None:
        pending = [k for k, _ in self._collect_prefix(_TOMB_TABLE, ())]
        for db, coll in pending:
            for tbl in (
                _doc_table_for(db, coll),
                _NAT_TABLE,
                _NAT_SEQ_TABLE,
                _IDX_TABLE,
                _IDX_ENTRIES_TABLE,
                _UNIQ_TABLE,
            ):
                rows = self._collect_prefix(tbl, (db, coll))
                self._delete_keys(tbl, [k for k, _ in rows])
            self._delete_keys(_TOMB_TABLE, [(db, coll)])

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
            for tbl in (
                _doc_table_for(db, coll),
                _NAT_TABLE,
                _NAT_SEQ_TABLE,
                _IDX_TABLE,
                _IDX_ENTRIES_TABLE,
                # Unique-key claims die with the collection — a claim that
                # survived DROP falsely rejected the value from a recreated
                # table (found by slt index/delete, the first weekly sweep
                # after #775).
                _UNIQ_TABLE,
            ):
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
            # Doc table sharded: a db's collections span all shards (+ legacy), so
            # purge every documents table for the db prefix.
            for tbl in (
                *_DOC_ALL_TABLES,
                _NAT_TABLE,
                _NAT_SEQ_TABLE,
                _IDX_TABLE,
                _IDX_ENTRIES_TABLE,
                _UNIQ_TABLE,
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
                for tbl in (
                    _doc_table_for(dst_db, dst_coll),
                    _NAT_TABLE,
                    _NAT_SEQ_TABLE,
                    _IDX_TABLE,
                    _IDX_ENTRIES_TABLE,
                    _UNIQ_TABLE,
                ):
                    rows = self._collect_prefix(tbl, (dst_db, dst_coll))
                    self._delete_keys(tbl, [k for k, _ in rows])
                c = self._cursor(_COLL_TABLE)
                c.set_key(dst_db, dst_coll)
                if c.search() == 0:
                    c.remove()
            # Doc table sharded: src collection lives in src's shard, dst in dst's
            # shard (may differ) — read from one, write to the other.
            src_doc_tbl = _doc_table_for(src_db, src_coll)
            doc_rows = self._collect_prefix(src_doc_tbl, (src_db, src_coll))
            self._delete_keys(src_doc_tbl, [k for k, _ in doc_rows])
            # Lazy shards: the rename target's shard may not exist yet.
            self._ensure_doc_shard(dst_db, dst_coll)
            dst_doc = self._cursor(_doc_table_for(dst_db, dst_coll))
            for k, v in doc_rows:
                dst_doc.set_key(dst_db, dst_coll, k[2])
                dst_doc.set_value(v)
                dst_doc.insert()
                dst_doc.reset()
            for tbl in (_NAT_TABLE, _NAT_SEQ_TABLE, _IDX_TABLE, _IDX_ENTRIES_TABLE, _UNIQ_TABLE):
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
            # A string index-key value names an index *plugin* (the special
            # index types). mongod recognises a fixed set — anything else is
            # rejected at parse time with "Unknown index plugin '<value>'"
            # (CannotCreateIndex, 67). We accept the geo plugins (2d /
            # 2dsphere); text / hashed are caught above as out-of-scope; any
            # other string (e.g. a typo'd ``{abc: "hallo thar"}``) is invalid.
            # mongo-c-driver's /Collection/index_w_write_concern asserts the
            # server rejects such a key.
            if isinstance(_spec_val, str) and _spec_val not in (
                "2d",
                "2dsphere",
                "2dsphere_bucket",
                "geoHaystack",
            ):
                raise CreateIndexUnsupported(f"Unknown index plugin '{_spec_val}'")
        options = dict(options or {})
        # An index build is DDL that must not interleave with in-flight writes to
        # the same collection: it scans the doc table into an entry list, writes
        # the catalog row, then lays the entries down. A concurrent insert landing
        # between the scan and the catalog write is invisible to BOTH halves — the
        # scan already snapshotted, and the inserter sees no index yet — so the doc
        # ends up with no index entry and an indexed query silently under-reports
        # it (`test_index_build_under_write_load_stays_consistent`). Holding the
        # per-collection lock is what `_coll_lock`'s own contract already promised
        # for DDL; `create_index` was the one DDL path not honouring it.
        #
        # LOCK ORDER: `_coll_lock` BEFORE `_lock` — the single canonical order (see
        # `_coll_lock`). Both are RLocks, so the nested `_collection_uuid` /
        # `_emit_oplog` calls below re-enter harmlessly on this thread.
        with self._coll_lock(db, coll), self._lock:
            # An index build SCANS the doc table to lay down its entries, so it
            # is a mutating scanner and needs the same snapshot refresh every
            # other one takes (drop_index / drop_all_indexes /
            # find_index_duplicates / prune_ttl / ...). Without it, this
            # thread's cached WT session can hold an MVCC read snapshot that
            # predates rows committed by OTHER connection threads — those docs
            # are invisible to the scan, get no index entries, and the index
            # silently under-reports them for the rest of its life while a
            # collection scan still finds them. Building an index during
            # concurrent writes is exactly when that happens (symptom:
            # `test_index_build_under_write_load_stays_consistent` seeing
            # index=38 vs scan=40 for a bucket).
            self._refresh_read_snapshot()
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
                existing_key = dict(existing.get("key") or {})
                existing_opts = dict(existing.get("options") or {})
                # Same name, different key spec → IndexKeySpecsConflict (86).
                # Key comparison is order-sensitive: mongod treats
                # ``{a: 1, b: 1}`` and ``{b: 1, a: 1}`` as distinct indexes, so
                # plain dict ``==`` (order-insensitive) would wrongly call them
                # equal — compare the ordered item lists.
                if list(existing_key.items()) != list(dict(key_spec).items()):
                    raise IndexKeySpecsConflict(
                        "An existing index has the same name as the requested "
                        "index. Requested index: "
                        f"{{ key: {dict(key_spec)!r}, name: {name!r} }}, "
                        f"existing index: {{ key: {existing_key!r}, name: {name!r} }}"
                    )
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
                entries: list[tuple[bytes, int]] = []
                for recordid, _id_k, blob in self._scan_docs(db, coll):
                    d = bson.decode(blob)
                    if partial_filter is not None and not matches(d, partial_filter):
                        continue
                    for cell_bytes in _doc_geo_cells(
                        d, geo_field, geo_type, options, index_name=name
                    ):
                        entries.append((cell_bytes, recordid))
                # Mark the on-disk entry format so a later build can tell these
                # RecordId entries from the pre-change id_key ones (the WT
                # key_format is SSSu either way).
                options["entryFormat"] = _ENTRY_FORMAT_RECORDID
                payload = bson.encode({"key": dict(key_spec), "options": options})
                c.reset()
                c[db, coll, name] = payload
                entry_cur = self._cursor(_IDX_ENTRIES_TABLE)
                for kb, entry_recordid in entries:
                    entry_cur.reset()
                    entry_cur[db, coll, name, _pack_entry(kb, entry_recordid)] = b""
                    if unique:
                        # Claim the existing rows' keys too, or the table would
                        # only protect values written after the index was made.
                        uq = self._cursor(_UNIQ_TABLE)
                        uq.reset()
                        uq[db, coll, name, _escape_kb(kb)] = entry_recordid
            else:
                # Single doc-table walk: decode each blob once and fold all
                # three checks (uniqueness, multikey detection, entry build)
                # into one pass. Index entries are written for every key
                # variant (``_index_key_variants``) so per-element multikey
                # lookups land at IXSCAN, and uniqueness is checked against
                # those same variants — mongod's unique-multikey rule is
                # "no two docs share any generated key".
                seen: dict[bytes, Any] | None = {} if unique else None
                multikey = False
                entries = []
                coll_opt = _parse_index_collation(options.get("collation"))
                for recordid, _id_k, blob in self._scan_docs(db, coll):
                    d = bson.decode(blob)
                    if partial_filter is not None and not matches(d, partial_filter):
                        continue
                    if not multikey and _doc_makes_multikey(d, key_spec_dict):
                        multikey = True
                    for kb in _index_key_variants(
                        d, key_spec_dict, sparse=sparse, collation=coll_opt
                    ):
                        if seen is not None:
                            if kb in seen:
                                raise IndexConflict(name, d.get("_id"), namespace=f"{db}.{coll}")
                            seen[kb] = d.get("_id")
                        entries.append((kb, recordid))
                if multikey:
                    options["multikey"] = True
                # Mark the on-disk entry format so a later build can tell these
                # RecordId entries from the pre-change id_key ones (the WT
                # key_format is SSSu either way).
                options["entryFormat"] = _ENTRY_FORMAT_RECORDID
                payload = bson.encode({"key": dict(key_spec), "options": options})
                c.reset()
                c[db, coll, name] = payload
                entry_cur = self._cursor(_IDX_ENTRIES_TABLE)
                for kb, entry_recordid in entries:
                    entry_cur.reset()
                    entry_cur[db, coll, name, _pack_entry(kb, entry_recordid)] = b""
                    if unique:
                        # Claim the existing rows' keys too, or the table would
                        # only protect values written after the index was made.
                        uq = self._cursor(_UNIQ_TABLE)
                        uq.reset()
                        uq[db, coll, name, _escape_kb(kb)] = entry_recordid
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
        # LOCK ORDER: `_coll_lock` BEFORE `_lock` (the canonical order, same as
        # `create_index`). Without the per-collection lock a concurrent
        # insert/update/delete landing between the entry-table snapshot and its
        # deletion is invisible to both, the drop-direction twin of the
        # create_index race #632 closed (#635).
        with self._coll_lock(db, coll), self._lock:
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
            # Capture the spec before removal: mongod's showExpandedEvents
            # ``dropIndexes`` event describes the dropped index in full
            # (``{v, key, name}``, probed 7.0.12), not just its name.
            dropped = bson.decode(bytes(c.get_value()))
            key_spec = dict(dropped.get("key", {}))
            c.remove()
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll, name))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            # The dropped unique index's claims go with it — recreating the
            # index (or just inserting the same values) must not hit them.
            uq_rows = self._collect_prefix(_UNIQ_TABLE, (db, coll, name))
            self._delete_keys(_UNIQ_TABLE, [k for k, _ in uq_rows])
            ui = self._collection_uuid(db, coll)
            self._emit_oplog(
                [
                    {
                        "op": "c",
                        "ns": f"{db}.$cmd",
                        "ui": bson.Binary(ui.bytes, subtype=4),
                        "o": {"dropIndexes": coll, "index": name, "key": key_spec},
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

    def set_index_options(self, db: str, coll: str, name: str, **opts: Any) -> bool:
        """Merge ``opts`` into an existing index's stored options blob.

        Read-modify-write of the ``{key, options}`` payload, mirroring
        ``set_index_expiry``. Backs ``collMod {index: {keyPattern|name,
        prepareUnique|unique: ...}}``. Returns ``True`` when the index
        existed and was updated.
        """
        with self._lock:
            self._refresh_read_snapshot()
            c = self._cursor(_IDX_TABLE)
            c.set_key(db, coll, name)
            if c.search() != 0:
                return False
            payload = bson.decode(bytes(c.get_value()))
            options = payload.get("options", {})
            options.update(opts)
            payload["options"] = options
            c.reset()
            c[db, coll, name] = bson.encode(payload)
            return True

    def find_index_duplicates(self, db: str, coll: str, name: str) -> list[list[Any]]:
        """Group ``_id`` values of documents that share the same key on
        index ``name``, returning one ``_id`` list per duplicated key
        (groups of size >= 2, ``_id``-sorted within each group).

        Backs ``collMod {index: {unique: true}}``: a non-empty result
        means the conversion to unique must be refused with code 359
        (``CannotConvertIndexToUnique``) and the groups reported as
        ``violations``.
        """
        with self._lock:
            self._refresh_read_snapshot()
            spec: tuple[dict[str, Any], dict[str, Any]] | None = None
            for n, key_spec, opts in self._iter_indexes(db, coll):
                if n == name:
                    spec = (key_spec, opts)
                    break
            if spec is None:
                return []
            key_spec, opts = spec
            sparse = bool(opts.get("sparse"))
            coll_opt = _parse_index_collation(opts.get("collation"))
            blobs = [blob for _rid, _id_k, blob in self._scan_docs(db, coll)]
        # Grouped over every key each doc contributes, matching what
        # ``_unique_conflict`` would refuse: on a multikey index two docs
        # violate uniqueness as soon as they share one generated key.
        groups: dict[bytes, list[Any]] = {}
        for blob in blobs:
            doc = bson.decode(blob)
            for kb in _index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt):
                groups.setdefault(kb, []).append(doc.get("_id"))
        out: list[list[Any]] = []
        for ids in groups.values():
            if len(ids) > 1:
                out.append(sorted(ids, key=_id_key))
        return out

    def drop_all_indexes(self, db: str, coll: str) -> int:
        # LOCK ORDER: `_coll_lock` BEFORE `_lock`, as `create_index` /
        # `drop_index` — the per-collection lock keeps a concurrent CRUD writer
        # from racing the index-entry snapshot-then-delete (#635).
        with self._coll_lock(db, coll), self._lock:
            # Mutating scanners read the current rows before deleting/rewriting
            # them; a snapshot pinned by an earlier positioned cursor on
            # this connection thread would hide rows committed by other
            # threads and turn the scan into a silent partial no-op
            # (the gauge's drop-then-reinsert E11000 cluster).
            self._refresh_read_snapshot()
            rows = self._collect_prefix(_IDX_TABLE, (db, coll))
            # Capture each index's key spec before deletion: mongod's
            # showExpandedEvents ``dropIndexes`` event describes the dropped
            # index in full (``{v, key, name}``, probed 7.0.12).
            dropped = [(k[2], dict(bson.decode(bytes(v)).get("key", {}))) for k, v in rows]
            self._delete_keys(_IDX_TABLE, [k for k, _ in rows])
            entry_rows = self._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll))
            self._delete_keys(_IDX_ENTRIES_TABLE, [k for k, _ in entry_rows])
            uq_rows = self._collect_prefix(_UNIQ_TABLE, (db, coll))
            self._delete_keys(_UNIQ_TABLE, [k for k, _ in uq_rows])
            if dropped:
                ui = self._collection_uuid(db, coll)
                self._emit_oplog(
                    [
                        {
                            "op": "c",
                            "ns": f"{db}.$cmd",
                            "ui": bson.Binary(ui.bytes, subtype=4),
                            "o": {"dropIndexes": coll, "index": n, "key": key},
                        }
                        for n, key in dropped
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
        recordid: int,
    ) -> None:
        """Write one entry per key ``doc`` contributes to each index.

        Entries point at the doc's **RecordId** (the doc-table key), so an IXSCAN
        fetch reads the row directly — no ``id_key`` hop. That also removes the
        timeseries special case the ``id_key`` form needed (a timeseries doc-table
        key carries a uniqueness suffix that isn't derivable from ``_id``).
        """
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
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
                    c[db, coll, name, _pack_entry(cell_bytes, recordid)] = b""
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            enforce = _unique or bool(index_options.get(name, {}).get("prepareUnique"))
            for kb in _index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt):
                c.reset()
                c[db, coll, name, _pack_entry(kb, recordid)] = b""
                if enforce:
                    self._claim_unique_key(db, coll, name, kb, recordid, key_spec, doc, coll_opt)

    def _index_conflict_from(
        self, db: str, coll: str, doc: dict[str, Any], taken: UniqueKeyTaken
    ) -> IndexConflict:
        """The refusal WiredTiger raised, reported as the duplicate-key error
        the snapshot probe would have produced for the same collision."""
        return IndexConflict(
            taken.index,
            doc.get("_id"),
            key_pattern=taken.key_pattern,
            key_value=taken.key_value,
            namespace=f"{db}.{coll}",
        )

    def _undo_partial_insert(self, db: str, coll: str, recordid: int, id_key: bytes) -> None:
        """Remove the doc row and index entries written before a unique key was
        refused, so a rejected insert leaves nothing behind. The whole statement
        runs in one WT transaction, but an unordered batch continues past the
        failure and must not carry a half-written row with it."""
        with contextlib.suppress(Exception):
            cur = self._cursor(_doc_table_for(db, coll))
            cur.reset()
            cur.set_key(db, coll, recordid)
            cur.remove()
        with contextlib.suppress(Exception):
            seq = self._cursor(_NAT_SEQ_TABLE)
            seq.reset()
            seq.set_key(db, coll, id_key)
            seq.remove()

    def _claim_unique_key(
        self,
        db: str,
        coll: str,
        name: str,
        kb: bytes,
        recordid: int,
        key_spec: dict[str, Any],
        doc: dict[str, Any],
        collation: Any,
    ) -> None:
        """Take ownership of one unique-index key, or fail.

        The insert uses a NON-overwrite cursor, so WiredTiger itself rejects a
        key another row already holds — including one committed after this
        transaction's snapshot, which the snapshot-read probe could not see.
        Two transactions racing for the same key collide on it and one takes a
        write conflict, which the storage retry wrapper replays into a clean
        duplicate-key error.
        """
        cur = self._cursor(_UNIQ_TABLE, overwrite=False)
        cur.reset()
        cur.set_key(db, coll, name, _escape_kb(kb))
        cur.set_value(recordid)
        try:
            rc = cur.insert()
        except Exception as exc:
            if _is_wt_duplicate_key(exc):
                raise self._dup_key_error(db, coll, name, kb, key_spec, doc, collation) from exc
            raise
        if rc != 0:
            raise self._dup_key_error(db, coll, name, kb, key_spec, doc, collation)

    def _dup_key_error(
        self,
        db: str,
        coll: str,
        name: str,
        kb: bytes,
        key_spec: dict[str, Any],
        doc: dict[str, Any],
        collation: Any,
    ) -> UniqueKeyTaken:
        return UniqueKeyTaken(
            name, dict(key_spec), _conflict_key_value(doc, key_spec, kb, collation=collation)
        )

    def _release_unique_keys(
        self,
        db: str,
        coll: str,
        name: str,
        keys: list[bytes],
        recordid: int,
    ) -> None:
        """Drop this row's claims so the values become available again.

        Only claims this RecordId actually owns are released: an update that
        rewrites a row re-claims its keys before the old entries are swept, and
        removing a claim another row now holds would silently unprotect it.
        """
        if not keys:
            return
        cur = self._cursor(_UNIQ_TABLE)
        for kb in keys:
            cur.reset()
            cur.set_key(db, coll, name, _escape_kb(kb))
            with contextlib.suppress(Exception):
                if cur.search() == 0 and cur.get_value() == recordid:
                    cur.remove()

    def _delete_index_entries(
        self,
        db: str,
        coll: str,
        doc: dict[str, Any],
        indexes: list[tuple[str, dict[str, Any], bool, bool]],
        partials: dict[str, dict[str, Any]] | None = None,
        *,
        recordid: int,
    ) -> None:
        """Remove the entries ``doc`` contributed, keyed by its RecordId."""
        if not indexes:
            return
        c = self._cursor(_IDX_ENTRIES_TABLE)
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
                    c.set_key(db, coll, name, _pack_entry(cell_bytes, recordid))
                    if c.search() == 0:
                        c.remove()
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            if _unique or index_options.get(name, {}).get("prepareUnique"):
                # Give the values back, or the row that held them would keep
                # them reserved forever and a delete-then-reinsert would fail.
                self._release_unique_keys(
                    db,
                    coll,
                    name,
                    list(_index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt)),
                    recordid,
                )
            for kb in _index_key_variants(doc, key_spec, sparse=sparse, collation=coll_opt):
                c.reset()
                c.set_key(db, coll, name, _pack_entry(kb, recordid))
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
        exclude_recordid: int | None,
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
            # ``prepareUnique`` enforces uniqueness on new writes without
            # the index being formally unique yet — mongod blocks dup
            # inserts the moment ``collMod {index: {prepareUnique: true}}``
            # lands, even while pre-existing duplicates remain.
            if not unique and not index_options.get(name, {}).get("prepareUnique"):
                continue
            pf = partials.get(name)
            if pf is not None and not matches(candidate_doc, pf):
                continue
            coll_opt = _parse_index_collation(index_options.get(name, {}).get("collation"))
            # Probe every key the doc contributes, not just the canonical
            # whole-doc one: on a multikey index mongod enforces
            # uniqueness across all generated keys, and for a path that
            # descends through an array the canonical key isn't even
            # among the entries the writers laid down.
            for kb in _index_key_variants(
                candidate_doc, key_spec, sparse=sparse, collation=coll_opt
            ):
                if not self._entry_taken(c, db, coll, name, kb, exclude_recordid):
                    continue
                return (
                    name,
                    dict(key_spec),
                    _conflict_key_value(candidate_doc, key_spec, kb, collation=coll_opt),
                )
        return None

    @staticmethod
    def _entry_taken(
        c: Any,
        db: str,
        coll: str,
        name: str,
        kb: bytes,
        exclude_recordid: int | None,
    ) -> bool:
        """True if index ``name`` already has an entry for key ``kb``
        belonging to a document other than ``exclude_recordid``."""
        esc_kb = _escape_kb(kb)
        c.reset()
        c.set_key(db, coll, name, esc_kb + _ENTRY_SEP)
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return False
        if rc < 0 and c.next() != 0:
            return False
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                return False
            row_esc, row_id = _unpack_entry(bytes(k[3]))
            if row_esc != esc_kb:
                return False
            if exclude_recordid is None or row_id != exclude_recordid:
                return True
            if c.next() != 0:
                return False

    def _scan_index_for_id_keys(
        self, db: str, coll: str, name: str, kb: bytes, *, prefix: bool = False
    ) -> list[int]:
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
        out: list[int] = []
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
            if row_id is not None:
                out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _all_id_keys_for_index(self, db: str, coll: str, name: str) -> list[int]:
        """Every RecordId with an entry in index ``name`` — a full index scan.

        Serves ``{field: {$exists: true}}`` via a sparse index: a sparse
        index's entries table holds an entry for exactly the docs where the
        indexed field is present (missing-field docs are omitted; present-
        but-null keeps an entry), so the complete set of entries *is* the
        ``$exists: true`` match set. A RecordId can repeat for multikey arrays
        (one entry per element); the caller's ``_docs_by_recordids`` dedups.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        c.set_key(db, coll, name, b"")
        rc = c.search_near()
        if rc == wt.WT_NOTFOUND:
            return []
        if rc < 0 and c.next() != 0:
            return []
        out: list[int] = []
        while True:
            k = c.get_key()
            if (k[0], k[1], k[2]) != (db, coll, name):
                break
            _row_esc, row_id = _unpack_entry(bytes(k[3]))
            if row_id is not None:
                out.append(row_id)
            if c.next() != 0:
                break
        return out

    def _docs_by_recordids(self, db: str, coll: str, recordids: list[int]) -> list[dict[str, Any]]:
        """Fetch documents by RecordId — the IXSCAN fetch.

        Index entries carry the RecordId directly, so this reads the doc row
        straight away; it used to resolve ``id_key -> _id index -> RecordId``
        first, and deleting that hop is the point of the entry-format change.
        """
        if not recordids:
            return []
        c = self._cursor_optional(_doc_table_for(db, coll))
        if c is None:
            return []  # lazy shards: collection's shard never written
        # Two-stage: WT cursor walk first (raw bytes), then ``bson.decode``
        # outside that loop. The cursor work is what needs lock scope;
        # decode is pure CPU and benefits from running unsynchronised.
        # Multikey indexes write per-element entries, so the same doc's
        # RecordId can appear more than once for queries that match
        # multiple elements. Dedupe while preserving order.
        raw: list[bytes] = []
        for recordid in dict.fromkeys(recordids):
            c.reset()
            c.set_key(db, coll, recordid)
            if c.search() == 0:
                raw.append(_unframe_doc_value(bytes(c.get_value()))[1])
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
    ) -> list[int]:
        """Walk index entries in each (lo, hi) range; return deduplicated RecordIds.

        A doc with N covering cells produces N index entries; we collect
        just one ``_id`` per doc. The post-fetch verifier (in
        ``find_matching``'s ``matches()`` step) discards docs whose
        actual geometry doesn't match the query.
        """
        c = self._cursor(_IDX_ENTRIES_TABLE)
        seen: set[int] = set()
        out: list[int] = []
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
        seen: set[int],
        out: list[int],
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
            kb_part, recordid = _unpack_entry(packed)
            if recordid is None:
                if c.next() != 0:
                    return
                continue
            if kb_part > hi_prefix:
                return
            if recordid not in seen:
                seen.add(recordid)
                out.append(recordid)
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
        recordids = self._try_index_id_keys(db, coll, filter, collation=collation)
        if recordids is None:
            return None
        return self._docs_by_recordids(db, coll, recordids)

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
                # Callers want RecordIds. For an `_id` lookup the `_id` index IS
                # the primary access path (not the secondary-index hop the
                # RecordId entry format removed), so resolve each key through it;
                # a key with no row simply matches nothing.
                rids = [self._doc_recordid(db, coll, k) for k in id_keys]
                return [r for r in rids if r is not None]
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
    ) -> list[tuple[int, bytes, bytes]]:
        """Return (RecordId, id_key, blob) triples that the write paths should
        consider. If an index covers the filter, only the indexed candidates are
        fetched; otherwise the full doc table is scanned. Either way,
        BSON decode is left to the caller so non-matching docs don't pay
        for it. Caller still applies ``matches()`` to the decoded doc —
        index lookups can produce false-positive candidates for partial
        scans (multikey, prefix overlap, etc).

        The RecordId comes back with each row because the doc table is keyed by
        it — a write path needs it to rewrite / remove the row — and the
        ``id_key`` because a timeseries row's key carries a suffix that is not
        derivable from ``_id``. Both come straight off the index entry and the
        framed row value, with no extra lookup.
        """
        if filter:
            recordids = self._try_index_id_keys(db, coll, filter)
            if recordids is not None:
                c = self._cursor_optional(_doc_table_for(db, coll))
                if c is None:
                    return []  # lazy shards: collection's shard never written
                out: list[tuple[int, bytes, bytes]] = []
                # Same dedup contract as ``_docs_by_recordids``: multikey
                # indexes can yield duplicate RecordIds for one doc.
                for recordid in dict.fromkeys(recordids):
                    c.reset()
                    c.set_key(db, coll, recordid)
                    if c.search() == 0:
                        id_k, blob = _unframe_doc_value(bytes(c.get_value()))
                        out.append((recordid, id_k, blob))
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
    ) -> list[int]:
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
    ) -> list[int]:
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
        out: list[int] = []
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
            if row_id is not None:
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
    ) -> list[int]:
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
        out: list[int] = []
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
            if row_id is not None:
                out.append(row_id)
            if c.next() != 0:
                break
        return out
