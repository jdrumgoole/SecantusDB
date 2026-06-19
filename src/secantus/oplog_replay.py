"""Oplog replay — the engine behind point-in-time recovery (PITR).

Real MongoDB PITR is *snapshot + oplog replay*: restore a base, then roll the
oplog forward to a target time. SecantusDB already records a mongod-shaped oplog
(``table:secantus_oplog``) in the same WiredTiger connection as the data, so a
backup self-contains its oplog. This module supplies the missing half — an
**applier** that replays oplog entries (``i`` / ``u`` / ``d`` / ``c`` / ``n``)
into a fresh :class:`~secantus.storage.Storage`, stopping at a target timestamp.

The v1 model (see ``docs/recovery.md``) replays onto an **empty** base, which is
correct whenever the source oplog still reaches genesis (``oplog_floor_seq() ==
1`` — the first minted seq). If the oplog has been pruned from the front,
:func:`restore_to_timestamp` raises a clear error pointing at the deferred v2
(base-snapshot stitching) rather than silently rebuilding a partial database.

Replay drives the ordinary :class:`Storage` write paths under
:meth:`Storage.replay_mode`, so documents, indexes, and natural (insertion)
order are reconstructed exactly as they were produced live — the oplog is the
*input*, never regenerated. The cut at the target time is by ``ts`` (or wall
clock), not by seq, so a multi-document transaction — whose statements all share
one ``ts`` — is always replayed all-or-nothing.
"""

from __future__ import annotations

import datetime as _dt
import tempfile
from collections.abc import Callable, Iterable, Mapping
from typing import Any

from bson import Timestamp

from .diff import apply_update_description
from .storage import Storage

# Oplog rows are read from the source in batches this size.
_READ_CHUNK = 5000

# The first seq Storage mints (``_next_seq`` starts at 1). An oplog whose floor
# is this value still reaches the database's first write; a higher floor means
# ``prune_oplog`` deleted the front and an empty-base replay would be partial.
_GENESIS_SEQ = 1


def _ns_split(ns: str) -> tuple[str, str]:
    """Split ``"db.coll"`` into ``(db, coll)`` on the first dot (db names never
    contain a dot; collection names may)."""
    db, _, coll = ns.partition(".")
    return db, coll


def _apply_collmod(storage: Storage, db: str, coll: str, description: Mapping[str, Any]) -> None:
    """Re-apply a ``collMod`` oplog ``operationDescription`` — the same shape the
    command handler builds (``_coll_mod`` in commands.py)."""
    for key, val in description.items():
        if key == "index" and isinstance(val, Mapping):
            name = val.get("name")
            expiry = val.get("expireAfterSeconds")
            if name is not None and expiry is not None:
                storage.set_index_expiry(db, coll, name, expiry)
        else:
            storage.set_collection_options(db, coll, **{key: val})


def _apply_command(storage: Storage, db: str, o: Mapping[str, Any]) -> None:
    """Dispatch a ``c`` (command) oplog entry's ``o`` to the matching Storage DDL.

    Mirrors the oplog ``c`` shapes Storage emits (create / drop / dropDatabase /
    createIndexes / dropIndexes / collMod / renameCollection). Collection
    *options* (``capped`` / ``size`` / ``max`` / ``validator`` / ``viewOn`` / …)
    ride the ``create`` entry as siblings of ``create`` in ``o``, so they are
    reconstructed on the new collection.
    """
    if "create" in o:
        options = {k: v for k, v in o.items() if k not in ("create", "idIndex")}
        storage.create_collection(db, o["create"], options=options or None)
    elif "drop" in o:
        storage.drop_collection(db, o["drop"])
    elif "dropDatabase" in o:
        storage.drop_database(db)
    elif "createIndexes" in o:
        coll = o["createIndexes"]
        for spec in o.get("indexes", []):
            options = {k: v for k, v in spec.items() if k not in ("v", "key", "name")}
            storage.create_index(db, coll, spec["name"], spec["key"], options)
    elif "dropIndexes" in o:
        storage.drop_index(db, o["dropIndexes"], o["index"])
    elif "collMod" in o:
        coll = o["collMod"]
        _apply_collmod(storage, db, coll, {k: v for k, v in o.items() if k != "collMod"})
    elif "renameCollection" in o:
        sdb, scoll = _ns_split(o["renameCollection"])
        ddb, dcoll = _ns_split(o["to"])
        storage.rename_collection(sdb, scoll, ddb, dcoll, drop_target="dropTarget" in o)
    # Unknown commands (e.g. an internal noop wrapped as 'c') are ignored.


def _apply_entry(storage: Storage, entry: Mapping[str, Any]) -> bool:
    """Apply one oplog entry to ``storage``. Returns True if it mutated state.

    Must be called inside ``storage.replay_mode()`` so the write paths don't
    re-emit oplog.
    """
    op = entry.get("op")
    if op == "n":  # periodic noop heartbeat — advances time, changes nothing
        return False
    if op == "c":
        db, _ = _ns_split(str(entry.get("ns", "")))
        _apply_command(storage, db, entry["o"])
        return True

    db, coll = _ns_split(str(entry.get("ns", "")))
    if op == "i":
        storage.insert(db, coll, [dict(entry["o"])])
        return True
    if op == "u":
        _id = entry["o2"]["_id"]
        o = entry["o"]
        if isinstance(o, Mapping) and o.get("$v") == 2 and "diff" in o:
            existing = storage.find_matching(db, coll, {"_id": _id})
            if not existing:
                # In-order replay should never reach an update for a missing
                # doc; tolerate it rather than corrupt the restore.
                return False
            post = apply_update_description(existing[0], o["diff"])
        else:
            post = dict(o)  # full-document replacement
        storage.update_matching(db, coll, {"_id": _id}, post, multi=False)
        return True
    if op == "d":
        _id = entry.get("o2", entry.get("o", {})).get("_id")
        if _id is None:
            return False
        storage.delete_matching(db, coll, {"_id": _id}, limit=1)
        return True
    return False


def _within_bound(
    entry: Mapping[str, Any],
    *,
    up_to_ts: Timestamp | None,
    up_to_wall: _dt.datetime | None,
) -> bool:
    """Is ``entry`` at or before the target? An entry past the bound stops
    replay. Both ``ts`` and ``wall`` are shared across a transaction's
    statements, so the cut never splits a transaction."""
    if up_to_ts is not None:
        ts = entry.get("ts")
        if isinstance(ts, Timestamp) and ts > up_to_ts:
            return False
    if up_to_wall is not None:
        wall = entry.get("wall")
        if isinstance(wall, _dt.datetime) and _as_utc(wall) > up_to_wall:
            return False
    return True


def _as_utc(dt: _dt.datetime) -> _dt.datetime:
    """Treat a naive datetime as UTC so comparisons don't raise."""
    if dt.tzinfo is None:
        return dt.replace(tzinfo=_dt.timezone.utc)
    return dt


def replay(
    target: Storage,
    oplog_entries: Iterable[tuple[int, Mapping[str, Any]]],
    *,
    up_to_ts: Timestamp | None = None,
    up_to_wall: _dt.datetime | None = None,
    carry_oplog: bool = False,
    pre_image_for: Callable[[int], Mapping[str, Any] | None] | None = None,
) -> dict[str, Any]:
    """Replay ``oplog_entries`` (``(seq, entry)`` in ascending seq order) into
    ``target``, stopping before the first entry past the target time.

    With ``carry_oplog`` the in-bound rows are also written **verbatim** into the
    target's oplog (via :meth:`Storage.import_oplog_segment`), so a change stream
    on the restored server can resume from a token minted before the restore
    point. ``pre_image_for(seq)`` supplies the pre-image doc for the seqs that
    had one (so ``fullDocumentBeforeChange`` keeps working on resume); pass
    ``source.read_preimage``. Without ``carry_oplog`` the restored store starts a
    fresh, empty oplog timeline (matching ``mongorestore``).

    Returns ``{"opsApplied", "entriesSeen", "lastSeq", "lastTs", "lastWall",
    "oplogCarried"}``.
    """
    ops = 0
    seen = 0
    last_seq = 0
    last_ts: Timestamp | None = None
    last_wall: _dt.datetime | None = None
    carried: list[tuple[int, Mapping[str, Any]]] = []
    with target.replay_mode():
        for seq, entry in oplog_entries:
            if not _within_bound(entry, up_to_ts=up_to_ts, up_to_wall=up_to_wall):
                break
            seen += 1
            last_seq = seq
            last_ts = entry.get("ts") if isinstance(entry.get("ts"), Timestamp) else last_ts
            last_wall = (
                entry.get("wall") if isinstance(entry.get("wall"), _dt.datetime) else last_wall
            )
            if carry_oplog:
                carried.append((seq, dict(entry)))
            if _apply_entry(target, entry):
                ops += 1
    if carry_oplog and carried:
        pre_images: dict[int, Mapping[str, Any]] = {}
        if pre_image_for is not None:
            for seq, _entry in carried:
                pre = pre_image_for(seq)
                if pre is not None:
                    pre_images[seq] = pre
        target.import_oplog_segment(carried, pre_images)
    target.checkpoint()
    return {
        "opsApplied": ops,
        "entriesSeen": seen,
        "lastSeq": last_seq,
        "lastTs": last_ts,
        "lastWall": last_wall,
        "oplogCarried": len(carried) if carry_oplog else 0,
    }


def _iter_source_oplog(source: Storage) -> Iterable[tuple[int, dict[str, Any]]]:
    """Yield every oplog row in seq order from ``source``, batched."""
    start = source.oplog_floor_seq()
    while True:
        batch = source.read_oplog(start_seq=start, limit=_READ_CHUNK)
        if not batch:
            return
        yield from batch
        start = batch[-1][0] + 1


def restore_to_timestamp(
    source_dir: str,
    target_dir: str,
    *,
    to_ts: Timestamp | None = None,
    to_wall: _dt.datetime | None = None,
    carry_oplog: bool = False,
) -> dict[str, Any]:
    """Rebuild ``target_dir`` as the database was at the target time by replaying
    ``source_dir``'s oplog forward.

    ``source_dir`` is a **stopped** server's data directory or an extracted
    backup archive (WiredTiger's single-writer lock forbids opening a live
    one). ``target_dir`` must be a fresh, empty path. With neither ``to_ts``
    nor ``to_wall`` the whole oplog is replayed ("latest").

    With ``carry_oplog`` the replayed oplog rows are preserved verbatim on the
    restored store, so a change stream there can resume from a token minted
    before the restore point. The default (``False``) starts a fresh oplog
    timeline, matching ``mongorestore``.

    Raises ``ValueError`` if the source oplog has been pruned past genesis
    (v1 replays onto an empty base; a non-genesis floor needs the deferred v2
    base-snapshot path).
    """
    if to_ts is not None and to_wall is not None:
        raise ValueError("pass at most one of to_ts / to_wall")
    if to_wall is not None:
        to_wall = _as_utc(to_wall)

    source = Storage(source_dir, enable_oplog=True)
    try:
        floor = source.oplog_floor_seq()
        if floor == 0:
            raise ValueError(
                "source has no oplog to replay — was it created with an oplog "
                "(SecantusDBServer replica_set_name / Storage enable_oplog=True)?"
            )
        if floor > _GENESIS_SEQ:
            raise ValueError(
                f"source oplog floor is seq {floor}, past genesis (seq "
                f"{_GENESIS_SEQ}): it has been pruned from the front, so an "
                "empty-base replay would be partial. Restoring to a time before "
                "the floor needs a base snapshot (PITR v2 — not yet implemented)."
            )
        target = Storage(target_dir, enable_oplog=True)
        try:
            stats = replay(
                target,
                _iter_source_oplog(source),
                up_to_ts=to_ts,
                up_to_wall=to_wall,
                carry_oplog=carry_oplog,
                pre_image_for=source.read_preimage if carry_oplog else None,
            )
        finally:
            target.close()
    finally:
        source.close()
    stats["sourceDir"] = source_dir
    stats["targetDir"] = target_dir
    return stats


def restore_archive_to_timestamp(
    archive_path: str,
    target_dir: str,
    *,
    to_ts: Timestamp | None = None,
    to_wall: _dt.datetime | None = None,
    carry_oplog: bool = False,
) -> dict[str, Any]:
    """Like :func:`restore_to_timestamp` but the source is a ``.tar.gz`` backup
    archive (from ``Storage.create_archive`` / ``secantusAdmin.backupArchive``).

    The archive is extracted into a temp dir, replayed, and the temp dir is
    discarded — only ``target_dir`` survives.
    """
    from .storage import extract_backup_archive

    with tempfile.TemporaryDirectory(prefix="secantus-pitr-src-") as tmp:
        extract_backup_archive(archive_path, tmp, allow_existing=True)
        return restore_to_timestamp(
            tmp, target_dir, to_ts=to_ts, to_wall=to_wall, carry_oplog=carry_oplog
        )
