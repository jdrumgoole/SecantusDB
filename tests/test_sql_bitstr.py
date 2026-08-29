"""Bit-string types (#109): bit(n) / varbit, B'…' literals, the &, |, #, ~,
<<, >> bitwise operators, || concat, and length / bit_length / octet_length /
get_bit / set_bit functions plus int <-> bit conversions.
"""

from __future__ import annotations

import pytest

from secantus.sql import bitstr, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure bitstr.py
# --------------------------------------------------------------------------- #


def test_normalize_pad_truncate():
    assert bitstr.normalize("101", length=4) == "1010"  # fixed pads right
    assert bitstr.normalize("10110", length=3) == "101"  # truncates
    assert bitstr.normalize("101", length=4, varying=True) == "101"  # varbit no pad
    assert bitstr.normalize("10110", length=3, varying=True) == "101"  # varbit truncates


def test_normalize_rejects_non_binary():
    with pytest.raises(bitstr.BitError):
        bitstr.normalize("1012")


def test_int_conversions():
    assert bitstr.from_int(10, 8) == "00001010"
    assert bitstr.from_int(-1, 4) == "1111"  # two's complement low bits
    assert bitstr.to_int("1010") == 10
    assert bitstr.to_int("") == 0


def test_bitwise_algebra():
    assert bitstr.band("1010", "0110") == "0010"
    assert bitstr.bor("1010", "0110") == "1110"
    assert bitstr.bxor("1010", "0110") == "1100"
    assert bitstr.bnot("1010") == "0101"


def test_bitwise_length_mismatch_raises():
    with pytest.raises(bitstr.BitError):
        bitstr.band("101", "0110")


def test_shifts_preserve_width():
    assert bitstr.shift_left("1010", 1) == "0100"
    assert bitstr.shift_right("1010", 1) == "0101"
    assert bitstr.shift_left("1010", 9) == "0000"
    assert bitstr.shift_right("1010", 9) == "0000"


def test_get_set_bit():
    assert bitstr.get_bit("0101", 0) == 0  # leftmost is index 0
    assert bitstr.get_bit("0101", 3) == 1
    assert bitstr.set_bit("0000", 1, 1) == "0100"
    with pytest.raises(bitstr.BitError):
        bitstr.get_bit("0101", 9)


def test_lengths():
    assert bitstr.bit_length("1010") == 4
    assert bitstr.octet_length("1010") == 1
    assert bitstr.octet_length("100000001") == 2


def test_is_bit_value():
    assert bitstr.is_bit_value("1010") is True
    assert bitstr.is_bit_value("") is False
    assert bitstr.is_bit_value("abc") is False
    assert bitstr.is_bit_value(10) is False


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


@pytest.fixture
def flags(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, flags bit(8), mask varbit)")
    run(storage, session, "INSERT INTO t VALUES (1, '10101010', '111')")
    run(storage, session, "INSERT INTO t VALUES (2, '00001111', '0')")
    return storage


def test_bitstring_literal_typed(storage, session):
    assert col(storage, session, "SELECT B'1010'").type_tag == "varbit"
    assert val(storage, session, "SELECT B'1010'") == "1010"


def test_bit_cast_pads(storage, session):
    assert col(storage, session, "SELECT '101'::bit(4)").type_tag == "bit"
    assert val(storage, session, "SELECT '101'::bit(4)") == "1010"


def test_varbit_cast(storage, session):
    assert col(storage, session, "SELECT '101'::varbit").type_tag == "varbit"
    assert val(storage, session, "SELECT '101'::varbit") == "101"


def test_int_to_bit(storage, session):
    assert val(storage, session, "SELECT 10::bit(8)") == "00001010"


def test_bit_to_int(storage, session):
    assert col(storage, session, "SELECT b'1010'::int").type_tag == "int4"
    assert val(storage, session, "SELECT b'1010'::int") == 10


def test_bitwise_and_or_xor_not(storage, session):
    assert val(storage, session, "SELECT b'1010' & b'0110'") == "0010"
    assert val(storage, session, "SELECT b'1010' | b'0110'") == "1110"
    assert val(storage, session, "SELECT b'1010' # b'0110'") == "1100"
    assert val(storage, session, "SELECT ~ b'1010'") == "0101"


def test_bitwise_typed_varbit(storage, session):
    assert col(storage, session, "SELECT b'1010' & b'0110'").type_tag == "varbit"


def test_shifts(storage, session):
    assert val(storage, session, "SELECT b'1010' << 1") == "0100"
    assert val(storage, session, "SELECT b'1010' >> 1") == "0101"


def test_concat(storage, session):
    assert val(storage, session, "SELECT b'1010' || b'11'") == "101011"


def test_length_functions(storage, session):
    assert val(storage, session, "SELECT length(b'1010')") == 4
    assert val(storage, session, "SELECT bit_length(b'1010')") == 4
    assert val(storage, session, "SELECT octet_length(b'100000001')") == 2


def test_get_set_bit_sql(storage, session):
    assert val(storage, session, "SELECT get_bit(b'0101', 3)") == 1
    assert col(storage, session, "SELECT get_bit(b'0101', 3)").type_tag == "int4"
    assert val(storage, session, "SELECT set_bit(b'0000', 1, 1)") == "0100"
    assert col(storage, session, "SELECT set_bit(b'0000', 1, 1)").type_tag == "varbit"


def test_integer_bitwise_still_works(storage, session):
    assert val(storage, session, "SELECT 5 & 3") == 1
    assert val(storage, session, "SELECT 5 | 2") == 7
    assert col(storage, session, "SELECT 5 & 3").type_tag == "int4"


def test_column_roundtrip(flags, session):
    assert val(flags, session, "SELECT flags FROM t WHERE id = 1") == "10101010"
    assert val(flags, session, "SELECT mask FROM t WHERE id = 1") == "111"


def test_where_bitwise_mask(flags, session):
    ids = [
        r[0]
        for r in run(
            flags,
            session,
            "SELECT id FROM t WHERE flags & b'00001111' = b'00001010' ORDER BY id",
        ).rows
    ]
    assert ids == [1]


def test_column_bitwise_in_select(flags, session):
    assert val(flags, session, "SELECT flags & b'00001111' FROM t WHERE id = 1") == "00001010"


# --------------------------------------------------------------------------- #
# Binary wire decode (pgextended._decode_varbit)
# --------------------------------------------------------------------------- #


def test_binary_varbit_decodes_to_bit_string():
    import struct

    from secantus.sql.pgextended import _decode_varbit

    # 4-byte bit length + ceil(bits/8) data bytes → the '0'/'1' string.
    assert _decode_varbit(struct.pack("!i", 4) + b"\xa0") == "1010"
    assert _decode_varbit(struct.pack("!i", 0)) == ""


def test_binary_varbit_empty_is_08P01():
    from secantus.sql import errors
    from secantus.sql.pgextended import _decode_varbit

    # An empty binary param can't hold the 4-byte length header.
    with pytest.raises(errors.SQLError) as e:
        _decode_varbit(b"")
    assert e.value.sqlstate == "08P01"


def test_binary_varbit_trailing_bytes_is_22P03():
    import struct

    from secantus.sql import errors
    from secantus.sql.pgextended import _decode_varbit

    # bitlen=82 needs 11 bytes; 17 trailing bytes leave the buffer unconsumed.
    with pytest.raises(errors.SQLError) as e:
        _decode_varbit(struct.pack("!i", 82) + b"\x00" * 17)
    assert e.value.sqlstate == "22P03"
