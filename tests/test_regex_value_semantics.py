"""A regex is a VALUE, not only a pattern.

Every case here is pinned against mongod 8.2.11 (2026-09-01). Three distinct
bugs lived in this corner, all of them silent wrong answers rather than errors:

* a bare `/ab/i` filter matched strings by pattern but never matched a stored
  regex EQUAL to it, so `find({v: /ab/i})` missed `{v: /ab/i}`;
* `bson.Code` subclasses `str`, so a JavaScript value was pattern-matched as
  though it were text -- mongod never applies a regex to code;
* `bson.Regex` defines no `__lt__`, so two regexes compared EQUAL and `$max`
  over regexes never moved.
"""

import pytest
from bson import Code, Regex

from secantus.ordering import sort_docs
from secantus.query import matches


class TestRegexEqualsRegex:
    """mongod matches a query regex against a stored regex by equality."""

    @pytest.mark.parametrize(
        ("doc_value", "query", "expected"),
        [
            (Regex("ab", "i"), Regex("ab", "i"), True),
            (Regex("ab", ""), Regex("ab", ""), True),
            # Options compare as a SET: `/ab/im` is `/ab/mi`.
            (Regex("ab", "im"), Regex("ab", "mi"), True),
            # ... but not as a subset.
            (Regex("ab", "i"), Regex("ab", "mi"), False),
            (Regex("ab", "i"), Regex("ab", ""), False),
            # The pattern is exact -- no substring, no case folding.
            (Regex("abc", ""), Regex("ab", ""), False),
            (Regex("AB", ""), Regex("ab", ""), False),
        ],
    )
    def test_bare_regex_matches_a_stored_regex(self, doc_value, query, expected):
        assert matches({"v": doc_value}, {"v": query}) is expected

    def test_a_string_still_matches_by_pattern(self):
        assert matches({"v": "xxabxx"}, {"v": Regex("ab", "")}) is True

    def test_through_an_array(self):
        assert matches({"v": [Regex("ab", "i")]}, {"v": Regex("ab", "i")}) is True

    @pytest.mark.parametrize("op", ["$in", "$regex"])
    def test_in_and_regex_carry_the_same_rule(self, op):
        arg = [Regex("ab", "i")] if op == "$in" else Regex("ab", "i")
        assert matches({"v": Regex("ab", "i")}, {"v": {op: arg}}) is True

    def test_nin_is_the_negation(self):
        assert matches({"v": Regex("ab", "i")}, {"v": {"$nin": [Regex("ab", "i")]}}) is False

    def test_regex_with_separate_options_participates(self):
        """`{$regex: "ab", $options: "i"}` equals a stored `/ab/i` ..."""
        assert matches({"v": Regex("ab", "i")}, {"v": {"$regex": "ab", "$options": "i"}}) is True

    def test_but_bare_regex_string_does_not_reach_an_optioned_stored_regex(self):
        """... while `{$regex: "ab"}` alone does not -- set equality, not subset."""
        assert matches({"v": Regex("ab", "i")}, {"v": {"$regex": "ab"}}) is False


class TestEqIsEqualityNotMatching:
    """`$eq` with a regex operand is equality ONLY -- the opposite of a bare one."""

    def test_eq_matches_the_stored_regex(self):
        assert matches({"v": Regex("ab", "i")}, {"v": {"$eq": Regex("ab", "i")}}) is True

    def test_eq_does_not_pattern_match_a_string(self):
        assert matches({"v": "ab"}, {"v": {"$eq": Regex("ab", "i")}}) is False

    def test_whereas_a_bare_regex_does(self):
        assert matches({"v": "ab"}, {"v": Regex("ab", "i")}) is True


class TestCodeIsNotText:
    """`bson.Code` subclasses `str`; mongod never regex-matches JavaScript."""

    @pytest.mark.parametrize(
        "query",
        [Regex("ab", ""), {"$regex": "ab"}, {"$in": [Regex("ab", "")]}],
    )
    def test_a_regex_never_matches_a_code_value(self, query):
        assert matches({"v": Code("ab")}, {"v": query}) is False

    def test_nin_therefore_keeps_it(self):
        assert matches({"v": Code("ab")}, {"v": {"$nin": [Regex("ab", "")]}}) is True


class TestRegexOrdering:
    """Pattern first, then the option string."""

    def test_sorts_in_mongods_order(self):
        corpus = [
            Regex("b", ""),
            Regex("a", "m"),
            Regex("a", ""),
            Regex("ab", ""),
            Regex("a", "im"),
            Regex("A", ""),
            Regex("a", "i"),
            Regex("", ""),
        ]
        docs = [{"_id": i, "v": v} for i, v in enumerate(corpus)]
        got = [(d["v"].pattern, d["v"].flags) for d in sort_docs(docs, {"v": 1})]
        assert got == [
            ("", 0),
            ("A", 0),
            ("a", 0),
            ("a", 2),  # /a/i
            ("a", 10),  # /a/im
            ("a", 8),  # /a/m
            ("ab", 0),
            ("b", 0),
        ]

    def test_two_regexes_are_not_all_equal(self):
        """The bug: every pair reported equal, so a sort was a no-op."""
        docs = [{"_id": 0, "v": Regex("z", "")}, {"_id": 1, "v": Regex("a", "")}]
        assert [d["_id"] for d in sort_docs(docs, {"v": 1})] == [1, 0]


class TestIndexedAndUnindexedAgree:
    """The in-memory sort and the persisted index entries must order alike.

    `sortkey._regex_options` writes the option string into the index-entry
    bytes; `ordering._regex_sort_key` drives the in-memory sort. The index
    encoder had this right all along and the in-memory sort reported every pair
    EQUAL, so the two disagreed -- the same failure shape as the JavaScript rank
    (which needed an `entryFormat` bump). Here no format change is needed,
    because it is the in-memory half that moved onto what the index already did.
    """

    CORPUS = [
        Regex("b", ""),
        Regex("a", "m"),
        Regex("a", ""),
        Regex("ab", ""),
        Regex("a", "mi"),
        Regex("A", ""),
        Regex("a", "i"),
        Regex("", ""),
    ]

    def test_the_two_encoders_render_options_alike(self):
        from secantus.ordering import _regex_sort_key
        from secantus.sortkey import _regex_options

        for r in self.CORPUS:
            assert _regex_options(r.flags).decode() == _regex_sort_key(r)[1]

    def test_index_bytes_sort_in_the_same_order_as_the_documents(self):
        from secantus.ordering import sort_docs
        from secantus.sortkey import encode_value

        docs = [{"_id": i, "v": v} for i, v in enumerate(self.CORPUS)]
        by_sort = [d["_id"] for d in sort_docs(docs, {"v": 1})]
        by_index = [i for _, i in sorted((encode_value(d["v"]), d["_id"]) for d in docs)]
        assert by_sort == by_index
