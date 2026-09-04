"""Projected field ORDER, measured against mongod 8.2.11 (2026-09-04).

mongod emits projected fields in the **stored document's** order and ignores
the projection spec's order entirely. Both engines got this wrong, differently:
the pure engine emitted SPEC order, and the Rust engine emitted ALPHABETICAL
order (its spec trie is a `BTreeMap`).

Neither was visible to the parity suite, because `==` on a dict ignores key
order -- and the alphabetical one was invisible even to a probe, for as long as
the corpus documents happened to be keyed alphabetically. Every document below
is deliberately NOT in alphabetical order, which is the only thing that
separates "document order" from "sorted".

Field order is behaviour: a driver renders it and a wire-level test can assert
it. These expectations are measured mongod output, not derived.
"""

from __future__ import annotations

import pytest
from bson import SON

from secantus.projection import apply_projection

# Keyed z, a, m on purpose -- alphabetical order would be a, m, z.
UNSORTED = SON([("_id", 1), ("z", 9), ("a", 2), ("m", 5)])
NESTED = SON([("_id", 1), ("b", SON([("y", 1), ("x", 2)])), ("a", 5)])


@pytest.mark.parametrize(
    ("doc", "spec", "expected_keys"),
    [
        # Spec order is irrelevant: these two answer the same thing.
        (UNSORTED, {"z": 1, "a": 1, "m": 1}, ["_id", "z", "a", "m"]),
        (UNSORTED, {"m": 1, "a": 1, "z": 1}, ["_id", "z", "a", "m"]),
        (UNSORTED, {"_id": 0, "m": 1, "a": 1}, ["a", "m"]),
        (UNSORTED, {"_id": 0, "a": 1, "m": 1}, ["a", "m"]),
        # A dotted path does not move its parent: `b` keeps the document's slot.
        (NESTED, {"b.x": 1, "a": 1}, ["_id", "b", "a"]),
        (NESTED, {"a": 1, "b.x": 1}, ["_id", "b", "a"]),
    ],
)
def test_projection_emits_document_order(doc, spec, expected_keys):
    assert list(apply_projection(dict(doc), spec)) == expected_keys


def test_a_subdocument_keeps_its_own_order():
    """The rule recurses: `b`'s fields follow `b`'s order, not the spec's."""
    got = apply_projection(dict(NESTED), {"b.x": 1, "b.y": 1})
    assert list(got) == ["_id", "b"]
    assert list(got["b"]) == ["y", "x"]


def test_alphabetical_is_not_document_order():
    """The regression guard proper.

    Both wrong answers -- spec order and alphabetical order -- are excluded
    here, and only for a document whose keys are not already sorted. A corpus
    of alphabetically-keyed documents cannot tell the three apart, which is how
    the Rust engine's version of this survived.
    """
    got = list(apply_projection(dict(UNSORTED), {"m": 1, "z": 1, "a": 1}))
    assert got == ["_id", "z", "a", "m"], got
    assert got != ["_id", "a", "m", "z"], "alphabetical order -- the Rust bug"
    assert got != ["_id", "m", "z", "a"], "spec order -- the pure-engine bug"
