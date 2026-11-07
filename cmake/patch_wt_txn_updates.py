"""Idempotent patch for WiredTiger's session statistics: count a running
transaction's updates (``txn_updates``), as WiredTiger 11 does.

mongod 8.2's ``Slow query`` line and profile entry carry, in
``storage.data``, the dirty bytes a transaction statement added
(``txnBytesDirty``, the session statistic ``txn_bytes_dirty``) and the updates
it added (``txnNumUpdates``, ``txn_updates``). The WiredTiger vendored here
(mongodb-7.0.33) keeps the first and not the second. WiredTiger 11
(mongodb-8.2) counts it in ``__txn_incr_bytes_dirty``, under the same
conditions as ``txn_bytes_dirty`` -- an application session, a transaction
running or holding an ID, not drafted into eviction, a page with a
``modify`` -- and only for a NEW update: the three sites in
``serial_inline.h`` that link an insert, an append or an update into a page
(every other caller of ``__wt_cache_page_inmem_incr`` passes
``new_update=false``).

What the patch changes, one target file per call:

- ``src/include/stat.h``: ``txn_updates`` in ``WT_SESSION_STATS``, after
  ``cache_time``, so the statistics already numbered keep their keys;
- ``src/support/stat.c``: its description, ``session: number of updates in
  this txn`` (WiredTiger 11's), and its clear, which zeroes it with
  ``txn_bytes_dirty`` as this engine's clear does;
- ``src/include/wiredtiger.in``: ``WT_STAT_SESSION_TXN_UPDATES`` (4008);
- ``src/include/btree_inline.h``: ``__wt_cache_page_inmem_incr_txn_update``,
  the count with ``txn_bytes_dirty``'s conditions;
- ``src/include/serial_inline.h``: the three new-update sites call it;
- ``src/session/session_api.c``: ``begin_transaction`` starts it at zero,
  where this engine starts ``txn_bytes_dirty``.

What it does NOT change: WiredTiger 11 also keeps the connection-wide
``cache_updates_txn_uncommitted_count`` and clears both session counts when
a transaction is released; this engine clears ``txn_bytes_dirty`` at the next
``begin_transaction`` instead, and ``txn_updates`` follows it.

Reapplying is a no-op: the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "/* secantus-patch: txn_updates */"

HELPER_ANCHOR = """/*
 * __wt_cache_decr_check_size --
"""
HELPER = f"""{PATCH_MARKER}
/*
 * __wt_cache_page_inmem_incr_txn_update --
 *     Count a new update the running transaction linked into a page, under the conditions
 *     txn_bytes_dirty is counted: WiredTiger 11's txn_updates.
 */
static inline void
__wt_cache_page_inmem_incr_txn_update(WT_SESSION_IMPL *session, WT_PAGE *page, size_t size)
{{
    if (size == 0 || page->modify == NULL || F_ISSET(session, WT_SESSION_INTERNAL) ||
      !F_ISSET(session->txn, WT_TXN_RUNNING | WT_TXN_HAS_ID) ||
      __wt_session_gen(session, WT_GEN_EVICT) != 0)
        return;
    WT_STAT_SESSION_INCRV(session, txn_updates, 1);
}}

"""

# (file suffix, [(anchor, replacement, expected count)])
EDITS: dict[str, list[tuple[str, str, int]]] = {
    "src/include/stat.h": [
        (
            "    int64_t lock_schema_wait;\n    int64_t cache_time;\n};\n",
            f"    int64_t lock_schema_wait;\n    int64_t cache_time;\n"
            f"    int64_t txn_updates; {PATCH_MARKER}\n}};\n",
            1,
        )
    ],
    "src/support/stat.c": [
        (
            '  "session: time waiting for cache (usecs)",\n};\n',
            '  "session: time waiting for cache (usecs)",\n'
            f'  "session: number of updates in this txn", {PATCH_MARKER}\n}};\n',
            1,
        ),
        (
            "    stats->lock_schema_wait = 0;\n    stats->cache_time = 0;\n}\n",
            "    stats->lock_schema_wait = 0;\n    stats->cache_time = 0;\n"
            "    stats->txn_updates = 0;\n}\n",
            1,
        ),
    ],
    "src/include/wiredtiger.in": [
        (
            "#define\tWT_STAT_SESSION_CACHE_TIME\t\t\t4007\n",
            "#define\tWT_STAT_SESSION_CACHE_TIME\t\t\t4007\n"
            f"{PATCH_MARKER}\n"
            "/*! session: number of updates in this txn */\n"
            "#define\tWT_STAT_SESSION_TXN_UPDATES\t\t\t4008\n",
            1,
        )
    ],
    "src/include/btree_inline.h": [(HELPER_ANCHOR, HELPER + HELPER_ANCHOR, 1)],
    "src/include/serial_inline.h": [
        (
            "    __wt_cache_page_inmem_incr(session, page, new_ins_size);\n",
            "    __wt_cache_page_inmem_incr(session, page, new_ins_size);\n"
            "    __wt_cache_page_inmem_incr_txn_update(session, page, new_ins_size); "
            f"{PATCH_MARKER}\n",
            2,
        ),
        (
            "    __wt_cache_page_inmem_incr(session, page, upd_size);\n",
            "    __wt_cache_page_inmem_incr(session, page, upd_size);\n"
            f"    __wt_cache_page_inmem_incr_txn_update(session, page, upd_size); {PATCH_MARKER}\n",
            1,
        ),
    ],
    "src/session/session_api.c": [
        (
            "    WT_STAT_SESSION_SET(session, txn_bytes_dirty, 0);\n",
            "    WT_STAT_SESSION_SET(session, txn_bytes_dirty, 0);\n"
            f"    WT_STAT_SESSION_SET(session, txn_updates, 0); {PATCH_MARKER}\n",
            1,
        )
    ],
}


def patch(path: Path) -> str:
    posix = path.as_posix()
    edits = next((e for suffix, e in EDITS.items() if posix.endswith(suffix)), None)
    if edits is None:
        raise SystemExit(f"patch_wt_txn_updates: no edits for {path}")
    text = path.read_text()
    if PATCH_MARKER in text:
        return "already patched"
    for anchor, replacement, count in edits:
        found = text.count(anchor)
        if found != count:
            # Fail loudly: an upstream change must be looked at, not skipped.
            raise SystemExit(
                f"patch_wt_txn_updates: {path}: anchor found {found} times, want {count}:\n{anchor}"
            )
        text = text.replace(anchor, replacement)
    path.write_text(text)
    return f"patched txn_updates into {path}"


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <file>", file=sys.stderr)
        return 2
    print(patch(Path(sys.argv[1])))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
