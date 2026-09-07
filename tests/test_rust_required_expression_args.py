"""Missing REQUIRED arguments to an expression operator, on the RUST server.

Twenty-five of these answered a WRONG VALUE rather than an error. The operators
read a required field with the evaluator's optional-field helper, which reports
an absent key as null, so `{$regexMatch: {}}` answered `false` and
`{$filter: {}}` answered `null` -- values a caller can branch on, from an
expression mongod rejects outright. The rest answered the generic
`2 BadValue: aggregation pipeline uses a stage or operator not supported by the
Rust server`, which blamed the operator when the argument was at fault.

MISSING is not NULL here, and the distinction is the fix: `{$trim: {input: null}}`
is legal and yields null, and `{$regexMatch: {input: null, regex: "a"}}` is legal
and yields false. Only an ABSENT key is an error.

Every code and message below is mongod 8.2.11's own, measured 2026-09-07 by
dropping one field at a time from a VALID argument document (57 cases, now at 0
divergences). They are written as literals rather than compared against the
Python engine: parity with the other engine is equally satisfied by both being
wrong.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_reqargs") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["reqargs"]
    d.c.drop()
    d.c.insert_one({"_id": 1, "s": "hi", "arr": [1, 2]})
    try:
        yield d
    finally:
        cli.close()


def _agg(db, expr, stage="$addFields"):
    return list(db.c.aggregate([{stage: {"x": expr}}]))


# (expression, code, message) -- mongod 8.2.11, inside an `$addFields` stage,
# which wraps a parse error as `Invalid $addFields :: caused by :: <message>`.
MISSING_CASES = [
    ({"$trim": {}}, 50695, "$trim requires an 'input' field"),
    ({"$ltrim": {}}, 50695, "$ltrim requires an 'input' field"),
    ({"$rtrim": {}}, 50695, "$rtrim requires an 'input' field"),
    ({"$regexFind": {}}, 31022, "$regexFind requires 'input' parameter"),
    ({"$regexFind": {"input": "a"}}, 31023, "$regexFind requires 'regex' parameter"),
    ({"$regexFindAll": {}}, 31022, "$regexFindAll requires 'input' parameter"),
    ({"$regexMatch": {}}, 31022, "$regexMatch requires 'input' parameter"),
    ({"$regexMatch": {"input": "a"}}, 31023, "$regexMatch requires 'regex' parameter"),
    ({"$reduce": {}}, 40077, "$reduce requires 'input' to be specified"),
    (
        {"$reduce": {"input": [1], "in": 1}},
        40078,
        "$reduce requires 'initialValue' to be specified",
    ),
    (
        {"$reduce": {"input": [1], "initialValue": 0}},
        40079,
        "$reduce requires 'in' to be specified",
    ),
    ({"$filter": {}}, 28648, "Missing 'input' parameter to $filter"),
    ({"$filter": {"input": [1]}}, 28650, "Missing 'cond' parameter to $filter"),
    ({"$map": {}}, 16880, "Missing 'input' parameter to $map"),
    ({"$map": {"input": [1]}}, 16882, "Missing 'in' parameter to $map"),
    ({"$replaceAll": {}}, 51749, "$replaceAll requires 'input' to be specified"),
    (
        {"$replaceAll": {"input": "a", "replacement": "b"}},
        51748,
        "$replaceAll requires 'find' to be specified",
    ),
    (
        {"$replaceAll": {"input": "a", "find": "a"}},
        51747,
        "$replaceAll requires 'replacement' to be specified",
    ),
    ({"$replaceOne": {}}, 51749, "$replaceOne requires 'input' to be specified"),
    ({"$setField": {}}, 4161102, "$setField requires 'field' to be specified"),
    (
        {"$setField": {"field": "f", "value": 1}},
        4161109,
        "$setField requires 'input' to be specified",
    ),
    (
        {"$setField": {"field": "f", "input": {}}},
        4161103,
        "$setField requires 'value' to be specified",
    ),
    ({"$sortArray": {}}, 2942502, "$sortArray requires 'input' to be specified"),
    (
        {"$sortArray": {"input": [1]}},
        2942503,
        "$sortArray requires 'sortBy' to be specified",
    ),
    ({"$dateToString": {}}, 18628, "Missing 'date' parameter to $dateToString"),
    ({"$cond": {}}, 17080, "Missing 'if' parameter to $cond"),
    ({"$cond": {"if": True, "else": 2}}, 17081, "Missing 'then' parameter to $cond"),
    ({"$cond": {"if": True, "then": 1}}, 17082, "Missing 'else' parameter to $cond"),
    ({"$let": {}}, 16876, "Missing 'vars' parameter to $let"),
    ({"$let": {"vars": {"v": 1}}}, 16877, "Missing 'in' parameter to $let"),
    ({"$switch": {}}, 40068, "$switch requires at least one branch"),
    # Present-but-empty gets the same code as absent.
    ({"$switch": {"branches": []}}, 40068, "$switch requires at least one branch"),
    ({"$zip": {}}, 34465, "$zip requires at least one input array"),
    ({"$zip": {"inputs": []}}, 34465, "$zip requires at least one input array"),
    (
        {"$dateAdd": {}},
        5166402,
        "$dateAdd requires startDate, unit, and amount to be present",
    ),
]


@pytest.mark.parametrize(
    "expr,code,message",
    MISSING_CASES,
    ids=[f"{next(iter(e))}-{i}" for i, (e, _, _) in enumerate(MISSING_CASES)],
)
def test_missing_required_argument_errors(db, expr, code, message):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _agg(db, expr)
    assert e.value.code == code
    # The stage wrapper is part of the message mongod sends.
    assert str(e.value).startswith(f"Invalid $addFields :: caused by :: {message}")


@pytest.mark.parametrize("stage", ["$addFields", "$project", "$set"])
def test_the_wrapper_names_the_stage(db, stage):
    """mongod names the stage in a parse error from a projection-style stage."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _agg(db, {"$trim": {}}, stage=stage)
    assert e.value.code == 50695
    assert str(e.value).startswith(
        f"Invalid {stage} :: caused by :: $trim requires an 'input' field"
    )


def test_the_same_error_is_bare_inside_match_expr(db):
    """Outside the projection-style stages mongod sends the message BARE, which
    is why the wrapper is applied per stage rather than inside the evaluator.

    `$group` should behave the same way and does NOT: it answers the generic
    refusal, because `group.rs` types its errors as `Result<T, ()>` and discards
    them. That is a pre-existing defect independent of this change -- it loses
    `$ln: 0`'s 28766 the same way -- and is filed in `tasks/backlog.md` rather
    than asserted here.
    """
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$match": {"$expr": {"$trim": {}}}}]))
    assert e.value.code == 50695
    assert str(e.value).startswith("$trim requires an 'input' field")


# A NULL required field is legal; only an ABSENT one is an error.
NULL_IS_FINE = [
    ({"$trim": {"input": None}}, None),
    ({"$regexMatch": {"input": None, "regex": "a"}}, False),
    ({"$filter": {"input": None, "cond": True}}, None),
    ({"$map": {"input": None, "in": "$$this"}}, None),
]


@pytest.mark.parametrize("expr,want", NULL_IS_FINE, ids=[next(iter(e)) for e, _ in NULL_IS_FINE])
def test_a_null_required_field_is_not_a_missing_one(db, expr, want):
    assert _agg(db, expr)[0]["x"] == want


def test_an_unknown_argument_outranks_a_missing_one(db):
    """mongod reports the unrecognised key even when a required one is absent."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _agg(db, {"$trim": {"k": 1}})
    assert e.value.code == 50694
    assert "found an unknown argument: k" in str(e.value)


def test_valid_forms_still_evaluate(db):
    """The guard must not fire on well-formed arguments."""
    assert _agg(db, {"$trim": {"input": "  a  "}})[0]["x"] == "a"
    assert _agg(db, {"$filter": {"input": [1, 2], "cond": True}})[0]["x"] == [1, 2]
    assert _agg(db, {"$cond": {"if": True, "then": 1, "else": 2}})[0]["x"] == 1
    assert _agg(db, {"$zip": {"inputs": [[1], [2]]}})[0]["x"] == [[1, 2]]
