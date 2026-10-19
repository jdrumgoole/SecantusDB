"""Regression tests for Rust PG server bugs found 2026-10-19, while sizing
RP-049 (`PgHandler`'s transaction-end resets).

The expected values are literals measured on PostgreSQL 15.19: a sequence
created in a block that then commits is an ordinary committed sequence, and
creating one on the shared reference is what the reference-protection rule
forbids.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog_fixed("RP-050")
def test_nextval_is_not_rolled_back_after_a_committed_block(pg: Any) -> None:
    """PostgreSQL 15.19: `nextval` is never undone by a ROLLBACK, so after
    `begin; create sequence s; commit;` a later block's `nextval` (1) is
    rolled back and the next call answers 2. Before the fix: 1 again, and on
    every later block of the connection -- an explicit COMMIT never cleared
    the committed block's record of the sequences it wrote."""
    pg.execute("begin")
    pg.execute("create sequence rp050_s")
    pg.execute("commit")
    pg.execute("begin")
    assert pg_answer(pg, "select nextval('rp050_s')") == ("ok", [(1,)])
    pg.execute("rollback")
    assert pg_answer(pg, "select nextval('rp050_s')") == ("ok", [(2,)])


@pytest.mark.backlog_fixed("RP-052")
def test_set_constraints_ends_with_its_transaction(pg: Any) -> None:
    """PostgreSQL 15.19: `SET CONSTRAINTS` lasts only for the current
    transaction, so after `begin; set constraints all deferred; commit;` a
    later block's insert that breaks a DEFERRABLE INITIALLY IMMEDIATE
    foreign key fails at once (23503). Before the fix the insert was
    accepted: the mode outlived the COMMIT, and only a ROLLBACK reset it."""
    pg.execute("create table rp052_p (id int primary key)")
    pg.execute("create table rp052_c (pid int references rp052_p deferrable initially immediate)")
    pg.execute("begin")
    pg.execute("set constraints all deferred")
    pg.execute("commit")
    pg.execute("begin")
    answer = pg_answer(pg, "insert into rp052_c values (42)")
    pg.execute("rollback")
    assert answer[:2] == ("err", "23503")
