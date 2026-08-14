"""Multi-document transaction registry.

MongoDB drivers run a transaction as a sequence of ordinary commands
that all carry the same envelope: ``lsid`` (the logical session id),
``txnNumber`` (monotonically increasing per session), and
``autocommit: false``. There is **no** standalone ``startTransaction``
command — the first statement of a transaction carries
``startTransaction: true`` alongside the envelope, and the transaction
ends with a ``commitTransaction`` / ``abortTransaction`` admin command
carrying the same ``lsid`` + ``txnNumber``.

``TransactionRegistry`` is the server-side state machine for that
protocol. It is deliberately storage-agnostic: the WiredTiger work
(begin / commit / rollback of the underlying WT transaction) is
injected as ``commit_func`` / ``rollback_func`` callables so the
registry can be unit-tested with fakes, mirroring how
``SessionRegistry`` takes an injectable clock.

State machine (spec-pinned by pymongo's ``transactions/unified``
suite):

* statement for an unknown / aborted ``(lsid, txnNumber)`` →
  251 ``NoSuchTransaction`` + ``TransientTransactionError`` label
* statement for a committed ``txnNumber`` → 256 ``TransactionCommitted``
* ``startTransaction`` with a ``txnNumber`` lower than the session's
  newest → 225 ``TransactionTooOld``
* re-``startTransaction`` of the in-progress ``txnNumber`` → 50911
* ``startTransaction`` with a higher ``txnNumber`` while an older
  transaction is in progress → the older one is implicitly aborted
* ``commitTransaction`` on a committed transaction → ``{ok: 1}``
  (drivers retry commits; idempotency is load-bearing)
* ``commitTransaction`` on an aborted / unknown transaction →
  251 + ``TransientTransactionError``
* ``abortTransaction`` on an aborted / unknown transaction → 251
  with **no** label (drivers swallow abort errors)

Transactions that idle past ``lifetime_seconds`` (default 60, the
spirit of mongod's ``transactionLifetimeLimitSeconds``) are reaped
opportunistically on every registry access — same no-background-sweeper
pattern as cursors and sessions. Connection close does NOT abort a
transaction: pymongo may legally send a transaction's statements and
its (retryable) commit on different pooled connections.
"""

from __future__ import annotations

import contextlib
import copy
import enum
import threading
import time
from collections.abc import Callable
from typing import Any

# mongod default is transactionLifetimeLimitSeconds=60.
DEFAULT_LIFETIME_SECONDS = 60.0

# How long a retryable-write record is kept. mongod expires these with the
# same 30-minute sweep it uses for transaction records; a driver that retries
# later than this re-executes, which is exactly mongod's behaviour too.
_RETRYABLE_RECORD_LIFETIME_SECONDS = 30 * 60.0

# Backstop on record count so a client minting unbounded sessions cannot grow
# the map without limit. Oldest-first eviction.
_RETRYABLE_RECORD_MAX = 10_000


def _is_recordable_reply(reply: dict[str, Any]) -> bool:
    """Whether ``reply`` represents a write that fully took effect.

    Only those are replayable. A failed or partially-failed write must
    re-execute on retry: caching an error would make a transient failure
    permanent, and caching a partial batch would report missing documents as
    written.
    """
    if not isinstance(reply, dict):
        return False
    try:
        if float(reply.get("ok", 0)) != 1.0:
            return False
    except (TypeError, ValueError):
        return False
    # A writeConcernError means the write applied but replication of it did
    # not confirm. mongod still records the statement — the retry must not
    # apply it twice — so this is deliberately NOT a disqualifier.
    return not reply.get("writeErrors")


TRANSIENT_LABEL = "TransientTransactionError"


class TxnState(enum.Enum):
    IN_PROGRESS = "inProgress"
    COMMITTED = "committed"
    ABORTED = "aborted"


class Transaction:
    """One transaction's server-side state.

    ``handle`` is the opaque storage-side transaction handle (the WT
    session wrapper); it is created lazily by the command layer at the
    first statement so the WT snapshot pins there, not at registry
    time. ``mutex`` serializes statement execution / commit / abort /
    reaping on this transaction; state transitions happen only while
    it is held.
    """

    __slots__ = (
        "lsid_bytes",
        "lsid_doc",
        "txn_number",
        "state",
        "mutex",
        "handle",
        "started_at",
        "last_use",
    )

    def __init__(
        self,
        lsid_bytes: bytes,
        lsid_doc: dict[str, Any] | None,
        txn_number: int,
        *,
        now: float,
    ) -> None:
        self.lsid_bytes = lsid_bytes
        self.lsid_doc = lsid_doc
        self.txn_number = txn_number
        self.state = TxnState.IN_PROGRESS
        self.mutex = threading.RLock()
        self.handle: Any = None
        self.started_at = now
        self.last_use = now


def no_such_transaction_reply(txn_number: int, *, label: bool) -> dict[str, Any]:
    err: dict[str, Any] = {
        "ok": 0.0,
        "errmsg": (
            f"Given transaction number {txn_number} does not match any in-progress transactions."
        ),
        "code": 251,
        "codeName": "NoSuchTransaction",
    }
    if label:
        err["errorLabels"] = [TRANSIENT_LABEL]
    return err


def _transaction_committed(txn_number: int) -> dict[str, Any]:
    return {
        "ok": 0.0,
        "errmsg": f"Transaction with {{ txnNumber: {txn_number} }} has been committed.",
        "code": 256,
        "codeName": "TransactionCommitted",
    }


def _transaction_too_old(txn_number: int, newest: int) -> dict[str, Any]:
    return {
        "ok": 0.0,
        "errmsg": (
            f"Cannot start transaction {txn_number} on session because a "
            f"newer transaction {newest} has already started."
        ),
        "code": 225,
        "codeName": "TransactionTooOld",
    }


def _cannot_restart(txn_number: int) -> dict[str, Any]:
    # mongod surfaces this as a Location assertion; codeName follows
    # its ``Location<code>`` convention for non-named codes.
    return {
        "ok": 0.0,
        "errmsg": (
            f"Cannot restart transaction {txn_number}: a transaction with "
            f"the same number is already in progress or has finished."
        ),
        "code": 50911,
        "codeName": "Location50911",
    }


class TransactionRegistry:
    """Thread-safe map of ``lsid_bytes`` → most-recent :class:`Transaction`.

    Lock discipline: the registry ``_lock`` guards the dicts; each
    transaction's ``mutex`` guards its ``state`` / ``handle``. Order is
    always ``_lock`` → ``txn.mutex`` (or ``txn.mutex`` alone) — never
    ``txn.mutex`` → ``_lock``, so transitions done while executing a
    statement can't deadlock with the reaper / ``abort_all``.
    """

    def __init__(
        self,
        *,
        commit_func: Callable[[Transaction], None] | None = None,
        rollback_func: Callable[[Transaction], None] | None = None,
        lifetime_seconds: float = DEFAULT_LIFETIME_SECONDS,
        time_func: Callable[[], float] | None = None,
    ) -> None:
        self._txns: dict[bytes, Transaction] = {}
        # Newest txnNumber ever seen per session (transactions and
        # retryable writes share the per-session sequence).
        self._last_number: dict[bytes, int] = {}
        # Retryable-write records: (lsid_bytes, txnNumber) -> (reply, stored_at).
        # mongod keeps the equivalent in ``config.transactions`` so a driver's
        # automatic retry gets the ORIGINAL reply instead of re-applying the
        # write. Without it a retried ``{$inc: {n: 1}}`` increments twice while
        # both replies claim ``nModified: 1`` — silent corruption.
        self._retryable: dict[tuple[bytes, int], tuple[dict[str, Any], float, bytes]] = {}
        self._lock = threading.Lock()
        self._commit = commit_func or (lambda txn: None)
        self._rollback = rollback_func or (lambda txn: None)
        self._lifetime = lifetime_seconds
        self._time = time_func if callable(time_func) else time.time

    # -- statement / commit / abort entry points ------------------------

    def for_statement(
        self,
        lsid_bytes: bytes,
        lsid_doc: dict[str, Any] | None,
        txn_number: int,
        *,
        start: bool,
    ) -> tuple[Transaction | None, dict[str, Any] | None]:
        """Resolve the transaction a statement should run in.

        Returns ``(txn, None)`` when the statement should execute
        inside ``txn``, or ``(None, error_reply)`` when the envelope is
        invalid for the current session state. The caller must
        re-validate ``txn.state`` under ``txn.mutex`` before running
        the statement — the reaper may abort between resolution and
        execution.
        """
        with self._lock:
            self._prune_locked()
            cur = self._txns.get(lsid_bytes)
            last = self._last_number.get(lsid_bytes, 0)
            if start:
                if txn_number < last:
                    return None, _transaction_too_old(txn_number, last)
                if cur is not None and cur.txn_number == txn_number:
                    with cur.mutex:
                        if cur.state is TxnState.COMMITTED:
                            return None, _transaction_committed(txn_number)
                    return None, _cannot_restart(txn_number)
                if txn_number == last and cur is None:
                    # The number was consumed by a retryable write.
                    return None, _cannot_restart(txn_number)
                if cur is not None:
                    self._abort_locked(cur)
                txn = Transaction(lsid_bytes, lsid_doc, txn_number, now=self._time())
                self._txns[lsid_bytes] = txn
                self._last_number[lsid_bytes] = txn_number
                return txn, None
            # Continuation statement (no startTransaction flag).
            if cur is None or cur.txn_number != txn_number:
                return None, no_such_transaction_reply(txn_number, label=True)
            with cur.mutex:
                if cur.state is TxnState.COMMITTED:
                    return None, _transaction_committed(txn_number)
                if cur.state is TxnState.ABORTED:
                    return None, no_such_transaction_reply(txn_number, label=True)
                cur.last_use = self._time()
            return cur, None

    def commit(self, lsid_bytes: bytes, txn_number: int) -> dict[str, Any] | None:
        """Commit ``(lsid, txnNumber)``. Returns an error reply or None (ok).

        Idempotent on an already-committed transaction. If
        ``commit_func`` raises, the transaction is rolled back and
        marked aborted (a failed commit cannot be retried into
        success at the WT layer) and the exception propagates for the
        command layer to classify.
        """
        with self._lock:
            self._prune_locked()
            cur = self._txns.get(lsid_bytes)
        if cur is None or cur.txn_number != txn_number:
            return no_such_transaction_reply(txn_number, label=True)
        with cur.mutex:
            if cur.state is TxnState.COMMITTED:
                return None
            if cur.state is TxnState.ABORTED:
                return no_such_transaction_reply(txn_number, label=True)
            try:
                self._commit(cur)
            except Exception:
                self._rollback_quietly(cur)
                cur.state = TxnState.ABORTED
                raise
            cur.state = TxnState.COMMITTED
            cur.last_use = self._time()
        return None

    def abort(self, lsid_bytes: bytes, txn_number: int) -> dict[str, Any] | None:
        """Abort ``(lsid, txnNumber)``. Returns an error reply or None (ok)."""
        with self._lock:
            self._prune_locked()
            cur = self._txns.get(lsid_bytes)
        if cur is None or cur.txn_number != txn_number:
            # No transient label: drivers fire-and-forget aborts.
            return no_such_transaction_reply(txn_number, label=False)
        with cur.mutex:
            if cur.state is TxnState.COMMITTED:
                return _transaction_committed(txn_number)
            if cur.state is TxnState.ABORTED:
                return no_such_transaction_reply(txn_number, label=False)
            self._rollback_quietly(cur)
            cur.state = TxnState.ABORTED
            cur.last_use = self._time()
        return None

    def abort_in_progress(self, txn: Transaction) -> None:
        """Server-side abort after a failed statement (mongod parity:
        any failed statement aborts the transaction). No-op if the
        transaction already reached a terminal state."""
        with txn.mutex:
            if txn.state is TxnState.IN_PROGRESS:
                self._rollback_quietly(txn)
                txn.state = TxnState.ABORTED

    def on_retryable_write(self, lsid_bytes: bytes, txn_number: int) -> None:
        """A retryable write (``txnNumber`` without ``autocommit``)
        consumes the session's txnNumber sequence and implicitly aborts
        an older in-progress transaction, as in mongod."""
        with self._lock:
            cur = self._txns.get(lsid_bytes)
            if cur is not None and txn_number > cur.txn_number:
                self._abort_locked(cur)
                self._txns.pop(lsid_bytes, None)
            last = self._last_number.get(lsid_bytes, 0)
            if txn_number > last:
                self._last_number[lsid_bytes] = txn_number

    # -- retryable-write records -----------------------------------------

    def retryable_reply(
        self, lsid_bytes: bytes, txn_number: int, identity: bytes
    ) -> dict[str, Any] | None:
        """The stored reply for an already-executed retryable write, if any.

        A driver retries with the SAME ``lsid`` + ``txnNumber`` after a network
        blip, a ``writeConcernError``, or a stepdown. mongod recognises the
        repeat and replays its stored reply rather than executing the write a
        second time; returning ``None`` here means "not seen before, run it".

        ``identity`` must match the recorded command too. A retry re-sends a
        byte-identical command, so a mismatch means the key was reused for a
        DIFFERENT write — and replaying one command's reply for another would
        be worse than the double-apply this exists to prevent. On a mismatch
        we execute normally rather than serve the wrong answer.
        """
        with self._lock:
            self._prune_retryable_locked()
            entry = self._retryable.get((lsid_bytes, txn_number))
            if entry is None or entry[2] != identity:
                return None
            return copy.deepcopy(entry[0])

    def record_retryable(
        self, lsid_bytes: bytes, txn_number: int, identity: bytes, reply: dict[str, Any]
    ) -> None:
        """Store ``reply`` as the outcome of this retryable write.

        Only *successful* writes are recorded. A write that failed did not
        take effect, so its retry must genuinely re-execute — caching the
        failure would turn a transient error into a permanent one. Partial
        batch failures (``writeErrors`` present) are likewise not recorded:
        mongod tracks per-statement ids and would retry only the missing
        documents, which we do not model, so the whole batch re-runs exactly
        as it does today rather than being wrongly reported as complete.
        """
        if not _is_recordable_reply(reply):
            return
        with self._lock:
            self._retryable[(lsid_bytes, txn_number)] = (
                copy.deepcopy(reply),
                self._time(),
                identity,
            )
            self._prune_retryable_locked()

    def _prune_retryable_locked(self) -> None:
        """Drop records past their lifetime, and cap total size.

        Called on every lookup / record, so an idle server sheds them without
        a background sweeper — the same opportunistic pattern the oplog and
        TTL pruning use. The cap is a backstop against a client that never
        stops minting sessions.
        """
        now = self._time()
        cutoff = now - _RETRYABLE_RECORD_LIFETIME_SECONDS
        stale = [k for k, (_r, at, _i) in self._retryable.items() if at < cutoff]
        for k in stale:
            self._retryable.pop(k, None)
        excess = len(self._retryable) - _RETRYABLE_RECORD_MAX
        if excess > 0:
            oldest = sorted(self._retryable.items(), key=lambda kv: kv[1][1])[:excess]
            for k, _v in oldest:
                self._retryable.pop(k, None)

    # -- bulk lifecycle --------------------------------------------------

    def abort_for_session(self, lsid_bytes: bytes) -> None:
        """``endSessions`` / ``killSessions``: abort the session's
        in-progress transaction, if any."""
        with self._lock:
            cur = self._txns.get(lsid_bytes)
            if cur is not None:
                self._abort_locked(cur)

    def abort_all(self) -> None:
        """``killAllSessions`` / server shutdown: abort everything."""
        with self._lock:
            for txn in self._txns.values():
                self._abort_locked(txn)

    def prune_expired(self) -> int:
        """Abort in-progress transactions idle past ``lifetime_seconds``.

        Returns the number reaped. Also called opportunistically from
        every statement / commit / abort resolution.
        """
        with self._lock:
            return self._prune_locked()

    # -- internals --------------------------------------------------------

    def _prune_locked(self) -> int:
        cutoff = self._time() - self._lifetime
        reaped = 0
        for txn in self._txns.values():
            with txn.mutex:
                if txn.state is TxnState.IN_PROGRESS and txn.last_use < cutoff:
                    self._rollback_quietly(txn)
                    txn.state = TxnState.ABORTED
                    reaped += 1
        return reaped

    def _abort_locked(self, txn: Transaction) -> None:
        with txn.mutex:
            if txn.state is TxnState.IN_PROGRESS:
                self._rollback_quietly(txn)
                txn.state = TxnState.ABORTED

    def _rollback_quietly(self, txn: Transaction) -> None:
        # Defensive: a rollback failure must not mask the state
        # transition (the WT session sweep in Storage.close() is the
        # backstop for leaked sessions).
        with contextlib.suppress(Exception):
            self._rollback(txn)

    def __len__(self) -> int:
        with self._lock:
            return sum(1 for t in self._txns.values() if t.state is TxnState.IN_PROGRESS)


__all__ = [
    "DEFAULT_LIFETIME_SECONDS",
    "TRANSIENT_LABEL",
    "Transaction",
    "TransactionRegistry",
    "TxnState",
]
