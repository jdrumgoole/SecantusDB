"""Float text rendering: the digit count and the notation are separate choices.

Postgres emits the shortest decimal that round-trips, then picks fixed or
scientific from the **exponent alone** — scientific iff ``exp < -4`` or
``exp >= 6`` for float4, ``>= 15`` for float8. Both renderers used to derive the
notation from the digit count instead, and got it wrong in opposite directions:
float4 via ``%g`` (scientific once ``exp >= precision``, so 80 printed
``8e+01``), float8 via Python's ``repr`` (whose threshold is 16, so 1e15 printed
``1000000000000000``).

**These assertions are on the rendered TEXT.** A driver-decoded comparison
cannot see this class at all — psycopg turns both `80` and `8e+01` into the same
Python float — which is why the bug survived a gauge and a probe that compared
values.
"""

from __future__ import annotations

import random
import struct

import pytest

import pg_oracle
from secantus.sql import typemap
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

DB = "floatdb"
FLOAT4_OID, FLOAT8_OID = 700, 701


@pytest.fixture
def wire(tmp_path):
    """A connection that hands back the RAW text for float columns, so the
    assertions see what the server actually put on the wire."""
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    conn = psycopg.connect(host=host, port=port, dbname=DB, user="joe", autocommit=True)

    class _Raw(psycopg.adapt.Loader):
        def load(self, data):
            return bytes(data).decode()

    conn.adapters.register_loader(FLOAT4_OID, _Raw)
    conn.adapters.register_loader(FLOAT8_OID, _Raw)
    try:
        yield conn
    finally:
        conn.close()
        srv.stop()
        st.close()


def text_of(conn, sql):
    with conn.cursor() as cur:
        cur.execute(sql)
        return cur.fetchall()[0][0]


class TestFloat4Rendering:
    @pytest.mark.parametrize(
        "value,expected",
        [
            # The regression: one significant digit, exponent 1. `%.1g` gives
            # `8e+01`; Postgres gives `80`.
            (80, "80"),
            (10, "10"),
            (20, "20"),
            (100, "100"),
            (500, "500"),
            (1000, "1000"),
            (100000, "100000"),
            # exp >= 6 is where Postgres switches, regardless of digit count.
            (1000000, "1e+06"),
            (10000000, "1e+07"),
            # 8 digits and exp 7 — scientific, which a precision-keyed rule
            # would have rendered fixed as `16777216`.
            (16777216, "1.6777216e+07"),
            # Untouched by the bug, and must stay untouched.
            (3, "3"),
            (64, "64"),
            (3.5, "3.5"),
            (0.5, "0.5"),
            (0.25, "0.25"),
            (123.456, "123.456"),
            # exp -4 is the last fixed one; -5 goes scientific.
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (0, "0"),
        ],
    )
    def test_rendering(self, wire, value, expected):
        assert text_of(wire, f"select {value!r}::real") == expected

    def test_negative_keeps_the_sign_and_the_notation(self, wire):
        assert text_of(wire, "select (-80)::real") == "-80"
        assert text_of(wire, "select (-1000000)::real") == "-1e+06"

    def test_the_slt_shape_that_found_it(self, wire):
        """`SELECT - CAST ( - col0 AS REAL )` from the sqllogictest corpus."""
        assert text_of(wire, "select - CAST ( - 80 AS REAL )") == "80"


class TestFloat8Rendering:
    @pytest.mark.parametrize(
        "value,expected",
        [
            # Postgres switches at exp 15; Python's repr switches at 16, which
            # is what these three used to expose.
            (1e15, "1e+15"),
            (1234567890123456.0, "1.234567890123456e+15"),
            (float(2**53), "9.007199254740992e+15"),
            # Below the threshold, fixed.
            (1e14, "100000000000000"),
            (123456789012345.0, "123456789012345"),
            (80, "80"),
            (0.1, "0.1"),
            (3.5, "3.5"),
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (1e16, "1e+16"),
        ],
    )
    def test_rendering(self, wire, value, expected):
        assert text_of(wire, f"select {value!r}::float8") == expected


class TestNonFinite:
    @pytest.mark.parametrize("tag", ["real", "float8"])
    @pytest.mark.parametrize(
        "literal,expected",
        [("'NaN'", "NaN"), ("'Infinity'", "Infinity"), ("'-Infinity'", "-Infinity")],
    )
    def test_special_values_are_unchanged(self, wire, tag, literal, expected):
        assert text_of(wire, f"select {literal}::{tag}") == expected


class TestRoundTrip:
    """Whatever notation is chosen, the text must still read back as the same
    value — that is the property the digit search exists for."""

    @pytest.mark.parametrize("seed", [1, 2, 3])
    def test_float4_round_trips(self, seed):
        rng = random.Random(seed)
        for _ in range(300):
            bits = rng.getrandbits(32)
            (value,) = struct.unpack("!f", struct.pack("!I", bits))
            if value != value or value in (float("inf"), float("-inf")):
                continue
            rendered = typemap._render_pg_float4(value)
            assert struct.pack("!f", float(rendered)) == struct.pack("!f", value)

    @pytest.mark.parametrize("seed", [1, 2, 3])
    def test_float8_round_trips(self, seed):
        rng = random.Random(seed)
        for _ in range(300):
            bits = rng.getrandbits(64)
            (value,) = struct.unpack("!d", struct.pack("!Q", bits))
            if value != value or value in (float("inf"), float("-inf")):
                continue
            assert float(typemap._render_pg_float(value)) == value


def _pg_reference():
    """A live PostgreSQL to check against, or None. Point elsewhere with
    SECANTUS_PG_ORACLE_DSN.

    Delegates to `pg_oracle` so all six oracle suites share one probe, and one
    skip reason that says why. The inline copies this replaced had drifted to
    three different default DSNs and skipped with a message indistinguishable
    from "PostgreSQL is not installed".
    """
    return pg_oracle.connect()


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_float_rendering_matches_real_postgres(wire):
    """The tables above say what we believe; this says what PostgreSQL does.

    Sweeps random bit patterns across each type's whole range, comparing the
    rendered TEXT rather than the decoded value."""
    pg = _pg_reference()
    assert pg is not None

    class _Raw(psycopg.adapt.Loader):
        def load(self, data):
            return bytes(data).decode()

    pg.adapters.register_loader(FLOAT4_OID, _Raw)
    pg.adapters.register_loader(FLOAT8_OID, _Raw)

    rng = random.Random(20260830)
    cases: list[tuple[str, float]] = []
    for _ in range(150):
        (v,) = struct.unpack("!f", struct.pack("!I", rng.getrandbits(32)))
        if v == v and abs(v) != float("inf"):
            cases.append(("real", v))
    for _ in range(150):
        (v,) = struct.unpack("!d", struct.pack("!Q", rng.getrandbits(64)))
        if v == v and abs(v) != float("inf"):
            cases.append(("float8", v))
    cases += [(t, v) for t in ("real", "float8") for v in (80, 0, 1e6, 0.0001, 1e-5, 3.5, 1e15)]

    try:
        mismatches = []
        for tag, value in cases:
            with pg.cursor() as a, wire.cursor() as b:
                a.execute(f"select %s::{tag}", (value,))
                b.execute(f"select %s::{tag}", (value,))
                theirs, ours = a.fetchall()[0][0], b.fetchall()[0][0]
            if theirs != ours:
                mismatches.append((tag, value, theirs, ours))
    finally:
        pg.close()
    assert not mismatches, "\n".join(f"{t} {v!r}: pg={x!r} us={y!r}" for t, v, x, y in mismatches)
