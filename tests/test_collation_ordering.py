"""Collated ORDER, and the rule that an index must not change it.

Collation shipped for MATCHING only: ``_normalize_string`` folds a string to one
value, and anything the fold does not distinguish fell back to comparing whole
codepoints. That put every accented word after ``z``, dropped tertiary case
order, and made ``caseFirst`` / ``backwards`` / ``numericOrdering`` accepted and
ignored -- and, worse, made a collated ``sort`` return a DIFFERENT order
depending on whether a collated index existed, because the index walk and the
Python sort disagreed.

Expectations are mongod 8.2.11's own output, measured 2026-09-01 with
``tools/probes/collation_order.py``.

Still open and deliberately not asserted here: LOCALE-specific ordering. Swedish
puts ``ä`` after ``z`` and Danish puts ``å`` last; that is CLDR data, not
something decomposition can derive, and it needs an ICU dependency.
"""

from __future__ import annotations

import pytest

from secantus.collation import parse, sort_levels
from secantus.ordering import sort_docs


def _order(values, spec):
    docs = [{"_id": i, "v": v} for i, v in enumerate(values)]
    return [d["v"] for d in sort_docs(docs, {"v": 1}, collation=parse(spec))]


@pytest.mark.parametrize(
    ("values", "spec", "expected"),
    [
        # Accents sort BESIDE their base letter, not after `z`.
        (["a", "á", "ä", "az", "b"], {"locale": "en"}, ["a", "á", "ä", "az", "b"]),
        (["ä", "az", "b"], {"locale": "de"}, ["ä", "az", "b"]),
        # Tertiary case order: lowercase first by default.
        (["a", "A", "b", "B"], {"locale": "en", "strength": 3}, ["a", "A", "b", "B"]),
        (
            ["a", "A", "b", "B"],
            {"locale": "en", "strength": 3, "caseFirst": "upper"},
            ["A", "a", "B", "b"],
        ),
        # numericOrdering was implemented for matching and never reached a sort.
        (
            ["10", "9", "2", "100"],
            {"locale": "en", "numericOrdering": True},
            ["2", "9", "10", "100"],
        ),
        (
            ["a2", "a10", "a1b3", "a1b20"],
            {"locale": "en", "numericOrdering": True},
            ["a1b3", "a1b20", "a2", "a10"],
        ),
        # ... and stays off when not asked for.
        (["10", "9", "2", "100"], {"locale": "en"}, ["10", "100", "2", "9"]),
        # French `backwards` compares accents from the END of the word.
        (
            ["cote", "côte", "coté", "côté"],
            {"locale": "fr", "backwards": True},
            ["cote", "côte", "coté", "côté"],
        ),
        (
            ["cote", "côte", "coté", "côté"],
            {"locale": "fr"},
            ["cote", "coté", "côte", "côté"],
        ),
        # Strength truncates the key: at 1 everything ties and the input order
        # survives (a stable sort), at 2 the accent separates but the case does
        # not.
        (["a", "A", "á", "B", "b"], {"locale": "en", "strength": 1}, ["a", "A", "á", "B", "b"]),
        (["a", "A", "á", "B", "b"], {"locale": "en", "strength": 2}, ["a", "A", "á", "B", "b"]),
        (["a", "A", "b"], {"locale": "en", "strength": 1, "caseLevel": True}, ["a", "A", "b"]),
        # ICU's mark order is NOT codepoint order: acute (U+0301) before grave
        # (U+0300).
        (
            ["a", "à", "á", "â", "ã", "ä"],
            {"locale": "en"},
            ["a", "á", "à", "â", "ä", "ã"],
        ),
    ],
)
def test_collated_order_matches_mongod(values, spec, expected):
    assert _order(values, spec) == expected


def test_no_collation_is_untouched():
    """A sort without a collation must stay on codepoint order."""
    docs = [{"_id": i, "v": v} for i, v in enumerate(["b", "A", "a", "B"])]
    assert [d["v"] for d in sort_docs(docs, {"v": 1})] == ["A", "B", "a", "b"]


def test_non_string_values_ignore_the_collation():
    docs = [{"_id": i, "v": v} for i, v in enumerate([3, 1, 2])]
    assert [d["v"] for d in sort_docs(docs, {"v": 1}, collation=parse({"locale": "en"}))] == [
        1,
        2,
        3,
    ]


def test_sort_levels_truncates_by_strength():
    en3 = parse({"locale": "en", "strength": 3})
    en2 = parse({"locale": "en", "strength": 2})
    en1 = parse({"locale": "en", "strength": 1})
    assert len(sort_levels("a", en1)) == 1
    assert len(sort_levels("a", en2)) == 2
    assert len(sort_levels("a", en3)) == 3
    # Equal at strength 1, distinguished at 2 and 3.
    assert sort_levels("a", en1) == sort_levels("A", en1) == sort_levels("á", en1)
    assert sort_levels("a", en2) == sort_levels("A", en2)
    assert sort_levels("a", en2) != sort_levels("á", en2)
    assert sort_levels("a", en3) != sort_levels("A", en3)


def test_an_index_does_not_change_the_collated_order(tmp_path):
    """The bug this file exists for: same query, same data, different ORDER
    with and without a collated index."""
    from secantus.storage import Storage

    spec = {"locale": "en"}
    values = ["a", "á", "ä", "az", "b"]
    without = Storage(str(tmp_path / "a"))
    withidx = Storage(str(tmp_path / "b"))
    try:
        for store in (without, withidx):
            store.insert("db", "c", [{"_id": i, "v": v} for i, v in enumerate(values)])
        withidx.create_index("db", "c", "v_c", {"v": 1}, {"collation": spec})
        a = [d["v"] for d in without.find_matching("db", "c", {}, sort={"v": 1}, collation=spec)]
        b = [d["v"] for d in withidx.find_matching("db", "c", {}, sort={"v": 1}, collation=spec)]
        assert a == b == ["a", "á", "ä", "az", "b"]
    finally:
        without.close()
        withidx.close()
