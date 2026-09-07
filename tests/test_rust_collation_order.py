"""Collated ORDER on the RUST server.

Two failures, both found by sweeping `tools/probes/collation_order.py` against
the Rust server for the first time.

**A case-insensitive query or sort over any non-ASCII text ERRORED.** Not only
accents — `ß` and `日` too, on match filters as well as sorts::

    find().sort("v", 1).collation({"locale": "en", "strength": 1})
        over ["a", "A", "á", "B", "b"]
            mongod / python  ['a', 'A', 'á', 'B', 'b']
            rust             2 BadValue: an indexed value is of a type the
                             Rust server does not support

`normalize_index_bytes` returned `None` for non-ASCII, meaning "defer to the
pure engine". That is right on the Python server and wrong on the Rust one,
which has no Python behind a defer.

**Collated ordering was not implemented**, so strings sorted by codepoint and
every accented word landed after `z`. `collation.sort_levels` is now ported to
`collation::sort_level_bytes`.

The expectations below are mongod 8.2.11's own answers, generated on
2026-09-07 across every collation option shape rather than written by hand.
Several are not guessable:

* `ß` folds to `ss`, so `["ß","s","t"]` sorts `["s","ß","t"]` — `to_lowercase`
  would leave `ß` alone and put it after `t`;
* the mark order is measured, not codepoint order (acute before grave);
* `backwards` (French) compares accents from the END, which is what makes
  `cote < côte < coté` rather than `cote < coté < côte`;
* at strength 1 the accented and case variants TIE, and mongod returns them in
  input order.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

#: (collation spec, input values, mongod's order) — generated from 8.2.11.
CASES: list[tuple[dict, list[str], list[str]]] = [
    ({"locale": "en"}, ["a", "A", "á", "Á", "ä", "é"], ["a", "A", "á", "Á", "ä", "é"]),
    ({"locale": "en"}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en"}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en"},
        ["résumé", "resume", "Resume", "resumes"],
        ["resume", "Resume", "résumé", "resumes"],
    ),
    ({"locale": "en"}, ["cote", "côte", "coté", "côté"], ["cote", "coté", "côte", "côté"]),
    ({"locale": "en"}, ["a2", "a10", "a1b3", "a1b20"], ["a10", "a1b20", "a1b3", "a2"]),
    ({"locale": "en"}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    ({"locale": "en"}, ["a", "á", "ä", "az", "b"], ["a", "á", "ä", "az", "b"]),
    (
        {"locale": "en", "strength": 1},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "A", "á", "Á", "ä", "é"],
    ),
    ({"locale": "en", "strength": 1}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "strength": 1}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "strength": 1},
        ["résumé", "resume", "Resume", "resumes"],
        ["résumé", "resume", "Resume", "resumes"],
    ),
    (
        {"locale": "en", "strength": 1},
        ["cote", "côte", "coté", "côté"],
        ["cote", "côte", "coté", "côté"],
    ),
    (
        {"locale": "en", "strength": 1},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    ({"locale": "en", "strength": 1}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    ({"locale": "en", "strength": 1}, ["a", "á", "ä", "az", "b"], ["a", "á", "ä", "az", "b"]),
    (
        {"locale": "en", "strength": 2},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "A", "á", "Á", "ä", "é"],
    ),
    ({"locale": "en", "strength": 2}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "strength": 2}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "strength": 2},
        ["résumé", "resume", "Resume", "resumes"],
        ["resume", "Resume", "résumé", "resumes"],
    ),
    (
        {"locale": "en", "strength": 2},
        ["cote", "côte", "coté", "côté"],
        ["cote", "coté", "côte", "côté"],
    ),
    (
        {"locale": "en", "strength": 2},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    ({"locale": "en", "strength": 2}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    ({"locale": "en", "strength": 2}, ["a", "á", "ä", "az", "b"], ["a", "á", "ä", "az", "b"]),
    (
        {"locale": "en", "strength": 3},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "A", "á", "Á", "ä", "é"],
    ),
    ({"locale": "en", "strength": 3}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "strength": 3}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "strength": 3},
        ["résumé", "resume", "Resume", "resumes"],
        ["resume", "Resume", "résumé", "resumes"],
    ),
    (
        {"locale": "en", "strength": 3},
        ["cote", "côte", "coté", "côté"],
        ["cote", "coté", "côte", "côté"],
    ),
    (
        {"locale": "en", "strength": 3},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    ({"locale": "en", "strength": 3}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    ({"locale": "en", "strength": 3}, ["a", "á", "ä", "az", "b"], ["a", "á", "ä", "az", "b"]),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["a", "A", "á", "Á", "ä", "é"],
        ["A", "a", "Á", "á", "ä", "é"],
    ),
    ({"locale": "en", "strength": 3, "caseFirst": "upper"}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "strength": 3, "caseFirst": "upper"}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["résumé", "resume", "Resume", "resumes"],
        ["Resume", "resume", "résumé", "resumes"],
    ),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["cote", "côte", "coté", "côté"],
        ["cote", "coté", "côte", "côté"],
    ),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["", "a", "ab", "A"],
        ["", "A", "a", "ab"],
    ),
    (
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
        ["a", "á", "ä", "az", "b"],
        ["a", "á", "ä", "az", "b"],
    ),
    (
        {"locale": "fr", "backwards": True},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "A", "á", "Á", "ä", "é"],
    ),
    ({"locale": "fr", "backwards": True}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "fr", "backwards": True}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "fr", "backwards": True},
        ["résumé", "resume", "Resume", "resumes"],
        ["resume", "Resume", "résumé", "resumes"],
    ),
    (
        {"locale": "fr", "backwards": True},
        ["cote", "côte", "coté", "côté"],
        ["cote", "côte", "coté", "côté"],
    ),
    (
        {"locale": "fr", "backwards": True},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    ({"locale": "fr", "backwards": True}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    ({"locale": "fr", "backwards": True}, ["a", "á", "ä", "az", "b"], ["a", "á", "ä", "az", "b"]),
    (
        {"locale": "en", "numericOrdering": True},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "A", "á", "Á", "ä", "é"],
    ),
    ({"locale": "en", "numericOrdering": True}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "numericOrdering": True}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "numericOrdering": True},
        ["résumé", "resume", "Resume", "resumes"],
        ["resume", "Resume", "résumé", "resumes"],
    ),
    (
        {"locale": "en", "numericOrdering": True},
        ["cote", "côte", "coté", "côté"],
        ["cote", "coté", "côte", "côté"],
    ),
    (
        {"locale": "en", "numericOrdering": True},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a1b3", "a1b20", "a2", "a10"],
    ),
    ({"locale": "en", "numericOrdering": True}, ["", "a", "ab", "A"], ["", "a", "A", "ab"]),
    (
        {"locale": "en", "numericOrdering": True},
        ["a", "á", "ä", "az", "b"],
        ["a", "á", "ä", "az", "b"],
    ),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["a", "A", "á", "Á", "ä", "é"],
        ["a", "á", "ä", "A", "Á", "é"],
    ),
    ({"locale": "en", "strength": 1, "caseLevel": True}, ["ß", "s", "t"], ["s", "ß", "t"]),
    ({"locale": "en", "strength": 1, "caseLevel": True}, ["日", "a", "z"], ["a", "z", "日"]),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["résumé", "resume", "Resume", "resumes"],
        ["résumé", "resume", "Resume", "resumes"],
    ),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["cote", "côte", "coté", "côté"],
        ["cote", "côte", "coté", "côté"],
    ),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["a2", "a10", "a1b3", "a1b20"],
        ["a10", "a1b20", "a1b3", "a2"],
    ),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["", "a", "ab", "A"],
        ["", "a", "A", "ab"],
    ),
    (
        {"locale": "en", "strength": 1, "caseLevel": True},
        ["a", "á", "ä", "az", "b"],
        ["a", "á", "ä", "az", "b"],
    ),
]


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(
        str(tmp_path_factory.mktemp("rs_collation") / "wt"), 0, replica_set_name=None
    )
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["collorder"]
    d.c.drop()
    try:
        yield d
    finally:
        cli.close()


def _ids() -> list[str]:
    return [
        f"{spec.get('locale')}-s{spec.get('strength', 3)}"
        f"{'-cf' if 'caseFirst' in spec else ''}"
        f"{'-bw' if spec.get('backwards') else ''}"
        f"{'-num' if spec.get('numericOrdering') else ''}-{i}"
        for i, (spec, _, _) in enumerate(CASES)
    ]


@pytest.mark.parametrize("spec,values,expected", CASES, ids=_ids())
def test_collated_sort_matches_mongod(db, spec: dict, values: list, expected: list) -> None:
    db.c.insert_many([{"_id": i, "v": v} for i, v in enumerate(values)])
    assert [d["v"] for d in db.c.find().sort("v", 1).collation(spec)] == expected


@pytest.mark.parametrize("spec,values,expected", CASES, ids=_ids())
def test_an_index_changes_speed_never_results(db, spec: dict, values: list, expected: list) -> None:
    """The probe's own invariant. A collated index must not change the answer."""
    db.c.insert_many([{"_id": i, "v": v} for i, v in enumerate(values)])
    db.c.create_index([("v", 1)], name="v_1", collation=spec)
    assert [d["v"] for d in db.c.find().sort("v", 1).collation(spec)] == expected


# `find({v: <value>}).collation({locale: "en", strength: N})` over the two docs
# `{v: <value>}` and `{v: "a"}` — measured against mongod 8.2.11 on 2026-09-07.
_MATCH_COUNT = {
    ("\u00e1", 1): 2,
    ("\u00e1", 2): 1,
    ("\u00e4", 1): 2,
    ("\u00e4", 2): 1,
    ("\u00e9", 1): 1,
    ("\u00e9", 2): 1,
    ("\u00f1", 1): 1,
    ("\u00f1", 2): 1,
    ("\u00df", 1): 1,
    ("\u00df", 2): 1,
    ("\u65e5", 1): 1,
    ("\u65e5", 2): 1,
    ("\u00c5", 1): 2,
    ("\u00c5", 2): 1,
}


@pytest.mark.parametrize("value", ["á", "ä", "é", "ñ", "ß", "日", "Å"])
@pytest.mark.parametrize("strength", [1, 2])
def test_non_ascii_no_longer_errors(db, value: str, strength: int) -> None:
    """Every one of these used to answer `2 BadValue: an indexed value is of a
    type the Rust server does not support` — a hard failure on ordinary input."""
    db.c.insert_many([{"_id": 0, "v": value}, {"_id": 1, "v": "a"}])
    spec = {"locale": "en", "strength": strength}
    assert len(list(db.c.find().sort("v", 1).collation(spec))) == 2
    # A match filter took the same path and failed the same way. The expected
    # count is MEASURED against mongod 8.2.11, not assumed: at strength 1 the
    # accent fold makes `á` / `ä` / `Å` equal to the second seeded doc `"a"`, so
    # the right answer there is 2, not 1. An earlier version of this test
    # hard-coded 1 and failed against a server that was behaving correctly.
    assert len(list(db.c.find({"v": value}).collation(spec))) == _MATCH_COUNT[(value, strength)]


def test_sharp_s_folds_to_ss_not_lowercase(db) -> None:
    """The case that makes `to_lowercase` insufficient: folding maps `ß` to
    `ss`, so it sorts BETWEEN `s` and `t`, not after `t`."""
    db.c.insert_many([{"_id": i, "v": v} for i, v in enumerate(["ß", "s", "t"])])
    got = [d["v"] for d in db.c.find().sort("v", 1).collation({"locale": "en", "strength": 2})]
    assert got == ["s", "ß", "t"]
