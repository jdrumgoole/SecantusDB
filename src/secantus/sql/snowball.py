"""The English (Porter2) Snowball stemmer — the algorithm PostgreSQL's
``english`` text-search configuration uses.

Written out rather than taken from a dependency: SecantusDB ships
self-contained binary wheels, and a stemmer is a closed, fully-specified
algorithm with a reference description (snowballstem.org/algorithms/english),
so vendoring the behaviour costs less than vendoring a package.

Correctness here is measured, not argued. ``tests/test_sql_stemmer.py`` runs
this against **6,101 words stemmed by PostgreSQL 14.13's own
``ts_lexize('english_stem', …)``** and requires an exact match on every one; the
corpus is a random sample of the system dictionary plus a hand-picked set that
exercises the rules most likely to be got wrong (the ``y``-to-``i`` cases, the
short-word exception, the ``e``-deletion condition, and the step-1b doubling
rules).
"""

from __future__ import annotations

import re

_VOWELS = "aeiouy"
_DOUBLES = ("bb", "dd", "ff", "gg", "mm", "nn", "pp", "rr", "tt")
_LI_ENDING = "cdeghkmnrt"

#: Words the algorithm does not touch, with their fixed stems.
_SPECIAL = {
    "skis": "ski",
    "skies": "sky",
    "dying": "die",
    "lying": "lie",
    "tying": "tie",
    "idly": "idl",
    "gently": "gentl",
    "ugly": "ugli",
    "early": "earli",
    "only": "onli",
    "singly": "singl",
    "sky": "sky",
    "news": "news",
    "howe": "howe",
    "atlas": "atlas",
    "cosmos": "cosmos",
    "bias": "bias",
    "andes": "andes",
}

#: Words that stop after step 1a.
_STOP_AFTER_1A = frozenset({"inning", "outing", "canning", "herring", "earring", "proceed",
                            "exceed", "succeed"})  # fmt: skip

_STEP2 = [
    ("ization", "ize"), ("ational", "ate"), ("fulness", "ful"), ("ousness", "ous"),
    ("iveness", "ive"), ("tional", "tion"), ("biliti", "ble"), ("lessli", "less"),
    ("entli", "ent"), ("ation", "ate"), ("alism", "al"), ("aliti", "al"),
    ("ousli", "ous"), ("iviti", "ive"), ("fulli", "ful"), ("enci", "ence"),
    ("anci", "ance"), ("abli", "able"), ("izer", "ize"), ("ator", "ate"),
    ("alli", "al"), ("bli", "ble"), ("ogi", "og"), ("li", ""),
]  # fmt: skip

_STEP3 = [
    ("ational", "ate"), ("tional", "tion"), ("alize", "al"), ("icate", "ic"),
    ("iciti", "ic"), ("ative", ""), ("ical", "ic"), ("ness", ""), ("ful", ""),
]  # fmt: skip

_STEP4 = [
    "ement", "ance", "ence", "able", "ible", "ment", "ant", "ent", "ism", "ate",
    "iti", "ous", "ive", "ize", "al", "er", "ic",
]  # fmt: skip


def _is_vowel(word: str, i: int) -> bool:
    return 0 <= i < len(word) and word[i] in _VOWELS


def _mark_y(word: str) -> str:
    """Upper-case every ``y`` that acts as a CONSONANT, so the vowel tests below
    can treat lower-case ``y`` as a vowel unconditionally."""
    if not word:
        return word
    out = list(word)
    if out[0] == "y":
        out[0] = "Y"
    for i in range(1, len(out)):
        if out[i] == "y" and out[i - 1] in _VOWELS:
            out[i] = "Y"
    return "".join(out)


def _regions(word: str) -> tuple[int, int]:
    """``(R1, R2)`` — the offsets after the first and second vowel-consonant
    pair, with Snowball's two exceptional prefixes."""
    r1 = len(word)
    for prefix in ("gener", "commun", "arsen"):
        if word.startswith(prefix):
            r1 = len(prefix)
            break
    else:
        for i in range(1, len(word)):
            if word[i] not in _VOWELS and word[i - 1] in _VOWELS:
                r1 = i + 1
                break
    r2 = len(word)
    for i in range(r1 + 1, len(word)):
        if word[i] not in _VOWELS and word[i - 1] in _VOWELS:
            r2 = i + 1
            break
    return r1, r2


def _ends_short_syllable(word: str) -> bool:
    if len(word) == 2:
        return _is_vowel(word, 0) and not _is_vowel(word, 1)
    if len(word) < 3:
        return False
    i = len(word) - 1
    return (
        not _is_vowel(word, i)
        and word[i] not in "wxY"
        and _is_vowel(word, i - 1)
        and not _is_vowel(word, i - 2)
    )


def _is_short(word: str, r1: int) -> bool:
    return r1 >= len(word) and _ends_short_syllable(word)


def _has_vowel(s: str) -> bool:
    return any(c in _VOWELS for c in s)


def stem(word: str) -> str:
    """The Porter2 stem of `word` (already lower-cased)."""
    if len(word) <= 2:
        return word
    if word in _SPECIAL:
        return _SPECIAL[word]

    w = word.lstrip("'")
    w = _mark_y(w)
    r1, r2 = _regions(w)

    # Step 0: possessive suffixes.
    for suffix in ("'s'", "'s", "'"):
        if w.endswith(suffix):
            w = w[: -len(suffix)]
            break

    # Step 1a: plurals.
    if w.endswith("sses"):
        w = w[:-2]
    elif w.endswith(("ied", "ies")):
        w = w[:-2] if len(w) > 4 else w[:-1]
    elif w.endswith(("us", "ss")):
        # Both are EXEMPT, not just `ss`. Without the `us` half, `argus` stemmed
        # to `argu` (PostgreSQL keeps `argus`) and — worse — `apodous` lost its
        # `s` here, so step 4's `ous` no longer matched and it stemmed to
        # `apodou` instead of `apod`. That single omission accounted for 248 of
        # the 6,101 corpus words.
        pass
    elif w.endswith("s") and _has_vowel(w[:-2]):
        w = w[:-1]

    if w.lower() in _STOP_AFTER_1A:
        return w.lower()

    # Step 1b: past tense and gerunds.
    if w.endswith(("eedly", "eed")):
        suffix = "eedly" if w.endswith("eedly") else "eed"
        if len(w) - len(suffix) >= r1:
            w = w[: -len(suffix)] + "ee"
    else:
        for suffix in ("ingly", "edly", "ing", "ed"):
            if w.endswith(suffix) and _has_vowel(w[: -len(suffix)]):
                w = w[: -len(suffix)]
                if w.endswith(("at", "bl", "iz")):
                    w += "e"
                elif w.endswith(_DOUBLES):
                    w = w[:-1]
                elif _is_short(w, r1):
                    w += "e"
                break

    # Step 1c: terminal y.
    if len(w) > 2 and w[-1] in "yY" and not _is_vowel(w, len(w) - 2):
        w = w[:-1] + "i"

    r1, r2 = _regions(w)

    # Step 2 / 3 / 4: suffix replacement, gated on R1 / R2.
    for suffix, repl in _STEP2:
        if w.endswith(suffix):
            if len(w) - len(suffix) >= r1:
                if suffix == "ogi" and not w[: -len(suffix)].endswith("l"):
                    break
                if suffix == "li" and (not w[: -len(suffix)] or w[-3] not in _LI_ENDING):
                    break
                w = w[: -len(suffix)] + repl
            break

    for suffix, repl in _STEP3:
        if w.endswith(suffix):
            if len(w) - len(suffix) >= r1:
                if suffix == "ative":
                    if len(w) - len(suffix) >= r2:
                        w = w[: -len(suffix)]
                else:
                    w = w[: -len(suffix)] + repl
            break

    # Snowball picks the LONGEST matching suffix and then applies its condition
    # ONCE. Falling through to a shorter suffix when the R2 test fails is a
    # different algorithm: `complement` matched `ement` (correctly rejected,
    # R2 too far), then matched `ent` (accepted) and stemmed to `complem`,
    # where PostgreSQL leaves `complement` alone. `_STEP4` is ordered
    # longest-first for exactly this reason.
    match = next((suffix for suffix in _STEP4 if w.endswith(suffix)), None)
    if match is not None:
        if len(w) - len(match) >= r2:
            w = w[: -len(match)]
    elif w.endswith("ion") and len(w) - 3 >= r2 and len(w) > 3 and w[-4] in "st":
        w = w[:-3]

    # Step 5: terminal e and doubled l.
    r1, r2 = _regions(w)
    if w.endswith("e"):
        if len(w) - 1 >= r2 or (len(w) - 1 >= r1 and not _ends_short_syllable(w[:-1])):
            w = w[:-1]
    elif w.endswith("ll") and len(w) - 1 >= r2:
        w = w[:-1]

    return w.replace("Y", "y").lower()


_WORD_RE = re.compile(r"[0-9A-Za-z]+")


def stem_text(text: str) -> list[str]:
    """Every word of `text`, stemmed."""
    return [stem(t.lower()) for t in _WORD_RE.findall(text or "")]
