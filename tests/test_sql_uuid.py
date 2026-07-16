"""UUID type + generators (#112): the uuid type, literals / casts,
gen_random_uuid / uuid_generate_v4, and equality / ordering.
"""

from __future__ import annotations

import uuid as _uuid

import pytest

from secantus.sql import run_sql, uuidtype
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"

SAMPLE = "550e8400-e29b-41d4-a716-446655440000"


# --------------------------------------------------------------------------- #
# Pure uuidtype.py
# --------------------------------------------------------------------------- #


def test_normalize_forms():
    assert uuidtype.normalize(SAMPLE) == SAMPLE
    assert uuidtype.normalize(SAMPLE.upper()) == SAMPLE  # lower-cased
    assert uuidtype.normalize("550e8400e29b41d4a716446655440000") == SAMPLE  # bare hex
    assert uuidtype.normalize("{" + SAMPLE + "}") == SAMPLE  # braced


def test_normalize_rejects_garbage():
    with pytest.raises(uuidtype.UUIDError):
        uuidtype.normalize("not-a-uuid")


def test_generate_is_valid_and_unique():
    a, b = uuidtype.generate(), uuidtype.generate()
    assert a != b
    assert _uuid.UUID(a).version == 4
    assert uuidtype.normalize(a) == a


def test_is_uuid_value():
    assert uuidtype.is_uuid_value(SAMPLE) is True
    assert uuidtype.is_uuid_value("nope") is False
    assert uuidtype.is_uuid_value(123) is False


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


def test_uuid_cast_typed(storage, session):
    assert col(storage, session, f"SELECT '{SAMPLE}'::uuid").type_tag == "uuid"


def test_uuid_cast_normalises(storage, session):
    assert val(storage, session, f"SELECT '{SAMPLE.upper()}'::uuid") == SAMPLE
    assert val(storage, session, "SELECT '550e8400e29b41d4a716446655440000'::uuid") == SAMPLE


def test_gen_random_uuid_typed(storage, session):
    assert col(storage, session, "SELECT gen_random_uuid()").type_tag == "uuid"


def test_gen_random_uuid_value(storage, session):
    v = val(storage, session, "SELECT gen_random_uuid()")
    assert _uuid.UUID(v).version == 4


def test_uuid_generate_v4_typed(storage, session):
    assert col(storage, session, "SELECT uuid_generate_v4()").type_tag == "uuid"


@pytest.fixture
def people(storage, session):
    run(storage, session, "CREATE TABLE people (id uuid PRIMARY KEY, name text)")
    run(storage, session, f"INSERT INTO people VALUES ('{SAMPLE}', 'alice')")
    run(storage, session, "INSERT INTO people VALUES (gen_random_uuid(), 'bob')")
    return storage


def test_uuid_column_roundtrip(people, session):
    assert val(people, session, "SELECT id FROM people WHERE name = 'alice'") == SAMPLE


def test_uuid_column_typed(people, session):
    assert col(people, session, "SELECT id FROM people WHERE name = 'alice'").type_tag == "uuid"


def test_uuid_where_equality(people, session):
    assert val(people, session, f"SELECT name FROM people WHERE id = '{SAMPLE}'") == "alice"


def test_uuid_where_equality_uppercase(people, session):
    # An uppercase literal is implicitly normalised to match the stored value.
    assert val(people, session, f"SELECT name FROM people WHERE id = '{SAMPLE.upper()}'") == "alice"


def test_gen_random_uuid_insert_is_unique(people, session):
    ids = [r[0] for r in run(people, session, "SELECT id FROM people").rows]
    assert len(ids) == len(set(ids)) == 2
    assert all(_uuid.UUID(i) for i in ids)


def test_uuid_order_by(storage, session):
    run(storage, session, "CREATE TABLE u (id uuid PRIMARY KEY)")
    run(storage, session, "INSERT INTO u VALUES ('00000000-0000-0000-0000-000000000002')")
    run(storage, session, "INSERT INTO u VALUES ('00000000-0000-0000-0000-000000000001')")
    ids = [r[0] for r in run(storage, session, "SELECT id FROM u ORDER BY id").rows]
    assert ids == [
        "00000000-0000-0000-0000-000000000001",
        "00000000-0000-0000-0000-000000000002",
    ]
