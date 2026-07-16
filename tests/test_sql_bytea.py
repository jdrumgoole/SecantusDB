"""bytea binary type (#116): hex / escape literals, encode / decode format
conversions, get_byte / set_byte / length / bit_length, || concatenation, and
column round-trips.
"""

from __future__ import annotations

import pytest

from secantus.sql import bytea, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure bytea.py
# --------------------------------------------------------------------------- #


def test_parse_hex():
    assert bytea.parse("\\xdeadbeef") == b"\xde\xad\xbe\xef"
    assert bytea.parse("\\xDE AD BE EF") == b"\xde\xad\xbe\xef"  # whitespace ignored
    assert bytea.parse("\\x") == b""


def test_parse_escape():
    assert bytea.parse("abc") == b"abc"
    assert bytea.parse("ab\\001c") == b"ab\x01c"
    assert bytea.parse("a\\\\b") == b"a\\b"  # doubled backslash -> one byte


def test_parse_passthrough_bytes():
    assert bytea.parse(b"\x00\x01") == b"\x00\x01"
    assert bytea.parse(bytearray(b"xy")) == b"xy"


def test_parse_rejects_bad_hex():
    with pytest.raises(bytea.ByteaError):
        bytea.parse("\\xzz")


def test_encode_hex():
    assert bytea.encode(b"\x01\x02\xff", "hex") == "0102ff"


def test_encode_base64():
    assert bytea.encode(b"\x00\x01\x02", "base64") == "AAEC"


def test_encode_escape():
    assert bytea.encode(b"ab\x01", "escape") == "ab\\001"
    assert bytea.encode(b"a\\b", "escape") == "a\\\\b"


def test_encode_rejects_unknown_format():
    with pytest.raises(bytea.ByteaError):
        bytea.encode(b"x", "rot13")


def test_decode_roundtrips_encode():
    for fmt in ("hex", "base64", "escape"):
        assert bytea.decode(bytea.encode(b"\x00\xab\xff hi", fmt), fmt) == b"\x00\xab\xff hi"


def test_get_set_byte():
    assert bytea.get_byte(b"\xde\xad", 1) == 0xAD
    assert bytea.set_byte(b"\xde\xad", 0, 0) == b"\x00\xad"


def test_byte_index_out_of_range():
    with pytest.raises(bytea.ByteaError):
        bytea.get_byte(b"\xde", 5)
    with pytest.raises(bytea.ByteaError):
        bytea.set_byte(b"\xde", 5, 0)


def test_concat():
    assert bytea.concat(b"\xde\xad", b"\xbe\xef") == b"\xde\xad\xbe\xef"


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


def test_hex_cast_typed(storage, session):
    assert col(storage, session, "SELECT '\\xdeadbeef'::bytea").type_tag == "bytea"


def test_hex_cast_value(storage, session):
    assert val(storage, session, "SELECT '\\xdeadbeef'::bytea") == b"\xde\xad\xbe\xef"


def test_escape_cast_value(storage, session):
    assert val(storage, session, "SELECT 'abc'::bytea") == b"abc"


def test_encode_typed_text(storage, session):
    assert col(storage, session, "SELECT encode('\\x0102ff'::bytea, 'hex')").type_tag == "text"


def test_encode_value(storage, session):
    assert val(storage, session, "SELECT encode('\\x0102ff'::bytea, 'hex')") == "0102ff"
    assert val(storage, session, "SELECT encode('\\x000102'::bytea, 'base64')") == "AAEC"


def test_decode_typed_bytea(storage, session):
    assert col(storage, session, "SELECT decode('deadbeef', 'hex')").type_tag == "bytea"


def test_decode_value(storage, session):
    assert val(storage, session, "SELECT decode('deadbeef', 'hex')") == b"\xde\xad\xbe\xef"
    assert val(storage, session, "SELECT decode('AAEC', 'base64')") == b"\x00\x01\x02"


def test_get_byte(storage, session):
    assert col(storage, session, "SELECT get_byte('\\xdeadbeef'::bytea, 1)").type_tag == "int4"
    assert val(storage, session, "SELECT get_byte('\\xdeadbeef'::bytea, 1)") == 0xAD


def test_set_byte(storage, session):
    c = col(storage, session, "SELECT set_byte('\\xdeadbeef'::bytea, 0, 0)")
    assert c.type_tag == "bytea"
    assert (
        val(storage, session, "SELECT set_byte('\\xdeadbeef'::bytea, 0, 0)") == b"\x00\xad\xbe\xef"
    )


def test_length_is_byte_count(storage, session):
    assert val(storage, session, "SELECT length('\\xdeadbeef'::bytea)") == 4
    assert val(storage, session, "SELECT octet_length('\\xdeadbeef'::bytea)") == 4
    assert val(storage, session, "SELECT bit_length('\\xdeadbeef'::bytea)") == 32


def test_concat_op(storage, session):
    c = col(storage, session, "SELECT '\\xdead'::bytea || '\\xbeef'::bytea")
    assert c.type_tag == "bytea"
    assert (
        val(storage, session, "SELECT '\\xdead'::bytea || '\\xbeef'::bytea") == b"\xde\xad\xbe\xef"
    )


@pytest.fixture
def blobs(storage, session):
    run(storage, session, "CREATE TABLE blobs (id int PRIMARY KEY, data bytea)")
    run(storage, session, "INSERT INTO blobs VALUES (1, '\\xcafe')")
    run(storage, session, "INSERT INTO blobs VALUES (2, '\\xbeef')")
    return storage


def test_column_roundtrip(blobs, session):
    assert val(blobs, session, "SELECT data FROM blobs WHERE id = 1") == b"\xca\xfe"


def test_column_typed(blobs, session):
    assert col(blobs, session, "SELECT data FROM blobs WHERE id = 1").type_tag == "bytea"


def test_where_equality(blobs, session):
    rows = run(blobs, session, "SELECT id FROM blobs WHERE data = '\\xcafe'::bytea").rows
    assert [r[0] for r in rows] == [1]


def test_encode_column(blobs, session):
    rows = run(blobs, session, "SELECT encode(data, 'hex') FROM blobs ORDER BY id").rows
    assert [r[0] for r in rows] == ["cafe", "beef"]
