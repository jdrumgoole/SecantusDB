"""Idempotent patch for WiredTiger's log: one ``fsync`` covers every commit
already written, under ``method=fsync``.

Targets ``src/log/log.c``.

With ``transaction_sync=(method=fsync)`` every committer writes its own log
slot and then syncs under ``log_sync_lock``. Stock WiredTiger records each
sync as covering only the syncing thread's OWN slot, although the ``fsync``
it has just made flushed every byte written to the file before the call. So
when commits B and C are written while A's sync is in flight, B syncs (which
flushes C's bytes too), records B, and C syncs again. Measured on Linux
(2026-10-09, DigitalOcean c-16, ext4, durable UPDATE by primary key): 1.8
commits per ``fdatasync`` at eight clients where PostgreSQL makes 2.7, and 3.1
against 5.7 at sixteen. PostgreSQL flushes to the end of what is written.

What the patch changes in ``__wt_log_release``, the sync of a written slot:

1. Before the ``fsync`` the thread notes ``write_lsn``: every byte below it
   has been written (it advances only when a slot's write has finished and
   every earlier slot's has). After its own ``fsync`` returns, ``sync_lsn``
   moves to that noted LSN instead of the slot's end, provided the noted LSN
   is in the slot's log file and that file is still the current one
   (``log->fileid``), so the handle that was synced is the one those bytes
   went to. Otherwise it stays at the slot's end, as stock.
2. A slot that needs only a file sync leaves the loop without taking
   ``log_sync_lock`` once ``sync_lsn`` has passed its end.
3. A thread that loses the try-lock waits 200 us for the signal, not 10 ms.
   Stock code could afford 10 ms because the lock holder's sync was always
   followed by the next thread's and its signal; with fewer syncs a missed
   signal would otherwise stall a commit for the full 10 ms.

What it does NOT change:

- A commit returns only after ``sync_lsn`` has passed its record, and
  ``sync_lsn`` only ever names bytes that were written before an ``fsync``
  of their file began.
- The directory sync, the sync of a log file being closed, ``method=dsync``
  and ``method=none``, and unsynced commits.

Reapplying is a no-op: the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "/* secantus-patch: fsync covers what is written */"

# 1. Locals.
DECL_ANCHOR = """    WT_LSN sync_lsn;
    int64_t release_buffered, release_bytes;
    bool locked;
"""
DECL = f"""    WT_LSN sync_lsn;
    WT_LSN written_lsn; {PATCH_MARKER}
    int64_t release_buffered, release_bytes;
    bool locked;
    bool own_sync;
"""

# 2. Leave without the lock when already covered; a short wait for the lock.
WAIT_ANCHOR = """        if (log->sync_lsn.l.file < slot->slot_end_lsn.l.file ||
          __wt_spin_trylock(session, &log->log_sync_lock) != 0) {
            __wt_cond_wait(session, log->log_sync_cond, 10 * WT_THOUSAND, NULL);
            continue;
        }
"""
WAIT = f"""        {PATCH_MARKER}
        if (!F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC_DIR) &&
          __wt_log_cmp(&log->sync_lsn, &slot->slot_end_lsn) >= 0) {{
            F_CLR_ATOMIC_16(slot, WT_SLOT_SYNC);
            break;
        }}
        if (log->sync_lsn.l.file < slot->slot_end_lsn.l.file ||
          __wt_spin_trylock(session, &log->log_sync_lock) != 0) {{
            __wt_cond_wait(session, log->log_sync_cond, 200, NULL);
            continue;
        }}
"""

# 3. Note what is written before the sync, and credit it afterwards.
SYNC_ANCHOR = """        if (F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC))
            WT_ERR(__log_fsync_file(session, &sync_lsn, "log_release", false));
"""
SYNC = f"""        if (F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC)) {{
            {PATCH_MARKER}
            own_sync = __wt_log_cmp(&log->sync_lsn, &sync_lsn) < 0;
            WT_ASSIGN_LSN(&written_lsn, &log->write_lsn);
            WT_ERR(__log_fsync_file(session, &sync_lsn, "log_release", false));
            if (own_sync && written_lsn.l.file == sync_lsn.l.file &&
              __wt_log_cmp(&written_lsn, &sync_lsn) > 0 && log->fileid == sync_lsn.l.file &&
              __wt_log_cmp(&log->sync_lsn, &sync_lsn) == 0) {{
                WT_ASSIGN_LSN(&log->sync_lsn, &written_lsn);
                __wt_cond_signal(session, log->log_sync_cond);
            }}
        }}
"""

EDITS = ((DECL_ANCHOR, DECL), (WAIT_ANCHOR, WAIT), (SYNC_ANCHOR, SYNC))


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-log.c>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    if path.name != "log.c":
        print(f"error: no fsync group edits for {path.name}", file=sys.stderr)
        return 1
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0
    # Every anchor must match exactly once BEFORE anything is written: a
    # half-applied commit-path patch must never reach a compiler.
    for anchor, _ in EDITS:
        if text.count(anchor) != 1:
            print(
                f"error: anchor found {text.count(anchor)} times in {path}:\n{anchor}",
                file=sys.stderr,
            )
            return 1
    for anchor, replacement in EDITS:
        text = text.replace(anchor, replacement)
    tmp = path.with_name(path.name + ".secantus-tmp")
    tmp.write_text(text)
    tmp.replace(path)
    print(f"patched fsync group sync into {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
