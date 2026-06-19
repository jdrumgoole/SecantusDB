"""PITR v2 — arbitrary-window recovery via archived oplog segments + base snapshots.

PITR v1 (:mod:`secantus.oplog_replay`) replays an oplog onto an **empty** base, so
its recovery window is the live oplog retention window: once ``prune_oplog`` drops
the front of the oplog, history before the new floor is gone and a restore to a
time before it would be partial (v1 refuses with a "past genesis" error).

v2 lifts that by keeping two durable artifacts in an **archive directory**:

* **Oplog segments** (``oplog-<start>-<end>.seg``) — the rows ``prune_oplog`` is
  about to drop, written out first (see :meth:`Storage._prune_oplog_locked` when
  ``oplog_archive_dir`` is set). Each segment is a stream of length-framed BSON
  docs ``{"s": seq, "e": entry, "p": pre_image | None}``.
* **Base snapshots** (``base-<head>.tar.gz``) — ordinary backup archives
  (:meth:`Storage.create_archive`) named by their oplog head seq, taken
  periodically by the operator (:meth:`Storage.archive_base_snapshot`).

A restore to target time ``T`` then: picks the newest base whose head is at or
before ``T``, extracts it (a *non-empty* base), and replays the oplog forward from
the base's head to ``T`` — sourcing those rows from the archived segments and the
other snapshots' live oplogs, deduped by seq. Because every seq is either still
live in some snapshot or captured in a segment, the merged stream is contiguous
from genesis, so any ``T`` within the archived window is reachable.
"""

from __future__ import annotations

import contextlib
import datetime as _dt
import glob
import os
import struct
import tempfile
from collections.abc import Iterable, Iterator, Mapping
from typing import Any

import bson
from bson import Timestamp

_SEG_PREFIX = "oplog-"
_SEG_SUFFIX = ".seg"
_BASE_PREFIX = "base-"
_BASE_SUFFIX = ".tar.gz"
_LEN = struct.Struct(">I")


def segment_name(start_seq: int, end_seq: int) -> str:
    """Filename for a segment covering ``[start_seq, end_seq]`` (zero-padded so a
    lexical directory listing is also seq order)."""
    return f"{_SEG_PREFIX}{start_seq:020d}-{end_seq:020d}{_SEG_SUFFIX}"


def base_name(head_seq: int) -> str:
    """Filename for a base snapshot whose oplog head is ``head_seq``."""
    return f"{_BASE_PREFIX}{head_seq:020d}{_BASE_SUFFIX}"


def write_segment(
    archive_dir: str,
    rows: Iterable[tuple[int, Mapping[str, Any], Mapping[str, Any] | None]],
) -> str | None:
    """Append ``rows`` (``(seq, entry, pre_image)`` in seq order) as one segment
    file in ``archive_dir``. Returns the path written, or ``None`` if no rows.
    """
    framed: list[bytes] = []
    first = last = None
    for seq, entry, pre in rows:
        if first is None:
            first = seq
        last = seq
        doc: dict[str, Any] = {"s": int(seq), "e": dict(entry)}
        if pre is not None:
            doc["p"] = dict(pre)
        blob = bson.encode(doc)
        framed.append(_LEN.pack(len(blob)))
        framed.append(blob)
    if first is None:
        return None
    os.makedirs(archive_dir, exist_ok=True)
    path = os.path.join(archive_dir, segment_name(first, last))
    # Write to a temp file then atomically rename so a crash mid-write never
    # leaves a half-segment that the reader would choke on.
    fd, tmp = tempfile.mkstemp(dir=archive_dir, suffix=".part")
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(b"".join(framed))
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise
    return path


def _read_segment_file(path: str) -> Iterator[tuple[int, dict[str, Any], dict[str, Any] | None]]:
    with open(path, "rb") as fh:
        data = fh.read()
    off = 0
    n = len(data)
    while off < n:
        (length,) = _LEN.unpack_from(data, off)
        off += _LEN.size
        doc = bson.decode(data[off : off + length])
        off += length
        yield int(doc["s"]), doc["e"], doc.get("p")


def iter_archived_oplog(
    archive_dir: str,
) -> Iterator[tuple[int, dict[str, Any], dict[str, Any] | None]]:
    """Yield ``(seq, entry, pre_image)`` from every segment in ``archive_dir``,
    in seq order (segments are non-overlapping, named by their range)."""
    seg_paths = sorted(glob.glob(os.path.join(archive_dir, f"{_SEG_PREFIX}*{_SEG_SUFFIX}")))
    for path in seg_paths:
        yield from _read_segment_file(path)


def list_base_snapshots(archive_dir: str) -> list[tuple[int, str]]:
    """Return ``[(head_seq, path), ...]`` for the base snapshots in ``archive_dir``,
    ascending by head seq."""
    out: list[tuple[int, str]] = []
    for path in glob.glob(os.path.join(archive_dir, f"{_BASE_PREFIX}*{_BASE_SUFFIX}")):
        stem = os.path.basename(path)[len(_BASE_PREFIX) : -len(_BASE_SUFFIX)]
        try:
            out.append((int(stem), path))
        except ValueError:
            continue
    out.sort()
    return out


def is_archive_dir(path: str) -> bool:
    """True if ``path`` is a PITR v2 archive directory — a directory holding at
    least one base snapshot or oplog segment. Distinguishes it from a stopped
    server's data directory (which holds WiredTiger files) so the restore entry
    points can route a directory ``source`` to the right path."""
    if not os.path.isdir(path):
        return False
    return bool(list_base_snapshots(path)) or bool(
        glob.glob(os.path.join(path, f"{_SEG_PREFIX}*{_SEG_SUFFIX}"))
    )


def read_base_manifest(base_path: str) -> dict[str, Any]:
    """Read the embedded ``pitr-manifest.json`` from a base snapshot tar without
    extracting the WiredTiger files. Returns ``{}`` if absent."""
    import json
    import tarfile

    from .storage import _PITR_MANIFEST_NAME

    with tarfile.open(base_path, "r:gz") as tar:
        try:
            member = tar.extractfile(_PITR_MANIFEST_NAME)
        except KeyError:
            return {}
        if member is None:
            return {}
        return json.loads(member.read())


def _ts_from_pair(pair: Any) -> Timestamp | None:
    if isinstance(pair, (list, tuple)) and len(pair) == 2:
        return Timestamp(int(pair[0]), int(pair[1]))
    return None


def _wall_from_iso(value: Any) -> _dt.datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        dt = _dt.datetime.fromisoformat(value)
    except ValueError:
        return None
    return dt if dt.tzinfo is not None else dt.replace(tzinfo=_dt.timezone.utc)


def _head_within_bound(
    manifest: Mapping[str, Any],
    *,
    to_ts: Timestamp | None,
    to_wall: _dt.datetime | None,
) -> bool:
    """Is this base snapshot's head at or before the target? A base whose head is
    *after* ``T`` already contains operations past ``T`` and can't be used."""
    if to_ts is not None:
        head_ts = _ts_from_pair(manifest.get("oplogHeadTs"))
        return head_ts is not None and head_ts <= to_ts
    if to_wall is not None:
        head_wall = _wall_from_iso(manifest.get("oplogHeadWall"))
        # No recorded wall (older archive) → conservatively unusable for a wall
        # bound; fall back to an empty base + segment replay.
        return head_wall is not None and head_wall <= to_wall
    return True  # no bound → "latest", every base qualifies


def select_base(
    archive_dir: str,
    *,
    to_ts: Timestamp | None = None,
    to_wall: _dt.datetime | None = None,
) -> tuple[int, str] | None:
    """Choose the newest base snapshot whose oplog head is at or before the target
    time, or ``None`` to replay onto an empty base (segments must reach genesis)."""
    chosen: tuple[int, str] | None = None
    for head_seq, path in list_base_snapshots(archive_dir):  # ascending
        if _head_within_bound(read_base_manifest(path), to_ts=to_ts, to_wall=to_wall):
            chosen = (head_seq, path)
    return chosen


def restore_from_archive_dir(
    archive_dir: str,
    target_dir: str,
    *,
    to_ts: Timestamp | None = None,
    to_wall: _dt.datetime | None = None,
    carry_oplog: bool = False,
) -> dict[str, Any]:
    """PITR v2: rebuild ``target_dir`` as of the target time from an archive
    directory of base snapshots + oplog segments.

    Picks the newest base snapshot whose head is at or before the target, extracts
    it as a non-empty base, then replays the oplog forward from the base's head to
    the target — stitching the rows from the archived segments and the newest
    snapshot's still-live oplog (deduped by seq). With no base at or before the
    target, replays onto an empty base, which requires the segments to reach
    genesis (seq 1).

    Raises ``ValueError`` if the merged oplog has a gap that prevents reaching the
    target time.
    """
    from .oplog_replay import replay
    from .storage import Storage, extract_backup_archive

    if to_ts is not None and to_wall is not None:
        raise ValueError("pass at most one of to_ts / to_wall")

    base = select_base(archive_dir, to_ts=to_ts, to_wall=to_wall)
    base_head = base[0] if base is not None else 0

    if base is not None:
        extract_backup_archive(base[1], target_dir, allow_existing=True)
    # else: target_dir stays empty; a fresh Storage opens on it below.

    # Gather post-base oplog rows (seq > base_head): the archived segments plus
    # the newest snapshot's still-live oplog (rows not yet pruned when it was
    # taken). Deduped by seq — every seq is immutable, so any source is fine.
    rows: dict[int, dict[str, Any]] = {}
    pre_map: dict[int, dict[str, Any]] = {}
    for seq, entry, pre in iter_archived_oplog(archive_dir):
        if seq > base_head:
            rows.setdefault(seq, entry)
            if pre is not None:
                pre_map.setdefault(seq, pre)

    bases = list_base_snapshots(archive_dir)
    if bases:
        newest_head, newest_path = bases[-1]
        if newest_head > base_head:
            with tempfile.TemporaryDirectory(prefix="secantus-pitr-base-") as tmp:
                extract_backup_archive(newest_path, tmp, allow_existing=True)
                src = Storage(tmp, enable_oplog=True)
                try:
                    start = base_head + 1
                    while True:
                        batch = src.read_oplog(start_seq=start, limit=2000)
                        if not batch:
                            break
                        for seq, entry in batch:
                            if seq > base_head:
                                rows.setdefault(seq, entry)
                                if carry_oplog and seq not in pre_map:
                                    pre = src.read_preimage(seq)
                                    if pre is not None:
                                        pre_map[seq] = pre
                        start = batch[-1][0] + 1
                finally:
                    src.close()

    # Contiguous run from base_head+1 — replay can't skip a missing seq.
    ordered = sorted(rows)
    contiguous: list[tuple[int, dict[str, Any]]] = []
    expected = base_head + 1
    for seq in ordered:
        if seq != expected:
            break
        contiguous.append((seq, rows[seq]))
        expected += 1
    gap_after = expected - 1  # last contiguous seq (== base_head if none)
    has_more_past_gap = bool(ordered) and ordered[-1] > gap_after

    target = Storage(target_dir, enable_oplog=True)
    try:
        stats = replay(
            target,
            iter(contiguous),
            up_to_ts=to_ts,
            up_to_wall=to_wall,
            carry_oplog=carry_oplog,
            pre_image_for=pre_map.get if carry_oplog else None,
        )
    finally:
        target.close()

    # If a bound was set and we never reached it because the contiguous run ran
    # out before the bound (yet later rows exist past a gap), the archive is
    # incomplete — fail loudly rather than return a silently-truncated database.
    bound_set = to_ts is not None or to_wall is not None
    reached_bound = stats["entriesSeen"] < len(contiguous) or not bound_set
    if bound_set and not reached_bound and has_more_past_gap:
        raise ValueError(
            f"archived oplog has a gap after seq {gap_after}: cannot reach the "
            "requested recovery time. The base snapshots and oplog segments do "
            "not cover the full range — take more frequent base snapshots."
        )

    stats["baseHeadSeq"] = base_head
    stats["basePath"] = base[1] if base is not None else None
    stats["archiveDir"] = archive_dir
    stats["targetDir"] = target_dir
    return stats
