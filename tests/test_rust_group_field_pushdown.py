"""Parity: the `$group` field-reference pushdown (`referenced_top_level_fields`).

Phase 6a lets the command layer decode only the top-level fields a `$group`
reads from each (often wide) input document, instead of materializing the whole
document, ahead of the stage. The pushdown is sound iff:

    when `group_referenced_fields(spec)` returns a field set `F`, running the
    group over documents reduced to `F` yields *exactly* what running it over
    the full documents yields — a concrete result or a deferral, either way.

This suite pins that invariant against the **real Rust group engine** (via
`_secantus_core.apply_pipeline`, the same engine the command layer runs): for a
curated + fuzz corpus it asserts `apply_pipeline(minimal) == apply_pipeline(full)`
whenever the collector accepts the spec. It also pins the bail set (specs that
must return `None`) and the exact field sets for representative shapes, so an
over-eager future collector — one that claims a spec it shouldn't — is caught.
"""

from __future__ import annotations

import datetime as _dt
import random

import bson
import pytest

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _referenced_fields(spec):
    """The collector's field set for a `$group` spec, or None to full-decode."""
    res = _rust.group_referenced_fields(bson.encode(spec))
    return None if res is None else set(res)


def _rust_group(docs, spec):
    res = _rust.apply_pipeline(
        bson.encode({"d": list(docs)}),
        bson.encode({"p": [{"$group": spec}]}),
        bson.encode({}),
        bson.encode({}),
    )
    return None if res is None else bson.decode(res)["d"]


def _minimal(docs, fields):
    return [{k: v for k, v in d.items() if k in fields} for d in docs]


def _assert_pushdown(docs, spec):
    """Core invariant: minimal-decoded input == full input, for the same engine."""
    fields = _referenced_fields(spec)
    if fields is None:
        return  # collector deferred; the command layer full-decodes — nothing to prove
    full = _rust_group(docs, spec)
    minimal = _rust_group(_minimal(docs, fields), spec)
    assert minimal == full, (
        f"pushdown diverged for spec={spec} fields={sorted(fields)}: "
        f"minimal={minimal!r} full={full!r}"
    )


# ---------------------------------------------------------------------------
# Exact field-set assertions — pin the collector's decisions directly.
# ---------------------------------------------------------------------------

_DATE = _dt.datetime(2020, 1, 2, 3, 4, 5)


@pytest.mark.parametrize(
    "spec,expected",
    [
        # simple field-path _id + simple accumulators
        ({"_id": "$k", "n": {"$sum": "$v"}}, {"k", "v"}),
        ({"_id": "$k", "n": {"$sum": 1}}, {"k"}),
        ({"_id": "$k"}, {"k"}),  # distinct
        ({"_id": "$a.b.c"}, {"a"}),  # dotted path -> top-level component only
        ({"_id": {"$year": "$date"}}, {"date"}),
        ({"_id": {"a": "$x", "b": "$y"}}, {"x", "y"}),  # compound _id
        ({"_id": None, "n": {"$avg": "$score"}}, {"score"}),
        (
            {"_id": "$g", "lo": {"$min": "$v"}, "hi": {"$max": "$v"}, "all": {"$push": "$w"}},
            {"g", "v", "w"},
        ),
        # accumulator arg is a nested expression whose leaves are $paths
        ({"_id": "$g", "t": {"$sum": {"$multiply": ["$price", "$qty"]}}}, {"g", "price", "qty"}),
        # $literal argument is opaque data, never a field path
        ({"_id": {"$literal": "$notAField"}, "n": {"$sum": 1}}, set()),
        # local vars ($map/$let) resolve to elements/bindings, not top-level fields;
        # the array/binding they range over is still collected
        ({"_id": {"$map": {"input": "$items", "as": "i", "in": "$$i.p"}}}, {"items"}),
        ({"_id": {"$let": {"vars": {"w": "$a"}, "in": {"$add": ["$$w", "$b"]}}}}, {"a", "b"}),
    ],
)
def test_referenced_fields_exact(spec, expected):
    assert _referenced_fields(spec) == expected


@pytest.mark.parametrize(
    "spec",
    [
        {"_id": "$$ROOT"},  # whole-document reference
        {"_id": "$$CURRENT"},
        {"_id": "$k", "doc": {"$mergeObjects": "$$ROOT"}},  # whole doc in an accumulator
        {"_id": {"$getField": "f"}},  # implicit-CURRENT computed field access
        {"_id": {"$getField": {"field": "f", "input": "$$CURRENT"}}},
        {"_id": "$k", "best": {"$top": {"output": "$v", "sortBy": {"s": 1}, "n": 1}}},
        {"_id": "$k", "best": {"$topN": {"output": "$v", "sortBy": {"s": 1}, "n": 2}}},
        {"_id": "$k", "worst": {"$bottom": {"output": "$v", "sortBy": {"s": 1}, "n": 1}}},
        {"_id": "$k", "ns": {"$firstN": {"input": "$v", "n": 2}}},  # not whitelisted -> bail
        {"n": {"$sum": 1}},  # no _id -> group errors anyway; collector must not claim it
        {"_id": "$k", "bad": {"$sum": "$a", "extra": 1}},  # malformed accumulator (2 keys)
        {"_id": "$k", "bad": 5},  # accumulator not a doc
    ],
)
def test_referenced_fields_bail(spec):
    assert _referenced_fields(spec) is None


# ---------------------------------------------------------------------------
# Pushdown invariant over a curated corpus of documents × specs.
# ---------------------------------------------------------------------------

_WIDE = [
    {"_id": 1, "k": "a", "v": 5, "w": 1.5, "s": 3, "extra1": "zzz", "extra2": [1, 2, 3]},
    {"_id": 2, "k": "b", "v": 15, "w": 2.0, "s": 1, "extra1": "yyy", "extra2": {"deep": 9}},
    {"_id": 3, "k": "a", "v": 25, "w": 3.5, "s": 2, "extra1": "xxx"},
    {"_id": 4, "k": "b", "v": None, "w": 4.0, "s": 4, "extra1": "www", "extra2": None},
    {"_id": 5, "k": "a", "price": 10, "qty": 2, "date": _DATE, "items": [{"p": 1}, {"p": 2}]},
    {"_id": 6, "date": _DATE, "items": [{"p": 3}]},  # several fields missing
]

_CURATED_SPECS = [
    {"_id": "$k", "n": {"$sum": "$v"}},
    {"_id": "$k", "n": {"$sum": 1}, "avg": {"$avg": "$v"}},
    {"_id": "$k", "lo": {"$min": "$v"}, "hi": {"$max": "$v"}},
    {"_id": "$k", "first": {"$first": "$v"}, "last": {"$last": "$w"}},
    {"_id": "$k", "vals": {"$push": "$v"}, "set": {"$addToSet": "$w"}},
    {"_id": "$k"},
    {"_id": {"$year": "$date"}, "n": {"$sum": 1}},
    {"_id": {"a": "$k", "b": "$s"}, "n": {"$sum": "$v"}},
    {"_id": "$k", "t": {"$sum": {"$multiply": ["$price", "$qty"]}}},
    {"_id": None, "total": {"$sum": "$v"}, "sd": {"$stdDevPop": "$w"}},
]


@pytest.mark.parametrize("spec", _CURATED_SPECS)
def test_pushdown_curated(spec):
    _assert_pushdown(_WIDE, spec)


# ---------------------------------------------------------------------------
# Fuzz: random wide documents × random simple-accumulator specs.
# ---------------------------------------------------------------------------

_FIELDS = ["k", "g", "v", "w", "s", "p", "q"]
_SIMPLE_ACCS = ["$sum", "$avg", "$min", "$max", "$first", "$last", "$push", "$addToSet"]


def _rand_value(rng):
    return rng.choice(
        [
            rng.randint(-50, 50),
            round(rng.uniform(-50, 50), 3),
            rng.choice(["a", "b", "c", "dd"]),
            None,
            [rng.randint(0, 5), rng.randint(0, 5)],
        ]
    )


def _rand_doc(rng, i):
    d = {"_id": i}
    # a wide doc: many fields present, so the pushdown has untouched fields to drop
    for f in _FIELDS:
        if rng.random() < 0.75:
            d[f] = _rand_value(rng)
    d["noise"] = {"big": "x" * 20, "arr": list(range(rng.randint(0, 4)))}
    return d


def _rand_spec(rng):
    id_field = rng.choice(_FIELDS)
    id_expr = rng.choice([f"${id_field}", None, {"a": f"${rng.choice(_FIELDS)}"}])
    spec = {"_id": id_expr}
    for j in range(rng.randint(0, 3)):
        op = rng.choice(_SIMPLE_ACCS)
        arg = rng.choice([f"${rng.choice(_FIELDS)}", 1, rng.randint(1, 3)])
        spec[f"acc{j}"] = {op: arg}
    return spec


@pytest.mark.parametrize("seed", range(60))
def test_pushdown_fuzz(seed):
    rng = random.Random(seed)
    docs = [_rand_doc(rng, i) for i in range(rng.randint(1, 8))]
    spec = _rand_spec(rng)
    _assert_pushdown(docs, spec)
