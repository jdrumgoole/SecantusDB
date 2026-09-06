"""`find` evaluates an expression-valued projection field.

Computed projections were unimplemented, and the Python server dropped the
fields **silently** — `ok: 1`, and a document without the field the client asked
for. (The Rust server refused honestly, `2 BadValue: projection is not supported
by the Rust server`, which is the better of the two behaviours by this project's
rule and is unchanged here; that half is filed.)

Every expectation below is mongod 8.2.11's own answer, measured 2026-09-06 with
`scratchpad/probe_proj.py` and `probe_proj2.py` and re-asserted against the live
server by `test_expectations_match_mongod` in the differential gate. Several of
the classification rules are not guessable and were got wrong before they were
measured:

* a **string** is an expression — `"$a"` is a field path and `"plain"` is a
  literal constant emitted on every document. Neither is an include flag.
* a **number or bool** is always a flag — `{n: 2}` includes a field called `n`,
  it does not set `n` to 2.
* `{$literal: 0}` and `{$literal: false}` are expressions yielding `0` / `false`,
  NOT exclusions.
* a plain subdocument is computed only if a leaf below it is: `{o: {p: 1}}` is an
  inclusion of `o.p`, `{o: {p: {$add: …}}}` is a computed subdocument.

And two run-time rules that look alike and are not: a bare field **reference**
that resolves to nothing OMITS the output field, while an **expression** over a
missing field yields `null`.
"""

from __future__ import annotations

import datetime

import pymongo
import pytest
from bson import Decimal128, Int64, MaxKey, MinKey, ObjectId, Timestamp

from secantus import SecantusDBServer
from secantus.projection import ProjectionError, apply_projection

DOCS = [
    {"_id": 1, "a": 2, "b": 3, "s": "hi", "arr": [1, 2, 3]},
    {"_id": 2, "b": 9, "s": "yo", "arr": []},
]


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    c = cli["computedproj"]["c"]
    c.insert_many([dict(d) for d in DOCS])
    try:
        yield c
    finally:
        cli.close()
        srv.stop()


def _rows(coll, spec):
    return [dict(d) for d in coll.find({}, spec).sort("_id", 1)]


# --- the expressions get evaluated ------------------------------------------


@pytest.mark.parametrize(
    ("spec", "expected"),
    [
        ({"n": {"$add": ["$a", 1]}}, [{"_id": 1, "n": 3}, {"_id": 2, "n": None}]),
        ({"n": {"$multiply": ["$a", 2]}}, [{"_id": 1, "n": 4}, {"_id": 2, "n": None}]),
        ({"n": {"$literal": 7}}, [{"_id": 1, "n": 7}, {"_id": 2, "n": 7}]),
        ({"n": {"$concat": ["$s", "!"]}}, [{"_id": 1, "n": "hi!"}, {"_id": 2, "n": "yo!"}]),
        ({"n": {"$toUpper": "$s"}}, [{"_id": 1, "n": "HI"}, {"_id": 2, "n": "YO"}]),
        (
            {"n": {"$cond": [{"$gt": ["$b", 5]}, "big", "small"]}},
            [{"_id": 1, "n": "small"}, {"_id": 2, "n": "big"}],
        ),
        ({"n": {"$size": "$arr"}}, [{"_id": 1, "n": 3}, {"_id": 2, "n": 0}]),
    ],
)
def test_an_expression_field_is_evaluated(coll, spec, expected):
    assert _rows(coll, spec) == expected


def test_a_computed_field_forces_inclusion(coll):
    """Only `_id` and the computed field survive — the rest of the document is
    dropped, as in any inclusion projection."""
    assert _rows(coll, {"n": {"$literal": 1}}) == [{"_id": 1, "n": 1}, {"_id": 2, "n": 1}]
    # ...and it composes with an explicit inclusion.
    assert _rows(coll, {"b": 1, "n": {"$literal": 1}}) == [
        {"_id": 1, "b": 3, "n": 1},
        {"_id": 2, "b": 9, "n": 1},
    ]
    # ...and with `_id: 0`.
    assert _rows(coll, {"_id": 0, "n": {"$literal": 1}}) == [{"n": 1}, {"n": 1}]


def test_mixing_a_computed_field_with_an_exclusion_is_refused(coll):
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _rows(coll, {"b": 0, "n": {"$literal": 1}})
    assert exc.value.code == 31252
    assert "Cannot use expression other than $meta in exclusion projection" in str(exc.value)


# --- the classification rules -----------------------------------------------


@pytest.mark.parametrize(
    ("spec", "expected"),
    [
        # A string is an EXPRESSION: a field path, or a literal constant.
        ({"n": "$a"}, [{"_id": 1, "n": 2}, {"_id": 2}]),
        ({"n": "plain"}, [{"_id": 1, "n": "plain"}, {"_id": 2, "n": "plain"}]),
        # A number or bool is a FLAG, never a literal.
        ({"n": 2}, [{"_id": 1}, {"_id": 2}]),
        ({"n": True}, [{"_id": 1}, {"_id": 2}]),
        # `$literal` of a falsy value is still a value, not an exclusion.
        ({"n": {"$literal": 0}}, [{"_id": 1, "n": 0}, {"_id": 2, "n": 0}]),
        ({"n": {"$literal": False}}, [{"_id": 1, "n": False}, {"_id": 2, "n": False}]),
        # A plain subdocument is an inclusion; a computed one is computed.
        ({"o": {"p": 1}}, [{"_id": 1}, {"_id": 2}]),
        (
            {"o": {"p": {"$add": ["$a", 1]}}},
            [{"_id": 1, "o": {"p": 3}}, {"_id": 2, "o": {"p": None}}],
        ),
        # A dotted output key builds the nesting rather than a literal dotted key.
        (
            {"o.p": {"$add": ["$a", 1]}},
            [{"_id": 1, "o": {"p": 3}}, {"_id": 2, "o": {"p": None}}],
        ),
        # `_id` itself can be computed.
        ({"_id": {"$add": ["$a", 1]}}, [{"_id": 3}, {"_id": None}]),
    ],
)
def test_the_classification_rules(coll, spec, expected):
    assert _rows(coll, spec) == expected


# --- missing input: reference omits, expression yields null -----------------


def test_a_bare_reference_to_a_missing_field_omits_the_output_field(coll):
    assert _rows(coll, {"n": "$a"}) == [{"_id": 1, "n": 2}, {"_id": 2}]
    assert _rows(coll, {"n": "$nope"}) == [{"_id": 1}, {"_id": 2}]


def test_an_expression_over_a_missing_field_yields_null(coll):
    assert _rows(coll, {"n": {"$add": ["$nope", 1]}}) == [
        {"_id": 1, "n": None},
        {"_id": 2, "n": None},
    ]
    assert _rows(coll, {"n": {"$concat": ["$nope", "x"]}}) == [
        {"_id": 1, "n": None},
        {"_id": 2, "n": None},
    ]


# --- an evaluation failure is an EXECUTION error ----------------------------


def test_a_bad_operand_is_wrapped_as_an_executor_error(coll):
    """`$size` on a non-array is an error, not null — and because it is found
    per document, mongod wraps it with the command and namespace."""
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        _rows(coll, {"n": {"$size": "$s"}})
    assert exc.value.code == 17124
    msg = str(exc.value)
    assert msg.startswith("Executor error during find command: computedproj.c :: caused by :: ")
    assert "The argument to $size must be an array" in msg


# --- neighbouring projection behaviour that must not move -------------------


@pytest.mark.parametrize(
    ("spec", "expected"),
    [
        ({"b": 1}, [{"_id": 1, "b": 3}, {"_id": 2, "b": 9}]),
        ({"b": 0, "s": 0, "arr": 0}, [{"_id": 1, "a": 2}, {"_id": 2}]),
        ({"_id": 0, "b": 1}, [{"b": 3}, {"b": 9}]),
        (
            {"arr": {"$slice": 1}},
            [
                {"_id": 1, "a": 2, "b": 3, "s": "hi", "arr": [1]},
                {"_id": 2, "b": 9, "s": "yo", "arr": []},
            ],
        ),
    ],
)
def test_ordinary_projections_are_unchanged(coll, spec, expected):
    assert _rows(coll, spec) == expected


# --------------------------------------------------------------------------
# Value KIND: which projection values are flags and which are literals.
#
# Measured one BSON type at a time against mongod 8.2.11 (2026-09-06). The
# result is narrower than it looks: only a BSON *number* or *bool* is an
# include/exclude flag. Everything else is a literal constant that replaces the
# field on every document -- including `null`, an empty string, and an array.
# --------------------------------------------------------------------------


LITERAL_VALUES = [
    ("empty string", ""),
    ("plain string", "plain"),
    ("null", None),
    ("array", [1, 2]),
    ("empty array", []),
    ("datetime", datetime.datetime(2020, 1, 1)),
    ("objectid", ObjectId("0" * 24)),
    ("timestamp", Timestamp(1, 2)),
    ("minkey", MinKey()),
    ("maxkey", MaxKey()),
]


@pytest.mark.parametrize("label,value", LITERAL_VALUES, ids=[c[0] for c in LITERAL_VALUES])
def test_non_numeric_value_is_a_literal_constant(label: str, value: object) -> None:
    """A non-numeric projection value replaces the field on every document."""
    doc = {"_id": 1, "a": 5}
    assert apply_projection(doc, {"n": value}) == {"_id": 1, "n": value}
    # …including over a field that already exists.
    assert apply_projection(doc, {"a": value}) == {"_id": 1, "a": value}


def test_decimal128_is_a_flag_not_a_literal() -> None:
    """`Decimal128` is a BSON NUMBER, so it is a flag — and its zero excludes.

    `bool(Decimal128("0"))` is `True` in Python (it is an object), so the naive
    truthiness test gets this exactly backwards.
    """
    doc = {"_id": 1, "a": 5, "b": 2}
    assert apply_projection(doc, {"a": Decimal128("1.5")}) == {"_id": 1, "a": 5}
    assert apply_projection(doc, {"n": Decimal128("1.5")}) == {"_id": 1}
    assert apply_projection(doc, {"n": Decimal128("0")}) == {"_id": 1, "a": 5, "b": 2}


def test_int64_is_a_flag() -> None:
    doc = {"_id": 1, "a": 5, "b": 2}
    assert apply_projection(doc, {"a": Int64(3)}) == {"_id": 1, "a": 5}
    assert apply_projection(doc, {"a": Int64(0)}) == {"_id": 1, "b": 2}


# --------------------------------------------------------------------------
# A plain sub-document is a SUB-PROJECTION, classified per leaf.
# --------------------------------------------------------------------------


def test_subdocument_mixes_inclusion_and_computed_leaves() -> None:
    """`{o: {p: 1, z: "$b"}}` includes `o.p` AND computes `o.z`.

    The whole sub-document is not one computed expression: mongod keeps the
    STORED `o.p` (9, not the literal 1) and appends the computed `o.z`.
    """
    doc = {"_id": 1, "a": 5, "b": 2, "o": {"p": 9, "r": 4}}
    assert apply_projection(doc, {"o": {"p": 1, "z": "$b"}}) == {
        "_id": 1,
        "o": {"p": 9, "z": 2},
    }
    # The dotted spelling is the same projection.
    assert apply_projection(doc, {"o.p": 1, "o.z": "$b"}) == {
        "_id": 1,
        "o": {"p": 9, "z": 2},
    }


def test_subdocument_inclusion_leaf_of_a_missing_field_is_omitted() -> None:
    """`{o: {q: "$b", r: 7}}`: `r` is an inclusion FLAG for `o.r`, not a literal 7."""
    doc = {"_id": 1, "b": 2, "o": {"p": 9, "r": 4}}
    assert apply_projection(doc, {"o": {"q": "$b", "r": 7}}) == {
        "_id": 1,
        "o": {"r": 4, "q": 2},
    }
    # A document with no `o` at all still gets the computed leaf.
    assert apply_projection({"_id": 2, "b": 3}, {"o": {"q": "$b", "r": 7}}) == {
        "_id": 2,
        "o": {"q": 3},
    }


def test_empty_sub_projection_is_an_error_at_any_depth() -> None:
    """mongod names the LEAF key, not the dotted path."""
    with pytest.raises(ProjectionError) as exc:
        apply_projection({"_id": 1}, {"n": {}})
    assert exc.value.code == 51270
    assert str(exc.value) == "Invalid empty sub-projection: n"

    with pytest.raises(ProjectionError) as exc:
        apply_projection({"_id": 1}, {"n": {"q": {}}})
    assert exc.value.code == 51270
    assert str(exc.value) == "Invalid empty sub-projection: q"
    assert exc.value.code_name == "Location51270"


# --------------------------------------------------------------------------
# A computed `_id` replaces the stored one AND moves to the end.
# --------------------------------------------------------------------------


def test_computed_id_is_appended_last() -> None:
    doc = {"_id": 1, "a": 5, "b": 2}
    out = apply_projection(doc, {"_id": "$b", "a": 1})
    assert out == {"a": 5, "_id": 2}
    assert list(out) == ["a", "_id"], "mongod puts a computed _id LAST"

    out = apply_projection(doc, {"_id": "zz", "a": 1})
    assert list(out) == ["a", "_id"]
    assert out["_id"] == "zz"


def test_computed_fields_follow_the_included_ones() -> None:
    doc = {"_id": 1, "a": 5, "b": 2}
    for spec in ({"n": "$b", "a": 1}, {"a": 1, "n": "$b"}):
        out = apply_projection(doc, spec)
        assert list(out) == ["_id", "a", "n"]
        assert out == {"_id": 1, "a": 5, "n": 2}


# --------------------------------------------------------------------------
# Three DIFFERENT errors for "a computed field in an exclusion projection",
# picked by the first offending leaf in SPEC ORDER.
# --------------------------------------------------------------------------


EXCLUSION_MIX = [
    (
        "literal string",
        {"a": 0, "n": "plain"},
        31310,
        'Cannot use an expression n: "plain" in an exclusion projection',
    ),
    (
        "literal null",
        {"a": 0, "n": None},
        31310,
        "Cannot use an expression n: null in an exclusion projection",
    ),
    (
        "literal array",
        {"a": 0, "n": [1, 2]},
        31310,
        "Cannot use an expression n: [ 1, 2 ] in an exclusion projection",
    ),
    (
        "empty array",
        {"a": 0, "n": []},
        31310,
        "Cannot use an expression n: [] in an exclusion projection",
    ),
    (
        "operator expr",
        {"a": 0, "n": {"$literal": 1}},
        31252,
        "Cannot use expression other than $meta in exclusion projection",
    ),
    (
        "operator expr $add",
        {"a": 0, "n": {"$add": [1, 2]}},
        31252,
        "Cannot use expression other than $meta in exclusion projection",
    ),
    (
        "numeric flag",
        {"a": 0, "n": Decimal128("1.5")},
        31253,
        "Cannot do inclusion on field n in exclusion projection",
    ),
    (
        "nested leaf named",
        {"a": 0, "o": {"p": {"q": "$b"}}},
        31310,
        'Cannot use an expression q: "$b" in an exclusion projection',
    ),
    (
        "inclusion leaf wins",
        {"a": 0, "o": {"p": 1, "z": "$b"}},
        31253,
        "Cannot do inclusion on field p in exclusion projection",
    ),
]


@pytest.mark.parametrize(
    "label,spec,code,message", EXCLUSION_MIX, ids=[c[0] for c in EXCLUSION_MIX]
)
def test_computed_in_exclusion_projection_errors(
    label: str, spec: dict, code: int, message: str
) -> None:
    with pytest.raises(ProjectionError) as exc:
        apply_projection({"_id": 1, "a": 5, "b": 2}, spec)
    assert exc.value.code == code
    assert str(exc.value) == message


def test_exclusion_mix_error_depends_on_spec_ORDER() -> None:
    """Swapping the two computed fields swaps the error code.

    This is the only reason the check walks the leaves rather than testing a
    set — and it is measured, not inferred.
    """
    doc = {"_id": 1, "a": 5, "b": 2}
    with pytest.raises(ProjectionError) as exc:
        apply_projection(doc, {"a": 0, "n": "plain", "m": {"$literal": 1}})
    assert exc.value.code == 31310
    with pytest.raises(ProjectionError) as exc:
        apply_projection(doc, {"a": 0, "m": {"$literal": 1}, "n": "plain"})
    assert exc.value.code == 31252


def test_id_zero_alongside_a_computed_field_is_not_a_mix() -> None:
    """`{_id: 0, …}` is the ordinary "inclusion without _id", not an exclusion."""
    doc = {"_id": 1, "a": 5, "b": 2}
    assert apply_projection(doc, {"_id": 0, "a": 1, "n": "$b"}) == {"a": 5, "n": 2}
