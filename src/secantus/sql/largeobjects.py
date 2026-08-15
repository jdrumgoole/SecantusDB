"""PostgreSQL large objects: the ``lo_*`` function surface over Fastpath.

pgjdbc's ``LargeObjectManager`` (and therefore its JDBC ``Blob``/``Clob`` and
the LargeObject API tests) drives large objects through the **Fastpath
sub-protocol**: it resolves the ``lo_*`` function OIDs from ``pg_catalog.
pg_proc`` once per connection, then issues wire ``FunctionCall`` ('F')
messages. This module is the server side of that surface:

- ``LO_PROC_OIDS`` — the built-in functions with **PostgreSQL's real OIDs**
  (``pg_proc.dat``), reflected into ``pg_catalog.pg_proc`` by ``virtual.py``.
- ``call`` — dispatch one Fastpath invocation (binary args in, binary result
  out), used by the wire server's 'F' handler.

Object bytes live in a per-database ``__sql_largeobjects__`` collection, one
document per object: ``{_id: <oid int>, data: <Binary>}``. Descriptors are
per-session (``Session.lo_descriptors``), carrying ``(oid, position, mode)``
— PG closes them at transaction end; we close them at session end (noted in
``tasks/backlog.md``; no gauge test distinguishes the two). Reads and writes
route through the session's open transaction when one is active, so a
``ROLLBACK`` discards ``lowrite`` data exactly as PG's transactional large
objects do.
"""

from __future__ import annotations

import struct
from typing import Any

from bson.binary import Binary

from secantus.sql import errors

LO_COLLECTION = "__sql_largeobjects__"
LO_CHUNKS = "__sql_largeobject_chunks__"

#: PostgreSQL's own pg_proc OIDs for the large-object functions (pg_proc.dat).
#: pgjdbc looks these up by name and then calls by OID, so the values are
#: arbitrary from its point of view — but real ones cost nothing and keep any
#: OID-hardcoding client working.
LO_PROC_OIDS: dict[str, int] = {
    "lo_open": 952,
    "lo_close": 953,
    "loread": 954,
    "lowrite": 955,
    "lo_lseek": 956,
    "lo_creat": 957,
    "lo_create": 715,
    "lo_tell": 958,
    "lo_unlink": 964,
    "lo_truncate": 1004,
    "lo_lseek64": 3170,
    "lo_tell64": 3171,
    "lo_truncate64": 3172,
}
_OID_TO_NAME = {oid: name for name, oid in LO_PROC_OIDS.items()}

#: ``libpq``'s inversion-mode flags (``libpq-fs.h``).
INV_WRITE = 0x00020000
INV_READ = 0x00040000

SEEK_SET, SEEK_CUR, SEEK_END = 0, 1, 2

#: PG's large-object size ceiling (4TB): seeks past it fail, and the failed
#: seek poisons the enclosing transaction exactly like any other error.
MAX_LO_SIZE = 4 * 1024**4

#: First OID handed to ``lo_creat`` on an empty store — past the range PG
#: reserves for built-ins, so reflected rows never collide with catalog oids.
_FIRST_LO_OID = 16384


def _i32(b: bytes) -> int:
    if len(b) != 4:
        raise errors.SQLError("22023", f"fastpath argument is {len(b)} bytes, expected 4")
    return struct.unpack(">i", b)[0]


def _i64(b: bytes) -> int:
    if len(b) != 8:
        raise errors.SQLError("22023", f"fastpath argument is {len(b)} bytes, expected 8")
    return struct.unpack(">q", b)[0]


class _Store:
    """Chunked large-object storage, range-based like PG's own 2KB pages.

    One metadata doc per object (``{_id: oid, size: N}``) plus 256KB chunk
    docs (``{_id: {"o": oid, "i": n}, data: Binary}``) — a single BSON doc
    cannot hold a large object (16MB cap), and ``lo_truncate`` may extend to
    multi-GB sizes, which PG represents sparsely: extension just grows the
    logical size; holes read as zero bytes and store nothing.

    Reads/writes join the session's open transaction (so ROLLBACK discards
    them), autocommit otherwise.
    """

    CHUNK = 256 * 1024

    def __init__(self, storage: Any, db: str, session: Any) -> None:
        self._storage = storage
        self._db = db
        self._session = session

    def _run(self, fn, *args, **kw):
        handle = getattr(self._session, "txn_handle", None)
        if handle is not None:
            with self._storage.use_user_transaction(handle):
                return fn(*args, **kw)
        return fn(*args, **kw)

    # -- metadata ---------------------------------------------------------- #

    def meta(self, oid: int) -> dict | None:
        docs = self._run(
            self._storage.find_matching, self._db, LO_COLLECTION, {"_id": oid}, limit=1
        )
        return docs[0] if docs else None

    def size(self, oid: int) -> int:
        m = self.meta(oid)
        return int(m.get("size", 0)) if m else 0

    def _set_size(self, oid: int, size: int) -> None:
        self._run(
            self._storage.update_matching,
            self._db,
            LO_COLLECTION,
            {"_id": oid},
            {"$set": {"size": int(size)}},
        )

    def create(self, oid: int) -> None:
        self._run(self._storage.insert, self._db, LO_COLLECTION, [{"_id": oid, "size": 0}])

    def delete(self, oid: int) -> int:
        n = self._run(self._storage.delete_matching, self._db, LO_COLLECTION, {"_id": oid})
        if n:
            self._run(self._storage.delete_matching, self._db, LO_CHUNKS, {"_id.o": oid})
        return n

    def next_oid(self) -> int:
        docs = self._run(self._storage.find_matching, self._db, LO_COLLECTION, {})
        top = max((int(d["_id"]) for d in docs), default=_FIRST_LO_OID - 1)
        return max(top + 1, _FIRST_LO_OID)

    # -- chunk-range data access ------------------------------------------- #

    def _chunk(self, oid: int, idx: int) -> bytes:
        docs = self._run(
            self._storage.find_matching,
            self._db,
            LO_CHUNKS,
            {"_id": {"o": oid, "i": idx}},
            limit=1,
        )
        return bytes(docs[0]["data"]) if docs else b""

    def _put_chunk(self, oid: int, idx: int, data: bytes) -> None:
        self._run(self._storage.delete_matching, self._db, LO_CHUNKS, {"_id": {"o": oid, "i": idx}})
        if data:
            self._run(
                self._storage.insert,
                self._db,
                LO_CHUNKS,
                [{"_id": {"o": oid, "i": idx}, "data": Binary(data)}],
            )

    def read(self, oid: int, pos: int, length: int) -> bytes:
        size = self.size(oid)
        if pos >= size or length <= 0:
            return b""
        end = min(pos + length, size)
        out = bytearray()
        for idx in range(pos // self.CHUNK, (end - 1) // self.CHUNK + 1):
            chunk = self._chunk(oid, idx)
            chunk = chunk + b"\x00" * (min(self.CHUNK, size - idx * self.CHUNK) - len(chunk))
            lo = max(pos - idx * self.CHUNK, 0)
            hi = min(end - idx * self.CHUNK, self.CHUNK)
            out += chunk[lo:hi]
        return bytes(out)

    def write(self, oid: int, pos: int, data: bytes) -> None:
        end = pos + len(data)
        last = (end - 1) // self.CHUNK + 1 if data else pos // self.CHUNK
        for idx in range(pos // self.CHUNK, last):
            chunk = bytearray(self._chunk(oid, idx))
            lo = max(pos - idx * self.CHUNK, 0)
            hi = min(end - idx * self.CHUNK, self.CHUNK)
            if len(chunk) < hi:
                chunk += b"\x00" * (hi - len(chunk))
            src_lo = idx * self.CHUNK + lo - pos
            chunk[lo:hi] = data[src_lo : src_lo + (hi - lo)]
            self._put_chunk(oid, idx, bytes(chunk))
        if end > self.size(oid):
            self._set_size(oid, end)

    def truncate(self, oid: int, length: int) -> None:
        size = self.size(oid)
        if length < size:
            last_idx = (length - 1) // self.CHUNK if length else -1
            self._run(
                self._storage.delete_matching,
                self._db,
                LO_CHUNKS,
                {"_id.o": oid, "_id.i": {"$gt": last_idx}},
            )
            if length and length % self.CHUNK:
                keep = self._chunk(oid, last_idx)[: length % self.CHUNK]
                self._put_chunk(oid, last_idx, keep)
        # Extension is sparse: the hole reads as zeros, stores nothing.
        self._set_size(oid, length)


def _descriptors(session: Any) -> dict[int, dict[str, Any]]:
    if not hasattr(session, "lo_descriptors"):
        session.lo_descriptors = {}
        session.lo_next_fd = 0
    return session.lo_descriptors


def _fd(session: Any, raw: bytes) -> dict[str, Any]:
    fd = _i32(raw)
    desc = _descriptors(session).get(fd)
    if desc is None:
        raise errors.SQLError("42704", f"invalid large-object descriptor: {fd}")
    return desc


# Fastpath functions that MUTATE stored large-object data (create / write /
# truncate / delete). Used by the pgserver Fastpath handler to apply the same
# RBAC + read-only-transaction gates a table write goes through, which the
# Fastpath sub-protocol otherwise skips (#836). `lo_open` is excluded: it only
# builds a descriptor — the actual mutation happens in `lowrite`/`lo_truncate`,
# which are gated here and additionally require the descriptor's INV_WRITE mode.
_WRITE_LO_FUNCS: frozenset[str] = frozenset(
    {"lo_creat", "lo_create", "lowrite", "lo_truncate", "lo_truncate64", "lo_unlink"}
)


def is_write_call(fn_oid: int) -> bool:
    """Whether the Fastpath function with this OID mutates stored large-object
    data (so it needs a write privilege and is barred in a read-only transaction)."""
    return _OID_TO_NAME.get(fn_oid) in _WRITE_LO_FUNCS


def call(fn_oid: int, args: list[bytes], *, storage: Any, db: str, session: Any) -> bytes:
    """Execute one Fastpath function call; returns the binary result value."""
    name = _OID_TO_NAME.get(fn_oid)
    if name is None:
        raise errors.SQLError("42883", f"function with OID {fn_oid} does not exist")
    store = _Store(storage, db, session)

    if name in ("lo_creat", "lo_create"):
        if name == "lo_creat":
            oid = store.next_oid()  # argument is the (ignored) access mode
        else:
            oid = _i32(args[0])
            if oid == 0:
                oid = store.next_oid()
            elif store.meta(oid) is not None:
                raise errors.SQLError("23505", f'large object "{oid}" already exists')
        store.create(oid)
        return struct.pack(">i", oid)

    if name == "lo_open":
        oid, mode = _i32(args[0]), _i32(args[1])
        if store.meta(oid) is None:
            raise errors.SQLError("42704", f"large object {oid} does not exist")
        descs = _descriptors(session)
        fd = session.lo_next_fd
        session.lo_next_fd += 1
        descs[fd] = {"oid": oid, "pos": 0, "mode": mode}
        return struct.pack(">i", fd)

    if name == "lo_close":
        fd = _i32(args[0])
        if _descriptors(session).pop(fd, None) is None:
            raise errors.SQLError("42704", f"invalid large-object descriptor: {fd}")
        return struct.pack(">i", 0)

    if name == "loread":
        desc, length = _fd(session, args[0]), _i32(args[1])
        chunk = store.read(desc["oid"], desc["pos"], max(length, 0))
        desc["pos"] += len(chunk)
        return chunk

    if name == "lowrite":
        desc, payload = _fd(session, args[0]), args[1]
        if not desc["mode"] & INV_WRITE:
            raise errors.SQLError("55000", "large object descriptor not opened for writing")
        store.write(desc["oid"], desc["pos"], payload)
        desc["pos"] += len(payload)
        return struct.pack(">i", len(payload))

    if name in ("lo_lseek", "lo_lseek64"):
        desc = _fd(session, args[0])
        offset = _i64(args[1]) if name.endswith("64") else _i32(args[1])
        whence = _i32(args[2])
        size = store.size(desc["oid"])
        base = {SEEK_SET: 0, SEEK_CUR: desc["pos"], SEEK_END: size}.get(whence)
        if base is None:
            raise errors.SQLError("22023", f"invalid whence: {whence}")
        new = base + offset
        if new < 0 or new > MAX_LO_SIZE:
            raise errors.SQLError("22023", f"invalid seek target: {new}")
        desc["pos"] = new
        return struct.pack(">q" if name.endswith("64") else ">i", new)

    if name in ("lo_tell", "lo_tell64"):
        desc = _fd(session, args[0])
        return struct.pack(">q" if name.endswith("64") else ">i", desc["pos"])

    if name in ("lo_truncate", "lo_truncate64"):
        desc = _fd(session, args[0])
        if not desc["mode"] & INV_WRITE:
            raise errors.SQLError("55000", "large object descriptor not opened for writing")
        length = _i64(args[1]) if name.endswith("64") else _i32(args[1])
        if length < 0 or length > MAX_LO_SIZE:
            raise errors.SQLError("22023", f"invalid truncate target: {length}")
        store.truncate(desc["oid"], length)
        return struct.pack(">i", 0)

    if name == "lo_unlink":
        oid = _i32(args[0])
        if store.delete(oid) == 0:
            raise errors.SQLError("42704", f"large object {oid} does not exist")
        # PG invalidates descriptors over the dead object.
        descs = _descriptors(session)
        for fd in [f for f, d in descs.items() if d["oid"] == oid]:
            del descs[fd]
        return struct.pack(">i", 1)

    raise errors.SQLError("42883", f"fastpath function {name} is not implemented")
