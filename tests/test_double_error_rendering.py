"""How a DOUBLE is rendered inside an error message.

mongod has **two** double renderings and they are not interchangeable. Both
were measured against 8.2.11 on 2026-09-07 by asking a real server, one value
at a time:

* the **value** form (``mongo::Value::toString``) is C's ``%g`` at precision 6
  — ``-0``, ``-1``, ``1.23457e+06``, ``-2.14748e+09``, ``0.000123457``. It is
  what ``$mergeObjects``, ``$replaceRoot``, ``$ln``, ``$log`` and ``$log10``
  echo.
* the **spec** form echoes a stage's own specification back as ``%.16g``,
  with a ``.0`` appended when that leaves no ``.`` or ``e`` — ``-0.0``,
  ``-1.0``, ``1234567.0``, ``-2147483648.0``. It is what
  ``$firstN``/``$lastN``/``$maxN``/``$median``'s "specification must be an
  object" and ``$graphLookup``'s missing-``from`` echo use.

  It is **not** the shortest round-trip form, though the two agree for every
  ordinary value. They part company at the bottom of the range: ``1e-308``
  echoes as ``9.999999999999999e-309`` and ``5e-324`` as
  ``4.940656458412465e-324``, because mongod prints the double's actual value
  to 16 digits rather than the shortest string that parses back to it. The
  first version of this file asserted the round-trip form and passed, because
  the code had been written from the same assumption; the differential gate
  against a real mongod is what caught it. The denormal rows below exist so
  that cannot recur.

One renderer used to serve both, so every value message rendered a double the
spec way. Fixing that naively — switching the shared renderer — silently breaks
``$graphLookup``, which is why it now takes the spec renderer explicitly.

Note ``$ln`` renders its operand as a DOUBLE whatever the operand's BSON type:
an Int32 ``-2147483648`` comes back as ``-2.14748e+09``, not as itself.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer
from secantus.bsontypes import fmt_double_parse, fmt_double_value

#: (value, value-form, spec-form) — every row measured against mongod 8.2.11.
RENDERINGS = [
    (-0.0, "-0", "-0.0"),
    (0.0, "0", "0.0"),
    (-1.0, "-1", "-1.0"),
    (1.5, "1.5", "1.5"),
    (100.0, "100", "100.0"),
    (123456.0, "123456", "123456.0"),
    (1234567.0, "1.23457e+06", "1234567.0"),
    (9999999.0, "1e+07", "9999999.0"),
    (-2147483648.0, "-2.14748e+09", "-2147483648.0"),
    (1e308, "1e+308", "1e+308"),
    (1e-308, "1e-308", "9.999999999999999e-309"),
    (5e-324, "4.94066e-324", "4.940656458412465e-324"),
    (1e-310, "1e-310", "9.999999999999969e-311"),
    (2.2250738585072014e-308, "2.22507e-308", "2.225073858507201e-308"),
    (1e-5, "1e-05", "1e-05"),
    (0.000123456789, "0.000123457", "0.000123456789"),
    (3.14159265358979, "3.14159", "3.14159265358979"),
    (1e16, "1e+16", "1e+16"),
    (float("inf"), "inf", "inf"),
    (float("-inf"), "-inf", "-inf"),
]


@pytest.mark.parametrize("value,value_form,spec_form", RENDERINGS)
def test_the_two_double_renderings(value: float, value_form: str, spec_form: str) -> None:
    assert fmt_double_value(value) == value_form
    assert fmt_double_parse(value) == spec_form


def test_nan_renders_lower_case_in_both() -> None:
    """Python's ``str`` gives ``nan`` but ``repr`` of a numpy-ish NaN and Rust's
    ``to_string`` give ``NaN``; mongod writes it lower-case."""
    nan = float("nan")
    assert fmt_double_value(nan) == "nan"
    assert fmt_double_parse(nan) == "nan"


def test_the_two_forms_actually_differ() -> None:
    """A guard against someone collapsing them back into one function."""
    differing = [v for v, a, b in RENDERINGS if a != b]
    assert len(differing) >= 6, "the two renderings must stay distinguishable"


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    host, port = srv.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    c = client["dbl"]["t"]
    c.insert_one({"_id": 1})
    try:
        yield c
    finally:
        client.close()
        srv.stop()


def _err(coll, pipeline) -> str:
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(coll.aggregate(pipeline))
    return str(exc.value).split(", full error")[0]


@pytest.mark.parametrize("value,value_form", [(v, a) for v, a, _ in RENDERINGS])
def test_mergeobjects_uses_the_value_form(coll, value: float, value_form: str) -> None:
    msg = _err(coll, [{"$project": {"_id": 0, "r": {"$mergeObjects": value}}}])
    assert f"but input {value_form} is of type double" in msg


@pytest.mark.parametrize("value,value_form", [(v, a) for v, a, _ in RENDERINGS])
def test_replaceroot_uses_the_value_form(coll, value: float, value_form: str) -> None:
    msg = _err(coll, [{"$replaceRoot": {"newRoot": {"$literal": value}}}])
    assert f"value was: {value_form}." in msg


@pytest.mark.parametrize(
    "value,value_form",
    [(v, a) for v, a, _ in RENDERINGS if v <= 0 or v != v],
)
def test_ln_domain_error_uses_the_value_form(coll, value: float, value_form: str) -> None:
    msg = _err(coll, [{"$project": {"_id": 0, "r": {"$ln": value}}}])
    assert msg.endswith(f"$ln's argument must be a positive number, but is {value_form}")


def test_ln_renders_an_int_operand_as_a_double() -> None:
    """mongod converts before rendering, so an Int32 gets the ``%g`` treatment."""
    assert fmt_double_value(float(-2147483648)) == "-2.14748e+09"


@pytest.mark.parametrize("value,spec_form", [(v, b) for v, _, b in RENDERINGS])
def test_firstn_specification_echo_uses_the_spec_form(coll, value: float, spec_form: str) -> None:
    msg = _err(coll, [{"$addFields": {"x": {"$firstN": value}}}])
    assert f"found $firstN: {spec_form}" in msg


@pytest.mark.parametrize("value,spec_form", [(v, b) for v, _, b in RENDERINGS])
def test_graphlookup_specification_echo_uses_the_spec_form(
    coll, value: float, spec_form: str
) -> None:
    """``$graphLookup`` echoes the SPEC, so it must not follow the value
    renderer — this is the case that breaks if the two are merged."""
    msg = _err(
        coll,
        [
            {
                "$graphLookup": {
                    "startWith": value,
                    "connectFromField": "a",
                    "connectToField": "b",
                    "as": "c",
                }
            }
        ],
    )
    assert f"startWith: {spec_form}," in msg
