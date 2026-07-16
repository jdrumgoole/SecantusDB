"""Commit-time write-conflict mapping (`storage._commit_batch_transaction`).

A concurrent WT transaction can mark a batch transaction rollback-only
after its last cursor operation; the conflict then surfaces at
``commit_transaction`` as a bare ``WiredTigerError("Invalid argument")``
with no WT_ROLLBACK marker in the message. These tests pin the mapping:
commit failures that carry a rollback reason become ``WriteConflictError``
(retryable by ``_retry_write_conflicts``); commit failures without one are
genuine durability errors and re-raise unchanged.
"""

from __future__ import annotations

import pytest

from secantus import storage as storage_mod
from secantus.storage import WriteConflictError, _commit_batch_transaction


class _StubSession:
    def __init__(
        self,
        *,
        commit_exc: Exception | None,
        rollback_reason: str | None,
        reason_raises: bool = False,
    ) -> None:
        self._commit_exc = commit_exc
        self._rollback_reason = rollback_reason
        self._reason_raises = reason_raises
        self.commit_config: str | None = "unset"
        self.rolled_back = False

    def commit_transaction(self, config: str | None = None) -> None:
        self.commit_config = config
        if self._commit_exc is not None:
            raise self._commit_exc

    def get_rollback_reason(self) -> str | None:
        if self._reason_raises:
            raise RuntimeError("no reason available")
        return self._rollback_reason

    def rollback_transaction(self) -> None:
        self.rolled_back = True
        # Mirrors WT: the failed commit already rolled the txn back, so an
        # explicit rollback raises.
        raise storage_mod.wt.WiredTigerError("no transaction is active")


def test_commit_conflict_with_reason_maps_to_write_conflict() -> None:
    session = _StubSession(
        commit_exc=storage_mod.wt.WiredTigerError("Invalid argument"),
        rollback_reason="conflict between concurrent operations",
    )
    with pytest.raises(WriteConflictError, match="conflict between concurrent"):
        _commit_batch_transaction(session, sync=False)
    assert session.rolled_back


def test_commit_einval_with_cleared_reason_maps() -> None:
    """The production shape: an internally rollback-marked transaction
    fails commit with bare EINVAL, and the auto-rollback has already
    cleared the rollback reason."""
    session = _StubSession(
        commit_exc=storage_mod.wt.WiredTigerError("Invalid argument"),
        rollback_reason=None,
    )
    with pytest.raises(WriteConflictError):
        _commit_batch_transaction(session, sync=False)


def test_commit_rollback_marker_without_reason_still_maps() -> None:
    session = _StubSession(
        commit_exc=storage_mod.wt.WiredTigerError(
            "WT_ROLLBACK: conflict between concurrent operations"
        ),
        rollback_reason=None,
    )
    with pytest.raises(WriteConflictError):
        _commit_batch_transaction(session, sync=False)


def test_commit_failure_without_reason_stays_loud() -> None:
    exc = storage_mod.wt.WiredTigerError("disk I/O failure")
    session = _StubSession(commit_exc=exc, rollback_reason=None)
    with pytest.raises(storage_mod.wt.WiredTigerError, match="disk I/O failure"):
        _commit_batch_transaction(session, sync=False)


def test_commit_failure_reason_lookup_error_stays_loud() -> None:
    exc = storage_mod.wt.WiredTigerError("disk I/O failure")
    session = _StubSession(commit_exc=exc, rollback_reason=None, reason_raises=True)
    with pytest.raises(storage_mod.wt.WiredTigerError, match="disk I/O failure"):
        _commit_batch_transaction(session, sync=False)


def test_successful_commit_passes_sync_config() -> None:
    session = _StubSession(commit_exc=None, rollback_reason=None)
    _commit_batch_transaction(session, sync=True)
    assert session.commit_config == "sync=on"
    _commit_batch_transaction(session, sync=False)
    assert session.commit_config is None


def test_commit_panic_stays_loud() -> None:
    # The SWIG binding has no panic subclass; panics are base
    # WiredTigerError with a WT_PANIC message and must never be
    # classified as retryable conflicts.
    exc = storage_mod.wt.WiredTigerError("WT_PANIC: fatal error: Invalid argument")
    session = _StubSession(commit_exc=exc, rollback_reason=None)
    with pytest.raises(storage_mod.wt.WiredTigerError, match="WT_PANIC"):
        _commit_batch_transaction(session, sync=False)
