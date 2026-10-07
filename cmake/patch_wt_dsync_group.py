"""Idempotent patch for WiredTiger's log: group commit under ``method=dsync``.

Targets ``src/log/log.c`` and ``src/log/log_slot.c`` (run once per file; the
file name selects the edits).

With ``transaction_sync=(method=dsync)`` the log file is opened ``O_DSYNC``, so
the ``pwrite`` of a log slot IS the durable write. Stock WiredTiger closes the
active slot on every synced commit (``force`` in ``__log_write_internal``), so
N concurrent committers make N slots and N synchronous writes, each of which
then waits for every earlier one. Measured on macOS (2026-10-07, release
``secantusd-pg``, INSERT, one table per client): 30.9k commits/s at 8 clients
stock, 37.3k patched; 1 to 4 clients unchanged.

What the patch changes, for a ``WT_LOG_DSYNC`` commit only:

1. The committer joins the active slot, copies its record in and releases its
   share, WITHOUT closing the slot. Commits that arrive while an earlier
   synchronous write is in flight therefore accumulate in one slot.
2. It then waits for its record to be written. Whichever waiter first finds
   every earlier slot written closes the slot (the existing forced-switch
   code, under a try-lock) and writes the whole group with one ``pwrite``.
3. ``__wt_log_release`` skips ``log_sync_lock`` when the slot needs only a
   directory sync and that sync has already happened for its log file. Stock
   code took a try-lock for that no-op and, on losing it, slept on
   ``log_sync_cond``, which nothing signals in dsync mode: a 10 ms stall.

What it does NOT change:

- A commit returns, and its updates become visible, only after ``write_lsn``
  has passed its record, which under ``O_DSYNC`` means the record is durable.
  A grouped committer also waits for the directory sync of its log file.
- ``method=fsync`` (the Rust and Python MongoDB servers, and the PostgreSQL
  server off macOS), ``method=none`` and unsynced commits run the stock code.
  ``WT_LOG_DSYNC`` is set in exactly one place, ``conn_log.c``, for
  ``method=dsync``.

Liveness: a waiter that cannot close its slot through the fast path (the slot
lock is held, or another thread is mid-copy) retries, and after 1000 yields
falls back to ``__wt_log_force_write``, the blocking close a log flush uses.

Reapplying is a no-op: the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "/* secantus-patch: dsync group commit */"

# --------------------------------------------------------------------------
# log_slot.c: a forced switch of one slot that does not wait for the slot lock.
# --------------------------------------------------------------------------
SLOT_ANCHOR = """/*
 * __wt_log_slot_init --
"""
SLOT_HELPER = f"""{PATCH_MARKER}
/*
 * __wt_log_slot_group_close --
 *     Forced switch of the given slot if it is still the active one, without waiting for the slot
 *     lock. Returns EBUSY if the lock is held or a thread is still copying into the slot.
 */
extern int __wt_log_slot_group_close(WT_SESSION_IMPL *session, WT_LOGSLOT *slot);
int
__wt_log_slot_group_close(WT_SESSION_IMPL *session, WT_LOGSLOT *slot)
{{
    WT_DECL_RET;
    WT_LOG *log;
    WT_MYSLOT myslot;

    log = S2C(session)->log;
    if (__wt_spin_trylock(session, &log->log_slot_lock) != 0)
        return (EBUSY);
    FLD_SET(session->lock_flags, WT_SESSION_LOCKED_SLOT);
    memset(&myslot, 0, sizeof(myslot));
    myslot.slot = slot;
    /* As in __wt_log_slot_switch: a slot we closed must get its replacement. */
    do {{
        ret = __log_slot_switch_internal(session, &myslot, true, NULL);
    }} while (ret == 0 && F_ISSET(&myslot, WT_MYSLOT_CLOSE));
    FLD_CLR(session->lock_flags, WT_SESSION_LOCKED_SLOT);
    __wt_spin_unlock(session, &log->log_slot_lock);
    if (ret != 0 && ret != EBUSY)
        WT_TRET(__wt_panic(session, ret, "log slot group close fatal error"));
    return (ret);
}}

"""

# --------------------------------------------------------------------------
# log.c
# --------------------------------------------------------------------------
# 1. The waiter, ahead of __log_write_internal.
LOG_HELPER_ANCHOR = """/*
 * __log_write_internal --
"""
LOG_HELPER = f"""{PATCH_MARKER}
extern int __wt_log_slot_group_close(WT_SESSION_IMPL *session, WT_LOGSLOT *slot);
/*
 * __log_dsync_group_commit --
 *     The caller's record is copied into its slot and released, and the slot was left open so
 *     commits arriving during an in-flight synchronous write share the next one. Wait for the
 *     record to be durable, closing (and so writing) the slot once every earlier slot is written.
 */
static int
__log_dsync_group_commit(WT_SESSION_IMPL *session, WT_LOGSLOT *slot, WT_LSN *lsn)
{{
    WT_CONNECTION_IMPL *conn;
    WT_DECL_RET;
    WT_LOG *log;
    uint32_t waits;

    conn = S2C(session);
    log = conn->log;
    for (waits = 0;; ++waits) {{
        if (slot->slot_error != 0)
            return (0);
        /* Written, and the log file's directory entry is synced: durable. */
        if (__wt_log_cmp(&log->write_lsn, lsn) > 0 && log->sync_dir_lsn.l.file >= lsn->l.file)
            return (0);
        WT_RET(WT_SESSION_CHECK_PANIC(session));
        if (slot == log->active_slot &&
          __wt_log_cmp(&log->write_lsn, &slot->slot_release_lsn) == 0) {{
            ret = __wt_log_slot_group_close(session, slot);
            if (ret == 0)
                continue;
            WT_RET_BUSY_OK(ret);
        }}
        if (waits < WT_THOUSAND)
            __wt_yield();
        else {{
            /* Unsynced earlier slots are advanced by the wrlsn thread, as in the stock wait. */
            if (conn->log_wrlsn_cond != NULL)
                __wt_cond_signal(session, conn->log_wrlsn_cond);
            /* Past the fast phase, close the slot the way a log flush does. */
            if (waits % 50 == 0 && slot == log->active_slot)
                WT_RET(__wt_log_force_write(session, 1, NULL));
            __wt_cond_wait(session, log->log_write_cond, 200, NULL);
        }}
    }}
    /* NOTREACHED */
}}

"""

# 2. A local for the decision.
LOG_DECL_ANCHOR = """    uint32_t fill_size, force, rdup_len;
    bool free_slot;
"""
LOG_DECL = (
    LOG_DECL_ANCHOR
    + f"""    bool grouped; {PATCH_MARKER}
"""
)

# 3. Do not close the slot for a grouped commit.
LOG_FORCE_ANCHOR = """    force = LF_ISSET(WT_LOG_FLUSH | WT_LOG_FSYNC);
    ret = 0;
"""
LOG_FORCE = (
    LOG_FORCE_ANCHOR
    + f"""    {PATCH_MARKER}
    grouped = LF_ISSET(WT_LOG_DSYNC) && LF_ISSET(WT_LOG_FLUSH) && !LF_ISSET(WT_LOG_FSYNC) &&
      !F_ISSET(&myslot, WT_MYSLOT_UNBUFFERED) && myslot.end_offset < WT_LOG_SLOT_BUF_MAX;
    if (grouped)
        force = 0;
"""
)

# 4. Wait for the group's write before the stock wait.
LOG_WAIT_ANCHOR = """    if (LF_ISSET(WT_LOG_FLUSH)) {
        /* Wait for our writes to reach the OS */
"""
LOG_WAIT = (
    f"""    {PATCH_MARKER}
    if (grouped && !WT_LOG_SLOT_DONE(release_size))
        WT_ERR(__log_dsync_group_commit(session, myslot.slot, &lsn));
"""
    + LOG_WAIT_ANCHOR
)

# 5. __wt_log_release: no lock for a directory sync that is already done.
LOG_DIRSYNC_ANCHOR = """    while (F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC | WT_SLOT_SYNC_DIR)) {
"""
LOG_DIRSYNC = (
    LOG_DIRSYNC_ANCHOR
    + f"""        {PATCH_MARKER}
        if (!F_ISSET_ATOMIC_16(slot, WT_SLOT_SYNC) &&
          log->sync_dir_lsn.l.file >= slot->slot_end_lsn.l.file) {{
            F_CLR_ATOMIC_16(slot, WT_SLOT_SYNC_DIR);
            break;
        }}
"""
)

EDITS = {
    "log_slot.c": ((SLOT_ANCHOR, SLOT_HELPER + SLOT_ANCHOR),),
    "log.c": (
        (LOG_HELPER_ANCHOR, LOG_HELPER + LOG_HELPER_ANCHOR),
        (LOG_DECL_ANCHOR, LOG_DECL),
        (LOG_FORCE_ANCHOR, LOG_FORCE),
        (LOG_WAIT_ANCHOR, LOG_WAIT),
        (LOG_DIRSYNC_ANCHOR, LOG_DIRSYNC),
    ),
}


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-log.c | path-to-log_slot.c>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    edits = EDITS.get(path.name)
    if edits is None:
        print(f"error: no dsync group-commit edits for {path.name}", file=sys.stderr)
        return 1
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0
    # Every anchor must match exactly once BEFORE anything is written: a
    # half-applied commit-path patch must never reach a compiler.
    for anchor, _ in edits:
        if text.count(anchor) != 1:
            print(
                f"error: anchor found {text.count(anchor)} times in {path}:\n{anchor}",
                file=sys.stderr,
            )
            return 1
    for anchor, replacement in edits:
        text = text.replace(anchor, replacement)
    tmp = path.with_name(path.name + ".secantus-tmp")
    tmp.write_text(text)
    tmp.replace(path)
    print(f"patched dsync group commit into {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
