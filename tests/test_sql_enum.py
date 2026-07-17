"""``CREATE TYPE … AS ENUM`` — enum types, enum-typed columns with value
validation, and ``pg_type`` / ``pg_enum`` reflection.

An enum column stores text but rejects a value outside the enum's declared labels
(``22P02``). Enum types reflect through ``pg_type`` (``typtype = 'e'``) and
``pg_enum`` (one row per label) so SQLAlchemy / psql's ``\\dT`` see them.
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
def mood(storage, session):
    run(storage, session, "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
    return storage


# -- CREATE / DROP TYPE -------------------------------------------------------- #


def test_create_type_enum(storage, session):
    assert run(storage, session, "CREATE TYPE mood AS ENUM ('a', 'b')").command_tag == "CREATE TYPE"


def test_duplicate_type_rejected(mood, session):
    assert sqlstate(mood, session, "CREATE TYPE mood AS ENUM ('x')") == "42710"


def test_drop_type(mood, session):
    assert run(mood, session, "DROP TYPE mood").command_tag == "DROP TYPE"
    assert sqlstate(mood, session, "DROP TYPE mood") == "42704"


def test_drop_type_if_exists(storage, session):
    assert run(storage, session, "DROP TYPE IF EXISTS nope").command_tag == "DROP TYPE"


def test_range_create_type(storage, session):
    # Range types are supported (see also test_pgserver_psycopg): the type and
    # its auto-created companion multirange reflect through pg_type/pg_range,
    # and literal casts parse with the declared subtype.
    assert (
        run(storage, session, "CREATE TYPE fr AS RANGE (subtype = float8)").command_tag
        == "CREATE TYPE"
    )
    rows = run(
        storage,
        session,
        "SELECT typname, typtype FROM pg_type WHERE typname IN ('fr', 'fr_multirange')"
        " ORDER BY typname",
    ).rows
    assert rows == [("fr", "r"), ("fr_multirange", "m")]
    r = run(storage, session, "SELECT '[1.5,2.5)'::fr").rows[0][0]
    assert r == {"lower": 1.5, "upper": 2.5, "lower_inc": True, "upper_inc": False}
    # Base types remain a faithful not-supported (0A000).
    assert sqlstate(storage, session, "CREATE TYPE base_t (input = f, output = g)") == "0A000"


# -- enum-typed columns -------------------------------------------------------- #


def test_enum_column_accepts_valid_label(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, 'happy')")
    assert run(mood, session, "SELECT id, m FROM t").rows == [(1, "happy")]


def test_enum_column_rejects_invalid_label(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    assert sqlstate(mood, session, "INSERT INTO t (id, m) VALUES (1, 'furious')") == "22P02"


def test_enum_column_allows_null(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, NULL)")
    assert run(mood, session, "SELECT m FROM t").rows == [(None,)]


def test_enum_column_update_validates(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t (id, m) VALUES (1, 'ok')")
    run(mood, session, "UPDATE t SET m = 'sad' WHERE id = 1")
    assert run(mood, session, "SELECT m FROM t").rows == [("sad",)]
    assert sqlstate(mood, session, "UPDATE t SET m = 'nope' WHERE id = 1") == "22P02"


def test_column_of_unknown_type_errors(storage, session):
    assert sqlstate(storage, session, "CREATE TABLE t (id int, m no_such_type)") == "42704"


# -- reflection ---------------------------------------------------------------- #


def test_pg_type_lists_enum(mood, session):
    rows = run(
        mood, session, "SELECT typname, typtype FROM pg_catalog.pg_type WHERE typtype = 'e'"
    ).rows
    assert rows == [("mood", "e")]


def test_pg_enum_lists_labels_in_order(mood, session):
    rows = run(
        mood,
        session,
        "SELECT enumlabel, enumsortorder FROM pg_catalog.pg_enum ORDER BY enumsortorder",
    ).rows
    assert rows == [("sad", 1.0), ("ok", 2.0), ("happy", 3.0)]


def test_enum_column_atttypid_matches_type_oid(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    att = run(mood, session, "SELECT atttypid FROM pg_attribute WHERE attname = 'm'").rows
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows
    assert att == typ and att != [(25,)]  # points at the enum oid, not text


# -- result-column oids (RowDescription) ---------------------------------------- #
# A SELECT / RETURNING result column of an enum type reports the enum's minted
# pg_type oid — not text's 25 — so a client that registered the type from the
# catalog recognises result columns. Non-enum columns are untouched.


def test_select_result_column_reports_enum_oid(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood, note text)")
    run(mood, session, "INSERT INTO t VALUES (1, 'happy', 'x')")
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]
    res = run(mood, session, "SELECT m, note FROM t")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("m", typ), ("note", 25)]
    assert typ != 25
    assert res.rows == [("happy", "x")]  # the value stays the label text


def test_select_star_reports_enum_oid(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]
    res = run(mood, session, "SELECT * FROM t")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("id", 23), ("m", typ)]


def test_returning_result_column_reports_enum_oid(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]
    res = run(mood, session, "INSERT INTO t VALUES (1, 'ok') RETURNING m, id")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("m", typ), ("id", 23)]
    res = run(mood, session, "UPDATE t SET m = 'sad' WHERE id = 1 RETURNING m")
    assert res.columns[0].pg_oid == typ
    res = run(mood, session, "DELETE FROM t WHERE id = 1 RETURNING m")
    assert res.columns[0].pg_oid == typ


def test_enum_pg_type_reports_array_oid(mood, session):
    # psycopg's EnumInfo.fetch asserts typarray > 0 and register_enum keys the
    # enum's array loader on it — a typarray of 0 registered that loader on
    # oid 0 (INVALID_OID), clobbering the client's unknown-oid text fallback.
    rows = run(mood, session, "SELECT oid, typarray FROM pg_type WHERE typname = 'mood'").rows
    (oid, typarray) = rows[0]
    assert typarray > 0 and typarray != oid


def test_enum_oid_stable_across_create_drop_alter(mood, session):
    # Real Postgres assigns a type's oid at CREATE and never renumbers or
    # reuses it. A positional mint (base + sorted-name index) would shift
    # 'mood' when a lexically-earlier type appears — and a psycopg client that
    # register_enum'd the old oid would decode the wrong type through it.
    def oid_of(name):
        return run(mood, session, f"SELECT oid FROM pg_type WHERE typname = '{name}'").rows[0][0]

    mood_oid = oid_of("mood")
    run(mood, session, "CREATE TYPE aaa AS ENUM ('x')")  # sorts before 'mood'
    aaa_oid = oid_of("aaa")
    assert oid_of("mood") == mood_oid
    assert aaa_oid != mood_oid
    run(mood, session, "DROP TYPE aaa")
    run(mood, session, "CREATE TYPE bbb AS ENUM ('y')")
    assert oid_of("bbb") not in (aaa_oid, mood_oid)  # dropped oids are not reused
    run(mood, session, "ALTER TYPE mood ADD VALUE 'meh'")
    assert oid_of("mood") == mood_oid  # ALTER TYPE keeps the oid
    res = run(mood, session, f"SELECT enumlabel FROM pg_enum WHERE enumtypid = {mood_oid}")
    assert ("meh",) in res.rows


def test_regtype_folds_unquoted_mixed_case(mood, session):
    # Postgres folds an unquoted identifier to lowercase: 'MoOd'::regtype is
    # the mood enum. psycopg's EnumInfo.fetch(conn, "StrTestEnum") depends on
    # this fold to find the lowercase-stored type.
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]
    rows = run(mood, session, "SELECT oid FROM pg_type WHERE oid = 'MoOd'::regtype").rows
    assert rows == [(typ,)]


def test_regtype_quoted_preserves_case(storage, session):
    run(storage, session, "CREATE TYPE \"CamelEnum\" AS ENUM ('a', 'b')")
    rows = run(
        storage, session, "SELECT typname FROM pg_type WHERE oid = '\"CamelEnum\"'::regtype"
    ).rows
    assert rows == [("CamelEnum",)]


def test_enum_cast_reports_enum_oid_and_validates(mood, session):
    # ``'ok'::mood`` describes with the enum's minted oid (the value stays the
    # label text) and an unknown label raises 22P02 like real Postgres.
    typ = run(mood, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]
    res = run(mood, session, "SELECT 'ok'::mood AS m, 'x' AS t")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("m", typ), ("t", 25)]
    assert res.rows == [("ok", "x")]
    assert sqlstate(mood, session, "SELECT 'nope'::mood") == "22P02"
    assert run(mood, session, "SELECT NULL::mood").rows == [(None,)]


def test_enum_array_cast_reports_array_oid_and_validates(mood, session):
    typ, typarray = run(
        mood, session, "SELECT oid, typarray FROM pg_type WHERE typname = 'mood'"
    ).rows[0]
    res = run(mood, session, "SELECT '{ok,sad}'::mood[] AS ms")
    assert res.columns[0].pg_oid == typarray != typ
    assert res.columns[0].type_tag == "text[]"
    assert res.rows == [(["ok", "sad"],)]
    assert sqlstate(mood, session, "SELECT '{ok,nope}'::mood[]") == "22P02"


def test_regtype_of_mixed_case_enum_renders_quoted(storage, session):
    # ``oid::regtype::text`` must quote a name that needs it — psycopg's
    # ClientCursor pastes the fetched regtype verbatim as a cast suffix, and an
    # unquoted CamelCase name would fold back to lowercase and miss the type.
    run(storage, session, "CREATE TYPE \"CamelEnum\" AS ENUM ('a')")
    run(storage, session, "CREATE TYPE plain AS ENUM ('b')")
    rows = run(
        storage,
        session,
        "SELECT oid::regtype::text FROM pg_type WHERE typtype = 'e' ORDER BY typname",
    ).rows
    assert rows == [('"CamelEnum"',), ("plain",)]


def test_second_enum_oid_distinct_and_stable(mood, session):
    run(mood, session, "CREATE TYPE colour AS ENUM ('red', 'blue')")
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood, c colour)")
    oids = dict(run(mood, session, "SELECT typname, oid FROM pg_type WHERE typtype = 'e'").rows)
    res = run(mood, session, "SELECT m, c FROM t")
    assert [c.pg_oid for c in res.columns] == [oids["mood"], oids["colour"]]
    assert oids["mood"] != oids["colour"]


# -- enum oid through pipeline / evaluated plan shapes ------------------------- #
# GROUP BY, JOIN, DISTINCT, and per-row-evaluated selects plan through the
# pipeline/evaluated planners whose out_columns carry string tags; the enum
# identity travels in out_enum_types so RowDescription still reports the mint.


@pytest.fixture
def peopled(mood, session):
    run(mood, session, "CREATE TABLE people (name text PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO people VALUES ('a', 'sad'), ('b', 'ok'), ('c', 'ok')")
    return mood


def mood_oid(storage, session):
    return run(storage, session, "SELECT oid FROM pg_type WHERE typname = 'mood'").rows[0][0]


def test_group_by_enum_key_reports_enum_oid(peopled, session):
    typ = mood_oid(peopled, session)
    res = run(peopled, session, "SELECT m, count(*) FROM people GROUP BY m ORDER BY m")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("m", typ), ("count", 20)]
    assert res.rows == [("sad", 1), ("ok", 2)]


def test_join_enum_column_reports_enum_oid(peopled, session):
    run(peopled, session, "CREATE TABLE teams (name text PRIMARY KEY, lead text)")
    run(peopled, session, "INSERT INTO teams VALUES ('x', 'a'), ('y', 'b')")
    typ = mood_oid(peopled, session)
    res = run(
        peopled,
        session,
        "SELECT t.name, p.m FROM teams t JOIN people p ON p.name = t.lead ORDER BY t.name",
    )
    assert [c.pg_oid for c in res.columns] == [25, typ]
    assert res.rows == [("x", "sad"), ("y", "ok")]


def test_join_group_by_enum_key_reports_enum_oid(peopled, session):
    run(peopled, session, "CREATE TABLE teams (name text PRIMARY KEY, lead text)")
    run(peopled, session, "INSERT INTO teams VALUES ('x', 'a'), ('y', 'b')")
    typ = mood_oid(peopled, session)
    res = run(
        peopled,
        session,
        "SELECT p.m, count(*) FROM teams t JOIN people p ON p.name = t.lead"
        " GROUP BY p.m ORDER BY p.m",
    )
    assert res.columns[0].pg_oid == typ


def test_distinct_enum_reports_enum_oid(peopled, session):
    typ = mood_oid(peopled, session)
    res = run(peopled, session, "SELECT DISTINCT m FROM people ORDER BY m")
    assert [(c.name, c.pg_oid) for c in res.columns] == [("m", typ)]
    assert res.rows == [("sad",), ("ok",)]


def test_evaluated_select_enum_column_reports_enum_oid(peopled, session):
    # A scalar function alongside the enum column forces the per-row evaluator.
    typ = mood_oid(peopled, session)
    res = run(peopled, session, "SELECT m, upper(name) FROM people ORDER BY name")
    assert res.columns[0].pg_oid == typ


def test_enum_array_constructor_reports_array_oid(mood, session):
    typ, typarray = run(
        mood, session, "SELECT oid, typarray FROM pg_type WHERE typname = 'mood'"
    ).rows[0]
    res = run(mood, session, "SELECT array['sad'::mood, 'ok'::mood] AS ms")
    assert res.columns[0].pg_oid == typarray != typ
    assert res.rows == [(["sad", "ok"],)]


# -- enum-array table columns (``mood[]``) ------------------------------------- #


def test_create_table_enum_array_column(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, ms mood[])")
    run(mood, session, "INSERT INTO t VALUES (1, '{sad,happy}')")
    typarray = run(mood, session, "SELECT typarray FROM pg_type WHERE typname = 'mood'").rows[0][0]
    res = run(mood, session, "SELECT ms FROM t")
    assert res.columns[0].pg_oid == typarray
    assert res.rows == [(["sad", "happy"],)]
    star = run(mood, session, "SELECT * FROM t")
    assert [c.pg_oid for c in star.columns] == [23, typarray]


def test_enum_array_column_validates_labels(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, ms mood[])")
    assert sqlstate(mood, session, "INSERT INTO t VALUES (1, '{sad,furious}')") == "22P02"
    run(mood, session, "INSERT INTO t VALUES (2, NULL)")
    run(mood, session, "INSERT INTO t VALUES (3, '{}')")
    assert run(mood, session, "SELECT ms FROM t ORDER BY id").rows == [(None,), ([],)]


def test_array_of_undeclared_type_rejected(mood, session):
    assert sqlstate(mood, session, "CREATE TABLE t (id int PRIMARY KEY, xs nosuch[])") == "42704"
