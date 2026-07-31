"""CREATE / DROP EXTENSION: SecantusDB ships citext / hstore built in (and
real Postgres preinstalls plpgsql), so installing those succeeds as a no-op;
anything else is honestly unavailable (0A000) rather than half-accepted.
SQLAlchemy's test provisioning runs ``CREATE EXTENSION IF NOT EXISTS citext``
at connect, which is what forced the statement to exist at all.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
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


@pytest.mark.parametrize("name", ["citext", "hstore", "plpgsql"])
def test_create_available_extension(storage, session, name):
    res = run(storage, session, f"CREATE EXTENSION {name}")
    assert res.command_tag == "CREATE EXTENSION"


def test_create_if_not_exists(storage, session):
    res = run(storage, session, "CREATE EXTENSION IF NOT EXISTS citext")
    assert res.command_tag == "CREATE EXTENSION"


def test_create_with_schema_tail_ignored(storage, session):
    res = run(storage, session, "CREATE EXTENSION hstore WITH SCHEMA public")
    assert res.command_tag == "CREATE EXTENSION"


def test_create_quoted_name(storage, session):
    res = run(storage, session, 'CREATE EXTENSION "citext"')
    assert res.command_tag == "CREATE EXTENSION"


def test_create_unavailable_extension(storage, session):
    with pytest.raises(SQLError) as exc:
        run(storage, session, "CREATE EXTENSION postgis")
    assert exc.value.sqlstate == "0A000"
    assert "postgis" in str(exc.value)


def test_create_unavailable_even_if_not_exists(storage, session):
    with pytest.raises(SQLError) as exc:
        run(storage, session, "CREATE EXTENSION IF NOT EXISTS postgis")
    assert exc.value.sqlstate == "0A000"


def test_drop_available_extension(storage, session):
    res = run(storage, session, "DROP EXTENSION hstore")
    assert res.command_tag == "DROP EXTENSION"


def test_drop_unknown_extension_errors(storage, session):
    with pytest.raises(SQLError) as exc:
        run(storage, session, "DROP EXTENSION postgis")
    assert exc.value.sqlstate == "42704"


def test_drop_if_exists_unknown_is_noop(storage, session):
    res = run(storage, session, "DROP EXTENSION IF EXISTS postgis")
    assert res.command_tag == "DROP EXTENSION"


def test_citext_still_works_after_create_extension(storage, session):
    run(storage, session, "CREATE EXTENSION IF NOT EXISTS citext")
    run(storage, session, "CREATE TABLE t (id bigint primary key, name citext)")
    run(storage, session, "INSERT INTO t VALUES (1, 'Alice')")
    res = run(storage, session, "SELECT id FROM t WHERE name = 'ALICE'")
    assert res.rows == [(1,)]
