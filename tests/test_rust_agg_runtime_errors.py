"""Runtime aggregation errors on the RUST server, pinned to mongod's codes.

"Runtime" means discoverable only while processing documents, so no spec-level
check at the command layer can reach them -- which is what makes them the half
of the error-code class that survived `tasks/remaining-work-plan.md`'s Phase 1b
spec-level work.

Measured against mongod 8.2.11 on 2026-08-31: the Rust server answered
``2 aggregation pipeline uses a stage or operator not supported by the Rust
server`` for **six of seven** probed cases, because the engine can only signal
these as `Fallback` and the server has no Python to fall back to. The three
pinned here are the ones whose message is a CONSTANT string, so naming them
needs no new machinery -- `secantus_core::aggregate::runtime_error` re-checks
the specific condition and reports mongod's code.

Two are deliberately NOT covered, and are recorded in `tasks/backlog.md`:

* ``$replaceRoot`` with a scalar ``newRoot`` (40228), whose message quotes the
  input document pruned to the fields the expression reads -- mongod runs
  dependency analysis first, and the Python server ports that in
  ``_input_document``. Naming it in Rust means porting that too.
* the expression type errors (``$arrayToObject`` 40386, ``$concatArrays``
  28664, ...), which are the head of a per-operator long tail.

The Python server already matches mongod on all seven (verified the same day),
so this file is Rust-only.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

NO_MATCHING_BRANCH = (
    "$switch could not find a matching branch for an input, and no default was specified."
)


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_aggrt") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["aggruntime"]
    try:
        yield d
    finally:
        cli.close()


def _agg_error(db, coll, docs, pipeline):
    db[coll].drop()
    db[coll].insert_many([dict(d) for d in docs])
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        list(db[coll].aggregate(pipeline))
    return exc.value


def test_densify_on_a_non_numeric_field(db) -> None:
    err = _agg_error(
        db,
        "densify_c",
        [{"_id": 1, "s": "x"}],
        [{"$densify": {"field": "s", "range": {"step": 1, "bounds": "full"}}}],
    )
    assert err.code == 5733201
    assert err.details["errmsg"].endswith("Densify field type must be numeric or a date")
    # The executor wrapper is mongod's, and applies to every runtime error.
    assert "Executor error during aggregate command on namespace:" in err.details["errmsg"]


def test_bucket_with_no_default_and_a_value_out_of_range(db) -> None:
    """mongod implements ``$bucket`` over ``$switch``, so it reports the
    ``$switch`` sentence under ``$bucket``'s own code."""
    err = _agg_error(
        db,
        "bucket_c",
        [{"_id": 1, "n": 99}],
        [{"$bucket": {"groupBy": "$n", "boundaries": [0, 10]}}],
    )
    assert err.code == 7158303
    assert err.details["errmsg"].endswith(NO_MATCHING_BRANCH)


def test_bucket_with_a_default_absorbs_the_out_of_range_value(db) -> None:
    db["bucket_ok"].drop()
    db["bucket_ok"].insert_many([{"_id": 1, "n": 99}])
    out = list(
        db["bucket_ok"].aggregate(
            [{"$bucket": {"groupBy": "$n", "boundaries": [0, 10], "default": "other"}}]
        )
    )
    assert out == [{"_id": "other", "count": 1}]


def test_switch_with_no_matching_branch_and_no_default(db) -> None:
    err = _agg_error(
        db,
        "switch_c",
        [{"_id": 1, "n": 1}],
        [
            {
                "$project": {
                    "v": {"$switch": {"branches": [{"case": {"$eq": ["$n", 99]}, "then": 1}]}}
                }
            }
        ],
    )
    assert err.code == 40066
    assert err.details["errmsg"].endswith(NO_MATCHING_BRANCH)


def test_switch_with_a_default_is_not_an_error(db) -> None:
    db["switch_ok"].drop()
    db["switch_ok"].insert_many([{"_id": 1, "n": 1}])
    out = list(
        db["switch_ok"].aggregate(
            [
                {
                    "$project": {
                        "v": {
                            "$switch": {
                                "branches": [{"case": {"$eq": ["$n", 99]}, "then": 1}],
                                "default": 0,
                            }
                        }
                    }
                }
            ]
        )
    )
    assert out == [{"_id": 1, "v": 0}]
