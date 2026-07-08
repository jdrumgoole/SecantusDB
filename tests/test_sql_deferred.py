"""Deferred constraints — ``DEFERRABLE`` / ``INITIALLY DEFERRED`` and
``SET CONSTRAINTS``.

A deferrable constraint that is currently deferred does not raise on the offending
write; its check is postponed to COMMIT (or ``SET CONSTRAINTS … IMMEDIATE``). This
lets a transaction hold a temporarily-inconsistent state — insert a child before
its parent, or swap two UNIQUE values — as long as the books balance by the time
the block ends. A violation that survives to the check point aborts the block.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run(s, session, "CREATE TABLE parent (id bigint primary key, name text)")
    run(s, session, "INSERT INTO parent (id, name) VALUES (1, 'a')")
    try:
        yield s
    finally:
        s.close()


# -- deferred FK --------------------------------------------------------------- #


def _child(storage, session, *, deferrable="DEFERRABLE INITIALLY DEFERRED"):
    run(
        storage,
        session,
        f"CREATE TABLE child (id bigint primary key, pid bigint "
        f"REFERENCES parent(id) {deferrable})",
    )


def test_deferred_fk_child_before_parent_commits(storage, session):
    """The classic case: insert a child pointing at a not-yet-existing parent,
    then insert the parent, all inside one block — COMMIT succeeds."""
    _child(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 5)")  # 5 doesn't exist yet
    run(storage, session, "INSERT INTO parent (id, name) VALUES (5, 'e')")
    assert run(storage, session, "COMMIT").command_tag == "COMMIT"
    assert run(storage, session, "SELECT id FROM child WHERE id = 10").rows == [(10,)]


def test_deferred_fk_unresolved_aborts_at_commit(storage, session):
    """A deferred FK that is still dangling at COMMIT raises 23503 and rolls back."""
    _child(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)")
    assert sqlstate(storage, session, "COMMIT") == "23503"
    # The block rolled back — the child is gone.
    assert run(storage, session, "SELECT count(*) AS n FROM child").rows == [(0,)]


def test_deferred_fk_resolved_by_deleting_child(storage, session):
    """Deleting the offending child before COMMIT clears the deferred violation."""
    _child(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)")
    run(storage, session, "DELETE FROM child WHERE id = 10")
    assert run(storage, session, "COMMIT").command_tag == "COMMIT"


def test_non_deferrable_fk_still_immediate(storage, session):
    """A plain FK (not DEFERRABLE) raises on the write even inside a block."""
    _child(storage, session, deferrable="")
    run(storage, session, "BEGIN")
    assert sqlstate(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)") == "23503"


def test_deferrable_initially_immediate_checks_on_write(storage, session):
    """DEFERRABLE INITIALLY IMMEDIATE checks eagerly unless SET CONSTRAINTS defers."""
    _child(storage, session, deferrable="DEFERRABLE INITIALLY IMMEDIATE")
    run(storage, session, "BEGIN")
    assert sqlstate(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)") == "23503"


# -- deferred UNIQUE ----------------------------------------------------------- #


@pytest.fixture
def uq_storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run(
        s,
        session,
        "CREATE TABLE t (id bigint primary key, n bigint, "
        "CONSTRAINT t_n_key UNIQUE (n) DEFERRABLE INITIALLY DEFERRED)",
    )
    run(s, session, "INSERT INTO t (id, n) VALUES (1, 10), (2, 20)")
    try:
        yield s
    finally:
        s.close()


def test_deferred_unique_swap_commits(uq_storage, session):
    """Swapping two UNIQUE values transiently collides but nets out — COMMIT ok."""
    run(uq_storage, session, "BEGIN")
    run(uq_storage, session, "UPDATE t SET n = 20 WHERE id = 1")  # transient dup on 20
    run(uq_storage, session, "UPDATE t SET n = 10 WHERE id = 2")
    assert run(uq_storage, session, "COMMIT").command_tag == "COMMIT"
    assert run(uq_storage, session, "SELECT n FROM t WHERE id = 1").rows == [(20,)]


def test_deferred_unique_real_dup_aborts_at_commit(uq_storage, session):
    """A genuine duplicate surviving to COMMIT raises 23505 and rolls back."""
    run(uq_storage, session, "BEGIN")
    run(uq_storage, session, "INSERT INTO t (id, n) VALUES (3, 10)")  # dup of row 1
    assert sqlstate(uq_storage, session, "COMMIT") == "23505"
    assert run(uq_storage, session, "SELECT count(*) AS c FROM t").rows == [(2,)]


# -- SET CONSTRAINTS ----------------------------------------------------------- #


def test_set_constraints_all_deferred_defers_immediate_fk(storage, session):
    """SET CONSTRAINTS ALL DEFERRED postpones an otherwise-immediate FK check."""
    _child(storage, session, deferrable="DEFERRABLE INITIALLY IMMEDIATE")
    run(storage, session, "BEGIN")
    run(storage, session, "SET CONSTRAINTS ALL DEFERRED")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 5)")
    run(storage, session, "INSERT INTO parent (id, name) VALUES (5, 'e')")
    assert run(storage, session, "COMMIT").command_tag == "COMMIT"


def test_set_constraints_immediate_rechecks_now(storage, session):
    """SET CONSTRAINTS ALL IMMEDIATE re-checks pending deferred constraints at
    once — an unresolved violation raises immediately, not at COMMIT."""
    _child(storage, session)  # INITIALLY DEFERRED
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)")
    assert sqlstate(storage, session, "SET CONSTRAINTS ALL IMMEDIATE") == "23503"


def test_set_constraints_immediate_passes_when_resolved(storage, session):
    """SET CONSTRAINTS ALL IMMEDIATE is a no-op once the deferred books balance."""
    _child(storage, session)
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 5)")
    run(storage, session, "INSERT INTO parent (id, name) VALUES (5, 'e')")
    assert run(storage, session, "SET CONSTRAINTS ALL IMMEDIATE").command_tag == "SET CONSTRAINTS"
    assert run(storage, session, "COMMIT").command_tag == "COMMIT"


def test_set_constraints_named_immediate(storage, session):
    """SET CONSTRAINTS <name> IMMEDIATE re-checks only the named constraint (here
    the auto-generated ``child_pid_fkey``)."""
    _child(storage, session)  # FK auto-named child_pid_fkey, INITIALLY DEFERRED
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)")
    assert sqlstate(storage, session, "SET CONSTRAINTS child_pid_fkey IMMEDIATE") == "23503"


def test_set_constraints_named_immediate_leaves_others_pending(storage, session):
    """A named IMMEDIATE flushes only its constraint; an unrelated deferred FK
    stays pending and is caught at COMMIT."""
    _child(storage, session)  # child_pid_fkey
    run(
        storage,
        session,
        "CREATE TABLE child2 (id bigint primary key, pid bigint "
        "REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
    )
    run(storage, session, "BEGIN")
    run(storage, session, "INSERT INTO child (id, pid) VALUES (10, 5)")  # resolved later
    run(storage, session, "INSERT INTO child2 (id, pid) VALUES (20, 99)")  # never resolved
    # Flushing only child_pid_fkey doesn't yet see its parent → still dangling.
    assert sqlstate(storage, session, "SET CONSTRAINTS child_pid_fkey IMMEDIATE") == "23503"


def test_deferred_state_cleared_across_transactions(storage, session):
    """SET CONSTRAINTS deferral mode resets at end of transaction (Postgres)."""
    _child(storage, session, deferrable="DEFERRABLE INITIALLY IMMEDIATE")
    run(storage, session, "BEGIN")
    run(storage, session, "SET CONSTRAINTS ALL DEFERRED")
    run(storage, session, "ROLLBACK")
    # New block — the ALL DEFERRED override is gone, so the immediate FK bites.
    run(storage, session, "BEGIN")
    assert sqlstate(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)") == "23503"


def test_deferred_violation_outside_txn_still_raises(storage, session):
    """Autocommit (no open block): a deferred constraint has nowhere to defer to,
    so it behaves immediately."""
    _child(storage, session)
    assert sqlstate(storage, session, "INSERT INTO child (id, pid) VALUES (10, 99)") == "23503"
