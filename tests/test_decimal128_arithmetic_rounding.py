"""Decimal128 arithmetic and rounding, on both servers.

`decimal.rs` had `add`, `mul` and `div_int` — the primitives `$sum` / `$avg`
accumulate with — but they were never wired into the EXPRESSION path. So
`{$add: [Decimal128("2.5"), 1]}` reported the operator unsupported on the
standalone Rust server while `{$sum: ...}` over the same values answered.

These are the operations IEEE 754-2008 defines as *correctly rounded*, which is
what makes them exactly matchable: add, subtract, multiply and the rounding
family have one right answer and mongod's Intel decimal library computes it.
The transcendentals do not — see `tasks/backlog.md` §7 for why they are
deliberately still deferred.

Every expectation probed against mongod 8.2.11 (2026-09-03).
"""

from __future__ import annotations

import contextlib

import pytest
from bson import Decimal128 as D
from bson import Int64

from secantus import SecantusDBServer


@contextlib.contextmanager
def _python_client(tmp_path):
    import pymongo

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        client = pymongo.MongoClient(
            srv.uri, serverSelectionTimeoutMS=5000, datetime_conversion="DATETIME_AUTO"
        )
        try:
            yield client
        finally:
            client.close()


@contextlib.contextmanager
def _rust_client(tmp_path):
    import pymongo

    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host,
            port,
            directConnection=True,
            serverSelectionTimeoutMS=5000,
            # A BSON date is any int64 of millis; year 292278994 is outside
            # Python's `datetime` and the default codec refuses to DECODE it.
            datetime_conversion="DATETIME_AUTO",
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        srv.stop()


@pytest.fixture(scope="module", params=["python", "rust"])
def _client(request, tmp_path_factory):
    factory = _python_client if request.param == "python" else _rust_client
    with factory(tmp_path_factory.mktemp("decarith")) as client:
        yield client


@pytest.fixture
def coll(_client):
    c = _client["decarith"]["c"]
    c.drop()
    c.insert_one({"_id": 1})
    return c


def _z(coll, expr):
    return list(coll.aggregate([{"$addFields": {"z": expr}}]))[0].get("z")


class TestArithmetic:
    @pytest.mark.parametrize(
        ("expr", "expected"),
        [
            ({"$add": [D("2.5"), 1]}, D("3.5")),
            # The QUANTUM survives: a trailing zero is significant.
            ({"$add": [D("2.50"), 2]}, D("4.50")),
            ({"$subtract": [D("2.5"), D("0.5")]}, D("2.0")),
            ({"$subtract": [1, D("2.5")]}, D("-1.5")),
            # `2.5 * 2` is `5.0`, NOT `5` -- trailing zeros are significant.
            ({"$multiply": [D("2.5"), 2]}, D("5.0")),
            # 34 digits are carried, not the 15 a double would hold.
            (
                {"$add": [D("1.000000000000000000000000000000001"), D("1")]},
                D("2.000000000000000000000000000000001"),
            ),
            ({"$add": [D("1.5"), D("2.5"), 1]}, D("5.0")),
        ],
    )
    def test_the_quantum_is_load_bearing(self, coll, expr, expected):
        assert _z(coll, expr) == expected

    def test_a_double_operand_widens_the_quantum(self, coll):
        """mongod takes the double at 15 significant digits for ARITHMETIC.

        This is not the same conversion the accumulators use, which strips to
        the short form -- picking that one answered `4.5`.
        """
        assert _z(coll, {"$add": [D("2.5"), 2.0]}) == D("4.50000000000000")
        assert _z(coll, {"$multiply": [D("2.5"), 2.0]}) == D("5.000000000000000")

    @pytest.mark.parametrize(
        ("expr", "expected"),
        [
            ({"$add": [D("Infinity"), 1]}, D("Infinity")),
            ({"$add": [D("NaN"), 1]}, D("NaN")),
            ({"$add": [D("-Infinity"), D("Infinity")]}, D("NaN")),
        ],
    )
    def test_the_non_finite_edges(self, coll, expr, expected):
        assert str(_z(coll, expr)) == str(expected)


class TestRounding:
    @pytest.mark.parametrize(
        ("expr", "expected"),
        [
            # Directional, and the result takes an INTEGER quantum: `2.00`
            # ceils to `2`, not `2.00`.
            ({"$ceil": D("2.5")}, "3"),
            ({"$ceil": D("-2.5")}, "-2"),
            ({"$ceil": D("2.00")}, "2"),
            ({"$floor": D("2.5")}, "2"),
            ({"$floor": D("-2.5")}, "-3"),
            ({"$trunc": D("2.5")}, "2"),
            ({"$trunc": D("-2.5")}, "-2"),
            # Ties to EVEN: 2.5 -> 2 but 3.5 -> 4.
            ({"$round": D("2.5")}, "2"),
            ({"$round": D("3.5")}, "4"),
            ({"$round": D("-2.5")}, "-2"),
        ],
    )
    def test_the_four_modes(self, coll, expr, expected):
        assert str(_z(coll, expr)) == expected

    @pytest.mark.parametrize(
        ("expr", "expected"),
        [
            ({"$round": [D("2.567"), 2]}, "2.57"),
            # `$trunc` at the same place goes toward zero instead.
            ({"$trunc": [D("2.567"), 2]}, "2.56"),
            # The place sets the QUANTUM whether or not it changed the value.
            ({"$round": [D("2.5"), 2]}, "2.50"),
            ({"$trunc": [D("2.5"), 3]}, "2.500"),
            # A negative place coarsens it.
            ({"$round": [D("25"), -1]}, "2E+1"),
            ({"$round": [D("5"), -1]}, "0E+1"),
            ({"$round": [D("15"), -1]}, "2E+1"),
            # The whole value sits below the target place: the deciding digit
            # is an implicit leading zero, not the coefficient's first digit.
            ({"$round": [D("9.995"), -3]}, "0E+3"),
            ({"$trunc": [D("9.995"), -3]}, "0E+3"),
        ],
    )
    def test_the_place_argument(self, coll, expr, expected):
        assert str(_z(coll, expr)) == expected

    def test_ceil_and_floor_of_an_infinity_are_NaN(self, coll):
        """An asymmetry, measured rather than reasoned.

        `$trunc` and `$round` pass a decimal infinity through, and a DOUBLE
        infinity passes through `$ceil` unchanged — but `$ceil` / `$floor` of a
        decimal infinity is NaN.
        """
        for op in ("$ceil", "$floor"):
            assert str(_z(coll, {op: D("Infinity")})) == "NaN", op
            assert str(_z(coll, {op: D("-Infinity")})) == "NaN", op
        for op in ("$trunc", "$round"):
            assert str(_z(coll, {op: D("Infinity")})) == "Infinity", op
        assert _z(coll, {"$ceil": float("inf")}) == float("inf")


@pytest.mark.parametrize("op", ["$ceil", "$floor", "$trunc", "$round"])
@pytest.mark.parametrize("value", [float("inf"), float("-inf"), float("nan")])
def test_a_non_finite_double_never_reaches_the_client_as_an_error(coll, op, value):
    """`math.ceil(inf)` raises `OverflowError` and `math.trunc(nan)` raises
    `ValueError`; both escaped the evaluator as `internal server error`.

    mongod answers the value. Five crash-class shapes were found this way, one
    of them by this file's own control assertion — an operator that refuses a
    value it should pass through is a bug the probe corpus never asked about,
    because the corpus has no infinities.
    """
    got = _z(coll, {op: value})
    assert isinstance(got, float)
    assert (got != got) if (value != value) else got == value


class TestNonFiniteAndBoundaries:
    """Value classes the probe corpus did not contain until 2026-09-03.

    Widening it added the infinities, NaN, signed zero, the int32/int64
    boundaries and the BSON types it had skipped. That immediately surfaced
    THIRTEEN crash-class bugs (each an `internal server error` reachable from
    any query) and six wrong values — on a corpus that had run tens of
    thousands of times without one infinity in it.
    """

    @pytest.mark.parametrize("op", ["$sin", "$cos", "$tan", "$asin", "$acos", "$atan", "$acosh"])
    def test_nan_is_never_a_domain_error(self, coll, op):
        """NaN answers NaN for EVERY trig operator, in both numeric types —
        never `50989`, even for the range-limited ones where `-1 <= nan <= 1`
        is trivially false."""
        got = _z(coll, {op: float("nan")})
        assert isinstance(got, float) and got != got
        assert str(_z(coll, {op: D("NaN")})) == "NaN"

    @pytest.mark.parametrize(
        ("op", "expected"),
        [
            ("$atan", "1.570796326794896619231321691639751"),
            ("$sinh", "Infinity"),
            ("$cosh", "Infinity"),
            ("$tanh", "1"),
            ("$asinh", "Infinity"),
            ("$acosh", "Infinity"),
        ],
    )
    def test_a_decimal_infinity_takes_the_operator_s_limit(self, coll, op, expected):
        """The series cannot run on an infinity — it raised
        `decimal.InvalidOperation` out of the evaluator. `$atan` answers pi/2
        to all 34 digits, not a float-precision approximation."""
        assert str(_z(coll, {op: D("Infinity")})) == expected

    def test_the_accumulators_unwrap_a_one_element_array(self, coll):
        """`{$sum: [[1, 2]]}` sums the INNER array; `{$sum: [[1], [2]]}` has
        two operands, both arrays, both ignored."""
        assert _z(coll, {"$sum": [[1, 2]]}) == 3
        assert _z(coll, {"$sum": [[1], [2]]}) == 0
        assert _z(coll, {"$max": [[1]]}) == 1
        assert _z(coll, {"$max": [[1], [2]]}) == [2]
        assert _z(coll, {"$min": [[3], [1]]}) == [1]

    def test_integer_rounding_does_not_go_through_a_double(self, coll):
        """`9223372036854775807` does not survive a round trip through f64,
        and mongod keeps it exactly."""
        assert _z(coll, {"$trunc": [Int64(2**63 - 1), -2]}) == 9223372036854775800
        assert _z(coll, {"$round": [Int64(2**63 - 1), -2]}) == 9223372036854775800
        # Toward ZERO for negatives, which Python's floor-based `%` does not give.
        assert _z(coll, {"$trunc": [-12345, -1]}) == -12340
        assert _z(coll, {"$round": [-12345, -1]}) == -12340

    def test_rounding_up_out_of_int64_is_an_error(self, coll):
        """Not a widening to double: mongod reports 51080."""
        from pymongo.errors import OperationFailure

        with pytest.raises(OperationFailure) as exc:
            _z(coll, {"$round": [Int64(2**63 - 1), -1]})
        assert exc.value.code == 51080

    def test_a_date_beyond_pythons_datetime_range_still_answers(self, coll):
        """A BSON date is any int64 of milliseconds. Year 292278994 is outside
        `datetime`, and `fromtimestamp` raised `OverflowError` there."""
        got = _z(coll, {"$toDate": Int64(2**63 - 1)})
        assert int(got) == 2**63 - 1

    @pytest.mark.parametrize("value", [1e308, -1e308, float("inf"), float("nan")])
    def test_a_date_outside_int64_is_a_conversion_error(self, coll, value):
        """mongod refuses these with 241 rather than saturating — which is what
        Rust's `as i64` silently did."""
        from pymongo.errors import OperationFailure

        with pytest.raises(OperationFailure) as exc:
            _z(coll, {"$toDate": value})
        assert exc.value.code == 241


def test_log_validates_its_base_before_deferring_a_decimal(coll):
    """The four checks run argument-type, base-type, argument-domain,
    base-domain — so a bad base is named even when the argument is a decimal
    the engine cannot compute with."""
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure) as exc:
        _z(coll, {"$log": [D("2.5"), 1]})
    assert exc.value.code == 28759
    assert "must be a positive number not equal to 1, but is 1" in str(exc.value)
