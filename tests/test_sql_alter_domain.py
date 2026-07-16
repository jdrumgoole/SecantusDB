"""``ALTER DOMAIN`` — evolve a domain's constraints / default / nullability.

Supported: ``ADD [CONSTRAINT c] CHECK (…) [NOT VALID]`` (re-validates existing
rows unless NOT VALID), ``DROP CONSTRAINT [IF EXISTS] c``, ``SET DEFAULT expr`` /
``DROP DEFAULT``, ``SET NOT NULL`` (re-validates) / ``DROP NOT NULL``, and
``RENAME TO new`` (which repoints every column that references the domain).
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def d(storage, session):
    run(storage, session, "CREATE DOMAIN posint AS integer CHECK (VALUE > 0)")
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, q posint)")
    run(storage, session, "INSERT INTO t VALUES (1, 5), (2, 50)")
    return storage


# -- ADD CONSTRAINT ----------------------------------------------------------- #


def test_add_constraint_enforced_on_new_rows(d, session):
    run(d, session, "ALTER DOMAIN posint ADD CONSTRAINT lt100 CHECK (VALUE < 100)")
    assert sqlstate(d, session, "INSERT INTO t VALUES (3, 200)") == "23514"
    run(d, session, "INSERT INTO t VALUES (3, 60)")  # within range → OK


def test_add_constraint_revalidates_existing_rows(d, session):
    # Existing rows hold 5 and 50; a CHECK (VALUE < 10) is violated by 50.
    assert sqlstate(d, session, "ALTER DOMAIN posint ADD CONSTRAINT lt10 CHECK (VALUE < 10)") == (
        "23514"
    )
    # Rejected → not applied, so 55 still inserts fine.
    run(d, session, "INSERT INTO t VALUES (3, 55)")


def test_add_constraint_not_valid_skips_revalidation(d, session):
    # NOT VALID does not re-check existing data, but applies to new writes.
    run(d, session, "ALTER DOMAIN posint ADD CONSTRAINT lt10 CHECK (VALUE < 10) NOT VALID")
    assert sqlstate(d, session, "INSERT INTO t VALUES (3, 20)") == "23514"
    run(d, session, "INSERT INTO t VALUES (3, 5)")  # OK


def test_add_unnamed_constraint(d, session):
    run(d, session, "ALTER DOMAIN posint ADD CHECK (VALUE <> 42)")
    assert sqlstate(d, session, "INSERT INTO t VALUES (3, 42)") == "23514"


def test_add_duplicate_constraint_name(d, session):
    run(d, session, "ALTER DOMAIN posint ADD CONSTRAINT c1 CHECK (VALUE < 100)")
    assert sqlstate(d, session, "ALTER DOMAIN posint ADD CONSTRAINT c1 CHECK (VALUE < 90)") == (
        "42710"
    )


# -- DROP CONSTRAINT ---------------------------------------------------------- #


def test_drop_constraint(d, session):
    run(d, session, "ALTER DOMAIN posint ADD CONSTRAINT lt100 CHECK (VALUE < 100)")
    run(d, session, "ALTER DOMAIN posint DROP CONSTRAINT lt100")
    run(d, session, "INSERT INTO t VALUES (3, 200)")  # constraint gone → OK
    # The original CHECK (VALUE > 0) still stands.
    assert sqlstate(d, session, "INSERT INTO t VALUES (4, -1)") == "23514"


def test_drop_missing_constraint(d, session):
    assert sqlstate(d, session, "ALTER DOMAIN posint DROP CONSTRAINT nope") == "42704"


def test_drop_constraint_if_exists(d, session):
    assert (
        run(d, session, "ALTER DOMAIN posint DROP CONSTRAINT IF EXISTS nope").command_tag
        == "ALTER DOMAIN"
    )


# -- DEFAULT ------------------------------------------------------------------ #


def test_set_and_drop_default(d, session):
    run(d, session, "ALTER DOMAIN posint SET DEFAULT 7")
    run(d, session, "CREATE TABLE a (id int PRIMARY KEY, q posint)")
    run(d, session, "INSERT INTO a (id) VALUES (1)")
    assert run(d, session, "SELECT q FROM a WHERE id = 1").rows == [(7,)]
    run(d, session, "ALTER DOMAIN posint DROP DEFAULT")
    run(d, session, "CREATE TABLE b (id int PRIMARY KEY, q posint)")
    run(d, session, "INSERT INTO b (id) VALUES (1)")
    assert run(d, session, "SELECT q FROM b WHERE id = 1").rows == [(None,)]


# -- NOT NULL ----------------------------------------------------------------- #


def test_set_not_null_enforced(d, session):
    run(d, session, "ALTER DOMAIN posint SET NOT NULL")
    assert sqlstate(d, session, "INSERT INTO t (id) VALUES (3)") == "23502"


def test_set_not_null_blocked_by_existing_null(storage, session):
    run(storage, session, "CREATE DOMAIN nn AS int")
    run(storage, session, "CREATE TABLE z (id int PRIMARY KEY, v nn)")
    run(storage, session, "INSERT INTO z (id) VALUES (1)")  # v is NULL
    assert sqlstate(storage, session, "ALTER DOMAIN nn SET NOT NULL") == "23502"


def test_drop_not_null(storage, session):
    run(storage, session, "CREATE DOMAIN nn AS int NOT NULL")
    run(storage, session, "CREATE TABLE z (id int PRIMARY KEY, v nn)")
    assert sqlstate(storage, session, "INSERT INTO z (id) VALUES (1)") == "23502"
    run(storage, session, "ALTER DOMAIN nn DROP NOT NULL")
    run(storage, session, "INSERT INTO z (id) VALUES (1)")  # NULL now allowed


# -- RENAME ------------------------------------------------------------------- #


def test_rename_repoints_columns_and_enforces(d, session):
    run(d, session, "ALTER DOMAIN posint RENAME TO posnum")
    # pg_type reflects the new name only.
    names = [r[0] for r in run(d, session, "SELECT typname FROM pg_type WHERE typtype='d'").rows]
    assert names == ["posnum"]
    # The column now reflects the renamed domain oid.
    rows = run(
        d,
        session,
        "SELECT a.attname, ty.typname FROM pg_attribute a "
        "JOIN pg_type ty ON a.atttypid = ty.oid WHERE ty.typtype = 'd'",
    ).rows
    assert rows == [("q", "posnum")]
    # Enforcement still active under the new name.
    assert sqlstate(d, session, "INSERT INTO t VALUES (3, -1)") == "23514"


def test_rename_clash(d, session):
    run(d, session, "CREATE DOMAIN other AS int")
    assert sqlstate(d, session, "ALTER DOMAIN posint RENAME TO other") == "42710"


# -- errors ------------------------------------------------------------------- #


def test_alter_missing_domain(storage, session):
    assert sqlstate(storage, session, "ALTER DOMAIN ghost SET NOT NULL") == "42704"
