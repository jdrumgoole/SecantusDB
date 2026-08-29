"""``CREATE DOMAIN`` — a named base type carrying its own NOT NULL / CHECK
constraints (and an optional DEFAULT).

A domain-typed column stores as the domain's base type, enforces the domain's
NOT NULL / CHECK on every write, inherits the domain's DEFAULT when the column
declares none, and reflects through ``pg_type`` (``typtype = 'd'``) with the
column's ``pg_attribute.atttypid`` pointing at the domain OID.
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
def t(storage, session):
    run(storage, session, "CREATE DOMAIN posint AS integer CHECK (VALUE > 0)")
    run(storage, session, "CREATE DOMAIN nonblank AS text NOT NULL CHECK (length(VALUE) > 0)")
    run(
        storage,
        session,
        "CREATE DOMAIN email AS varchar(255) CONSTRAINT email_chk CHECK (VALUE LIKE '%@%')",
    )
    run(
        storage,
        session,
        "CREATE TABLE t (id int PRIMARY KEY, qty posint, name nonblank, mail email)",
    )
    return storage


# -- base type + happy path --------------------------------------------------- #


def test_domain_column_stores_as_base_type(t, session):
    run(t, session, "INSERT INTO t VALUES (1, 5, 'joe', 'a@b.com')")
    result = run(t, session, "SELECT id, qty, name, mail FROM t")
    assert result.rows == [(1, 5, "joe", "a@b.com")]
    # qty reports the domain's base type, not the placeholder 'text'.
    assert result.columns[1].type_tag == "int4"


def test_nullable_domain_without_not_null_accepts_null(t, session):
    # posint / email have no NOT NULL, so an omitted / NULL value is fine.
    run(t, session, "INSERT INTO t (id, name) VALUES (1, 'joe')")
    assert run(t, session, "SELECT qty, mail FROM t").rows == [(None, None)]


# -- CHECK enforcement -------------------------------------------------------- #


def test_check_violation_on_insert(t, session):
    assert sqlstate(t, session, "INSERT INTO t VALUES (1, -3, 'joe', 'a@b.com')") == "23514"


def test_named_check_violation(t, session):
    assert sqlstate(t, session, "INSERT INTO t VALUES (1, 5, 'joe', 'nope')") == "23514"


def test_check_message_names_the_constraint(t, session):
    with pytest.raises(errors.SQLError) as ei:
        run(t, session, "INSERT INTO t VALUES (1, 5, 'joe', 'nope')")
    assert "email_chk" in str(ei.value)
    assert "domain email" in str(ei.value)


def test_check_violation_on_update(t, session):
    run(t, session, "INSERT INTO t VALUES (1, 5, 'joe', 'a@b.com')")
    assert sqlstate(t, session, "UPDATE t SET qty = -1 WHERE id = 1") == "23514"
    # a valid update goes through
    run(t, session, "UPDATE t SET qty = 10 WHERE id = 1")
    assert run(t, session, "SELECT qty FROM t WHERE id = 1").rows == [(10,)]


def test_check_passes_when_value_null(t, session):
    # A domain CHECK is not evaluated for a NULL value (three-valued logic).
    run(t, session, "INSERT INTO t (id, name) VALUES (1, 'joe')")
    assert run(t, session, "SELECT id FROM t").rows == [(1,)]


# -- NOT NULL enforcement ----------------------------------------------------- #


def test_not_null_violation_on_insert(t, session):
    assert sqlstate(t, session, "INSERT INTO t VALUES (1, 5, NULL, 'a@b.com')") == "23502"


def test_not_null_message(t, session):
    with pytest.raises(errors.SQLError) as ei:
        run(t, session, "INSERT INTO t VALUES (1, 5, NULL, 'a@b.com')")
    assert "domain nonblank does not allow null values" in str(ei.value)


def test_not_null_length_check_still_fires(t, session):
    # nonblank is NOT NULL *and* length(VALUE) > 0 — an empty string is a CHECK
    # violation, not a NOT NULL one.
    assert sqlstate(t, session, "INSERT INTO t VALUES (1, 5, '', 'a@b.com')") == "23514"


# -- DEFAULT inheritance ------------------------------------------------------ #


def test_domain_default_inherited(storage, session):
    run(storage, session, "CREATE DOMAIN score AS int DEFAULT 100 CHECK (VALUE >= 0)")
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, s score)")
    run(storage, session, "INSERT INTO u (id) VALUES (1)")
    assert run(storage, session, "SELECT s FROM u WHERE id = 1").rows == [(100,)]


def test_column_default_overrides_domain_default(storage, session):
    run(storage, session, "CREATE DOMAIN score AS int DEFAULT 100")
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, s score DEFAULT 7)")
    run(storage, session, "INSERT INTO u (id) VALUES (1)")
    assert run(storage, session, "SELECT s FROM u WHERE id = 1").rows == [(7,)]


# -- DDL errors --------------------------------------------------------------- #


def test_duplicate_domain(t, session):
    assert sqlstate(t, session, "CREATE DOMAIN posint AS int") == "42710"


def test_domain_name_clashes_with_enum(storage, session):
    run(storage, session, "CREATE TYPE mood AS ENUM ('happy', 'sad')")
    assert sqlstate(storage, session, "CREATE DOMAIN mood AS int") == "42710"


def test_undefined_type_column(storage, session):
    assert sqlstate(storage, session, "CREATE TABLE bad (x nosuchtype)") == "42704"


def test_unsupported_base_type(storage, session):
    assert sqlstate(storage, session, "CREATE DOMAIN d AS nosuchbase") == "42704"


# -- DROP DOMAIN -------------------------------------------------------------- #


def test_drop_domain(t, session):
    run(t, session, "DROP DOMAIN email")
    names = [r[0] for r in run(t, session, "SELECT typname FROM pg_type WHERE typtype='d'").rows]
    assert "email" not in names
    assert "posint" in names


def test_drop_missing_domain(storage, session):
    assert sqlstate(storage, session, "DROP DOMAIN nope") == "42704"


def test_drop_domain_if_exists(storage, session):
    # No error, no-op.
    assert run(storage, session, "DROP DOMAIN IF EXISTS nope").command_tag == "DROP DOMAIN"


# -- reflection --------------------------------------------------------------- #


def test_pg_type_reflects_domains(t, session):
    rows = run(
        t,
        session,
        "SELECT typname, typtype, typnotnull FROM pg_type WHERE typtype='d' ORDER BY typname",
    ).rows
    assert rows == [("email", "d", False), ("nonblank", "d", True), ("posint", "d", False)]


def test_pg_type_typbasetype_points_at_base(t, session):
    # posint's base is integer (oid 23); the row's typbasetype must be that oid.
    rows = run(
        t,
        session,
        "SELECT bt.typname FROM pg_type d JOIN pg_type bt ON d.typbasetype = bt.oid "
        "WHERE d.typname = 'posint'",
    ).rows
    assert rows == [("int4",)]


def test_pg_attribute_atttypid_is_domain_oid(t, session):
    rows = run(
        t,
        session,
        "SELECT a.attname, ty.typname FROM pg_attribute a "
        "JOIN pg_type ty ON a.atttypid = ty.oid WHERE ty.typtype = 'd' ORDER BY a.attname",
    ).rows
    assert rows == [("mail", "email"), ("name", "nonblank"), ("qty", "posint")]


def test_domain_checks_in_pg_constraint(t, session):
    rows = run(
        t,
        session,
        "SELECT conname, contype FROM pg_constraint WHERE contypid <> 0 ORDER BY conname",
    ).rows
    assert rows == [("email_chk", "c"), ("nonblank_check", "c"), ("posint_check", "c")]


def test_domain_base_typmod_surfaces_in_pg_type(storage, session):
    # pgjdbc's getColumns reads a domain column's COLUMN_SIZE from the domain's
    # pg_type.typbasetype + typtypmod (domainColumnSize): varbit(3) -> 3,
    # numeric(8,3) -> packed precision/scale, an int domain -> no typmod.
    run(storage, session, "CREATE DOMAIN nndom AS int not null")
    run(storage, session, "CREATE DOMAIN varbit2 AS varbit(3)")
    run(storage, session, "CREATE DOMAIN float83 AS numeric(8,3)")
    run(storage, session, "CREATE TABLE domaintable (id nndom, v varbit2, f float83)")
    rows = run(
        storage,
        session,
        "SELECT typname, typbasetype, typtypmod FROM pg_type"
        " WHERE typname IN ('nndom','varbit2','float83') ORDER BY typname",
    ).rows
    assert rows == [
        ("float83", 1700, ((8 << 16) | 3) + 4),
        ("nndom", 23, -1),
        ("varbit2", 1562, 3),
    ]
