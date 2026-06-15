"""Unit tests for the transaction state machine (no storage, no wire).

The registry's commit / rollback callables are fakes that record calls,
and the clock is injected, so every spec-pinned transition in
``secantus.transactions`` is driven deterministically here. The
pymongo-driven conformance proof lives in ``tests/test_transactions.py``.
"""

from __future__ import annotations

from secantus.transactions import (
    TRANSIENT_LABEL,
    Transaction,
    TransactionRegistry,
    TxnState,
)

LSID_A = b"a" * 16
LSID_B = b"b" * 16


class FakeClock:
    def __init__(self) -> None:
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now


class Recorder:
    def __init__(self) -> None:
        self.committed: list[Transaction] = []
        self.rolled_back: list[Transaction] = []

    def commit(self, txn: Transaction) -> None:
        self.committed.append(txn)

    def rollback(self, txn: Transaction) -> None:
        self.rolled_back.append(txn)


def make_registry(clock: FakeClock | None = None, lifetime: float = 60.0):
    rec = Recorder()
    reg = TransactionRegistry(
        commit_func=rec.commit,
        rollback_func=rec.rollback,
        lifetime_seconds=lifetime,
        time_func=clock or FakeClock(),
    )
    return reg, rec


def begin(reg, lsid=LSID_A, n=1) -> Transaction:
    txn, err = reg.for_statement(lsid, {"id": lsid}, n, start=True)
    assert err is None
    assert txn is not None
    return txn


def test_begin_and_continue():
    reg, _ = make_registry()
    txn = begin(reg)
    assert txn.state is TxnState.IN_PROGRESS
    again, err = reg.for_statement(LSID_A, {"id": LSID_A}, 1, start=False)
    assert err is None
    assert again is txn


def test_statement_unknown_txn_is_251_with_label():
    reg, _ = make_registry()
    txn, err = reg.for_statement(LSID_A, None, 7, start=False)
    assert txn is None
    assert err["code"] == 251
    assert err["codeName"] == "NoSuchTransaction"
    assert err["errorLabels"] == [TRANSIENT_LABEL]


def test_statement_for_committed_txn_is_256():
    reg, _ = make_registry()
    begin(reg)
    assert reg.commit(LSID_A, 1) is None
    txn, err = reg.for_statement(LSID_A, None, 1, start=False)
    assert txn is None
    assert err["code"] == 256
    assert err["codeName"] == "TransactionCommitted"


def test_statement_for_aborted_txn_is_251_with_label():
    reg, _ = make_registry()
    begin(reg)
    assert reg.abort(LSID_A, 1) is None
    txn, err = reg.for_statement(LSID_A, None, 1, start=False)
    assert txn is None
    assert err["code"] == 251
    assert err["errorLabels"] == [TRANSIENT_LABEL]


def test_start_with_older_number_is_225():
    reg, _ = make_registry()
    begin(reg, n=5)
    txn, err = reg.for_statement(LSID_A, None, 3, start=True)
    assert txn is None
    assert err["code"] == 225
    assert err["codeName"] == "TransactionTooOld"


def test_restart_in_progress_number_is_50911():
    reg, _ = make_registry()
    begin(reg, n=1)
    txn, err = reg.for_statement(LSID_A, None, 1, start=True)
    assert txn is None
    assert err["code"] == 50911


def test_restart_committed_number_is_256():
    reg, _ = make_registry()
    begin(reg, n=1)
    reg.commit(LSID_A, 1)
    txn, err = reg.for_statement(LSID_A, None, 1, start=True)
    assert txn is None
    assert err["code"] == 256


def test_start_higher_number_implicitly_aborts_older():
    reg, rec = make_registry()
    old = begin(reg, n=1)
    new = begin(reg, n=2)
    assert old.state is TxnState.ABORTED
    assert rec.rolled_back == [old]
    assert new.state is TxnState.IN_PROGRESS
    # The old number is now unknown.
    _, err = reg.for_statement(LSID_A, None, 1, start=False)
    assert err["code"] == 251


def test_commit_is_idempotent():
    reg, rec = make_registry()
    txn = begin(reg)
    assert reg.commit(LSID_A, 1) is None
    assert reg.commit(LSID_A, 1) is None
    assert rec.committed == [txn]  # the WT commit ran exactly once
    assert txn.state is TxnState.COMMITTED


def test_commit_aborted_or_unknown_is_251_with_label():
    reg, _ = make_registry()
    err = reg.commit(LSID_A, 9)
    assert err["code"] == 251
    assert err["errorLabels"] == [TRANSIENT_LABEL]
    begin(reg, n=1)
    reg.abort(LSID_A, 1)
    err = reg.commit(LSID_A, 1)
    assert err["code"] == 251
    assert err["errorLabels"] == [TRANSIENT_LABEL]


def test_commit_failure_aborts_and_reraises():
    boom = RuntimeError("WT commit blew up")

    def bad_commit(txn):
        raise boom

    rolled = []
    reg = TransactionRegistry(
        commit_func=bad_commit,
        rollback_func=rolled.append,
        time_func=FakeClock(),
    )
    txn, _ = reg.for_statement(LSID_A, None, 1, start=True)
    try:
        reg.commit(LSID_A, 1)
    except RuntimeError as exc:
        assert exc is boom
    else:  # pragma: no cover
        raise AssertionError("commit_func error must propagate")
    assert txn.state is TxnState.ABORTED
    assert rolled == [txn]
    # A retried commit now reports NoSuchTransaction.
    err = reg.commit(LSID_A, 1)
    assert err["code"] == 251


def test_abort_unknown_or_aborted_is_251_without_label():
    reg, _ = make_registry()
    err = reg.abort(LSID_A, 4)
    assert err["code"] == 251
    assert "errorLabels" not in err
    begin(reg, n=1)
    assert reg.abort(LSID_A, 1) is None
    err = reg.abort(LSID_A, 1)
    assert err["code"] == 251
    assert "errorLabels" not in err


def test_abort_committed_is_256():
    reg, _ = make_registry()
    begin(reg)
    reg.commit(LSID_A, 1)
    err = reg.abort(LSID_A, 1)
    assert err["code"] == 256


def test_abort_in_progress_after_failed_statement():
    reg, rec = make_registry()
    txn = begin(reg)
    reg.abort_in_progress(txn)
    assert txn.state is TxnState.ABORTED
    assert rec.rolled_back == [txn]
    # Terminal states are sticky.
    reg.abort_in_progress(txn)
    assert rec.rolled_back == [txn]
    err = reg.commit(LSID_A, 1)
    assert err["code"] == 251


def test_retryable_write_aborts_older_txn_and_consumes_number():
    reg, rec = make_registry()
    old = begin(reg, n=1)
    reg.on_retryable_write(LSID_A, 2)
    assert old.state is TxnState.ABORTED
    assert rec.rolled_back == [old]
    # Starting a txn at or below the consumed number is rejected.
    _, err = reg.for_statement(LSID_A, None, 1, start=True)
    assert err["code"] == 225
    _, err = reg.for_statement(LSID_A, None, 2, start=True)
    assert err["code"] == 50911
    txn, err = reg.for_statement(LSID_A, None, 3, start=True)
    assert err is None and txn is not None


def test_sessions_are_independent():
    reg, _ = make_registry()
    a = begin(reg, lsid=LSID_A, n=1)
    b = begin(reg, lsid=LSID_B, n=1)
    assert a is not b
    assert reg.commit(LSID_A, 1) is None
    assert b.state is TxnState.IN_PROGRESS


def test_abort_for_session_and_abort_all():
    reg, rec = make_registry()
    a = begin(reg, lsid=LSID_A)
    b = begin(reg, lsid=LSID_B)
    reg.abort_for_session(LSID_A)
    assert a.state is TxnState.ABORTED
    assert b.state is TxnState.IN_PROGRESS
    reg.abort_all()
    assert b.state is TxnState.ABORTED
    assert rec.rolled_back == [a, b]


def test_lifetime_reaping_with_injectable_clock():
    clock = FakeClock()
    reg, rec = make_registry(clock=clock, lifetime=60.0)
    txn = begin(reg)
    clock.now += 30
    # Activity refreshes last_use.
    again, err = reg.for_statement(LSID_A, None, 1, start=False)
    assert err is None and again is txn
    clock.now += 59
    assert reg.prune_expired() == 0
    assert txn.state is TxnState.IN_PROGRESS
    clock.now += 2
    assert reg.prune_expired() == 1
    assert txn.state is TxnState.ABORTED
    assert rec.rolled_back == [txn]
    # The expired txn now answers like an aborted one.
    err = reg.commit(LSID_A, 1)
    assert err["code"] == 251
    assert err["errorLabels"] == [TRANSIENT_LABEL]


def test_len_counts_only_in_progress():
    reg, _ = make_registry()
    begin(reg, lsid=LSID_A)
    begin(reg, lsid=LSID_B)
    assert len(reg) == 2
    reg.commit(LSID_A, 1)
    assert len(reg) == 1
