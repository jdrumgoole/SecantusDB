"""The decimal128 trig / hyperbolic family answers instead of refusing.

The Rust server used to refuse every one of these for a `Decimal128` operand —
108 of 285 measured shapes came back "a construct the Rust server does not
support" where mongod returns a number, which is the least faithful outcome
available. It now answers all fifteen, sharing the Python engine's results.

**Exact last-digit agreement with mongod is NOT asserted here, deliberately.**
mongod is itself correctly-rounded only about 78% of the time — it carries
Intel RDFP's error — so pinning its digits would pin an implementation
artefact, the same trap CLAUDE.md records for `codeName` and the expected-type
list. What is pinned is the part that is contractual: the BSON type, the domain
errors, the exact constants at the boundaries, and agreement with the true
value to a margin that both servers clear. `tools/probes/decimal_transcendental_rounding.py`
tracks the last digit against a 60-digit reference.
"""

from __future__ import annotations

from decimal import Decimal, localcontext

import pymongo
import pytest
from bson import Decimal128

from secantus import SecantusDBServer

#: `(operator, argument, true value to 30 significant digits)`.
#: References COMPUTED with mpmath at 60 digits and truncated to 30, not typed
#: by hand -- a first version of this table was written from memory and five of
#: the fifteen rows were wrong, which the tests caught but a looser assertion
#: would not have. 30 digits leaves margin for the last-ulp disagreement that is
#: mongod's, not ours.
CASES = [
    ("$sin", "1.5", "0.997494986604054430941723371141"),
    ("$cos", "1.5", "0.0707372016677029100881898514343"),
    ("$tan", "1.5", "14.1014199471717193876460836520"),
    ("$asin", "0.5", "0.523598775598298873077107230547"),
    ("$acos", "0.5", "1.04719755119659774615421446109"),
    ("$atan", "1.5", "0.982793723247329067985710611015"),
    ("$sinh", "1.5", "2.12927945509481749683438749468"),
    ("$cosh", "1.5", "2.35240961524324732576766796544"),
    ("$tanh", "1.5", "0.905148253644866438242303696456"),
    ("$acosh", "1.5", "0.962423650119206894995517826849"),
    ("$asinh", "1.5", "1.19476321728710930411193082852"),
    ("$ln", "1.5", "0.405465108108164381978013115464"),
    ("$log10", "1.5", "0.176091259055681242081289008531"),
    ("$exp", "1.5", "4.48168907033806482260205546012"),
    ("$sqrt", "1.5", "1.22474487139158904909864203735"),
]


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    path = tmp_path_factory.mktemp("dectrig")
    server = SecantusDBServer(port=0, storage_path=str(path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["dectrig"]["c"]
    finally:
        client.close()
        server.stop()


def _apply(coll, op, arg):
    coll.delete_many({})
    coll.insert_one({"_id": 1, "v": Decimal128(arg)})
    return list(coll.aggregate([{"$project": {"r": {op: "$v"}}}]))[0]["r"]


@pytest.mark.parametrize("op,arg,expected", CASES, ids=[c[0][1:] for c in CASES])
def test_a_decimal_operand_answers_a_decimal(coll, op, arg, expected):
    """The operator returns a `Decimal128` -- not a double, and not an error.

    The BSON TYPE is the load-bearing half: answering a double would compare
    and sort differently downstream even when the digits look right.
    """
    got = _apply(coll, op, arg)
    assert isinstance(got, Decimal128), f"{op} answered {type(got).__name__}"
    with localcontext() as ctx:
        ctx.prec = 30
        assert +got.to_decimal() == +Decimal(expected)


def test_every_operator_keeps_34_digits(coll):
    """A decimal128 answer carries the format's full precision."""
    got = _apply(coll, "$sinh", "1.5")
    digits = len(got.to_decimal().as_tuple().digits)
    assert digits >= 33, f"only {digits} significant digits"


@pytest.mark.parametrize(
    "op,arg,domain",
    [
        ("$asin", "2", "[-1,1]"),
        ("$acos", "2", "[-1,1]"),
        ("$acosh", "0.5", "[1,inf]"),
        ("$atanh", "2", "[-1,1]"),
    ],
)
def test_an_out_of_domain_decimal_is_mongods_50989(coll, op, arg, domain):
    """The domain errors survive -- implementing the series must not swallow
    them, and they are what a refusal used to be confused with."""
    coll.delete_many({})
    coll.insert_one({"_id": 1, "v": Decimal128(arg)})
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        list(coll.aggregate([{"$project": {"r": {op: "$v"}}}]))
    assert excinfo.value.code == 50989
    assert f"value must be in {domain}" in excinfo.value.details["errmsg"]


@pytest.mark.parametrize("op", ["$sin", "$cos", "$tan", "$sinh", "$cosh", "$tanh", "$atan"])
def test_nan_answers_nan_not_a_domain_error(coll, op):
    got = _apply(coll, op, "NaN")
    assert isinstance(got, Decimal128) and got.to_decimal().is_nan()


@pytest.mark.parametrize(
    "op,arg,expected",
    [
        # The zero constants are mongod's own quanta, which no series produces.
        ("$sin", "0", "0"),
        ("$cos", "0", "1.000000000000000000000000000000000"),
        ("$tan", "0", "0E-40"),
        ("$sinh", "0", "0E-40"),
        ("$cosh", "0", "1.000000000000000000000000000000000"),
        ("$tanh", "0", "0"),
        ("$atan", "0", "0"),
        ("$asin", "0", "0E-40"),
        # asin(1) and acos(0) are exactly pi/2 at 34 digits.
        ("$asin", "1", "1.570796326794896619231321691639751"),
        ("$acos", "0", "1.570796326794896619231321691639751"),
        ("$acosh", "1", "0"),
    ],
)
def test_the_boundary_constants_are_mongods(coll, op, arg, expected):
    got = _apply(coll, op, arg)
    assert str(got) == expected
