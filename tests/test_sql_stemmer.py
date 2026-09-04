"""The English (Porter2) stemmer, pinned against PostgreSQL's own.

`secantus.sql.snowball` is written out rather than taken from a dependency —
SecantusDB ships self-contained wheels, and a stemmer is a closed, fully
specified algorithm. That trade is only defensible if the behaviour is
*measured*, so this file runs it against **6,094 words stemmed by PostgreSQL
14.13's `ts_lexize('english_stem', …)`** and requires an exact match on every
one. The corpus is `tests/data/english_stems.txt`, generated (not hand-written)
from a random sample of the system dictionary plus a hand-picked set covering
the rules most easily got wrong.

Two of those rules were in fact got wrong first time, and both are pinned
below:

* **step 1a exempts `us` as well as `ss`.** Without that, `argus` stemmed to
  `argu`, and — worse — `apodous` lost its `s` here, so step 4's `ous` no
  longer matched and it stemmed to `apodou` rather than `apod`. That single
  omission accounted for 248 of the 6,094 words.
* **step 4 takes the LONGEST matching suffix and applies its condition once.**
  Falling through to a shorter suffix when the R2 test fails is a different
  algorithm: `complement` matched `ement` (correctly rejected), then matched
  `ent` (accepted) and stemmed to `complem`, where PostgreSQL leaves it alone.
"""

from __future__ import annotations

import pathlib

import pytest

from secantus.sql import snowball

_CORPUS = pathlib.Path(__file__).parent / "data" / "english_stems.txt"


def _pairs() -> list[tuple[str, str]]:
    out = []
    for line in _CORPUS.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        word, _, stem = line.partition("\t")
        if stem:
            out.append((word, stem))
    return out


def test_the_corpus_is_present_and_large():
    """A truncated corpus would make every other test here pass vacuously."""
    pairs = _pairs()
    assert len(pairs) > 6000, f"corpus looks truncated: {len(pairs)} pairs"


def test_every_word_matches_postgresql():
    pairs = _pairs()
    wrong = [
        (w, expected, snowball.stem(w)) for w, expected in pairs if snowball.stem(w) != expected
    ]
    assert not wrong, (
        f"{len(wrong)} of {len(pairs)} words differ from PostgreSQL; first 10: {wrong[:10]}"
    )


@pytest.mark.parametrize(
    "word,expected",
    [
        # The two rules that were got wrong first time.
        ("argus", "argus"),
        ("apodous", "apod"),
        ("ankylosaurus", "ankylosaurus"),
        ("complement", "complement"),
        ("firmament", "firmament"),
        ("battlemented", "battlement"),
        # y-to-i, and the words that bypass it entirely.
        ("flies", "fli"),
        ("fly", "fli"),
        ("sky", "sky"),
        ("skies", "sky"),
        ("skis", "ski"),
        ("dying", "die"),
        ("early", "earli"),
        # step 1b: doubling, e-restoration, and the short-word exception.
        ("hopping", "hop"),
        ("hoping", "hope"),
        ("falling", "fall"),
        ("agreed", "agre"),
        ("feed", "feed"),
        ("proceed", "proceed"),
        ("inning", "inning"),
        # the long derivational chains.
        ("generalization", "general"),
        ("oscillators", "oscil"),
        ("decisiveness", "decis"),
        ("hopefulness", "hope"),
        ("sensitivity", "sensit"),
        ("communism", "communism"),
    ],
)
def test_hand_picked_rules(word, expected):
    assert snowball.stem(word) == expected


def test_short_words_are_untouched(word="be"):
    assert snowball.stem(word) == word
    assert snowball.stem("a") == "a"


def test_stem_text_tokenises_and_stems():
    assert snowball.stem_text("Running cats, quickly!") == ["run", "cat", "quick"]
