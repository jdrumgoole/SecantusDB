"""Collation helpers for case- and accent-aware string comparison.

MongoDB's ``collation`` option overrides how strings are compared by
field-level operators (``$eq``, ``$gt``, ``$lt``, ``$in``, ``$nin``,
``$gte``, ``$lte``, ``$ne``) and by the cross-field equality used by
``distinct`` and ``$group``. Each storage / query call site that
accepts ``collation`` threads it through to ``matches()`` /
``apply_pipeline()`` which forward it to the comparison primitives
in this module.

In scope:

- ``locale`` is accepted but the value is treated as ``"en_US"`` /
  ``"simple"`` (the only locales the in-scope drivers exercise).
- ``strength`` 1–3:
    * **1 (primary)**: base characters only — case and accents
      ignored. Implemented via ``unicodedata.normalize('NFKD', …)``
      → strip combining marks → ``str.casefold()``.
    * **2 (secondary)**: accents kept, case ignored — ``casefold()``.
    * **3 (tertiary, default)**: standard Python equality / ordering.
- ``numericOrdering: true``: numeric digit substrings sort by value
  rather than codepoint (``"a10" > "a2"``). Implemented but only
  exercised when the user opts in.
- ``caseLevel``: when ``true`` alongside ``strength 1/2``, case
  matters. Falls back to ``str`` directly.

Out of scope: ``alternate`` (variable weights),
``maxVariable``, ``backwards``, ``caseFirst``, and locale-specific
tertiary differences (German sharp-s, etc.). Mongo-java-driver's
collation tests use ``strength 2`` only — the in-scope features above
cover every failing case in the gauge. Anything more would require
ICU.

Public API:

- ``Collation`` — a frozen normalised view of the user's spec.
- ``compare_keys(a, b, collation)`` — sort-style compare returning
  ``-1`` / ``0`` / ``1``.
- ``equal(a, b, collation)`` — collation-aware equality, used by
  field-level ``$eq`` / ``$in`` / ``$ne``.
- ``cmp_key(value, collation)`` — a hashable key suitable for
  bucketing (``$group``, ``distinct``).
"""

from __future__ import annotations

import functools
import re
import unicodedata
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Collation:
    """Resolved collation flags. ``None`` everywhere means no collation
    (the default — vanilla Python equality and ordering apply)."""

    strength: int = 3
    case_level: bool = False
    numeric_ordering: bool = False
    #: ``"off"`` (ICU's default), ``"lower"`` or ``"upper"``. Only affects
    #: ORDER, never matching, and only at the tertiary level.
    case_first: str = "off"
    #: French secondary ordering: accents compare from the END of the string.
    backwards: bool = False

    @property
    def case_insensitive(self) -> bool:
        return self.strength <= 2 and not self.case_level

    @property
    def accent_insensitive(self) -> bool:
        return self.strength <= 1

    @property
    def supports_index_encoding(self) -> bool:
        """Can this collation be baked into byte-sortable index entries?

        Strength 1/2/3 + ``caseLevel`` work — the normalisation is a
        deterministic string → string transformation that preserves
        sort order under byte comparison.

        ``numericOrdering`` does not. It needs a length-prefixed
        digit-run encoding to keep ``"a10" > "a2"`` correct under
        lex-byte ordering; we don't ship that v1, so queries that
        combine ``numericOrdering`` with a string-bearing index
        fall through to COLLSCAN. The default (no numericOrdering)
        covers the vast majority of case-insensitive use cases.
        """
        return not self.numeric_ordering


def parse(spec: Any) -> Collation | None:
    """Build a :class:`Collation` from the user's ``collation`` map.

    Returns ``None`` for falsy / non-dict input, so callers can pass
    the raw command argument through without pre-checking it.
    """
    if not isinstance(spec, dict) or not spec:
        return None
    strength = spec.get("strength", 3)
    try:
        strength = int(strength)
    except (TypeError, ValueError):
        strength = 3
    case_first = spec.get("caseFirst", "off")
    if case_first not in ("off", "lower", "upper"):
        case_first = "off"
    return Collation(
        strength=strength,
        case_level=bool(spec.get("caseLevel", False)),
        numeric_ordering=bool(spec.get("numericOrdering", False)),
        case_first=case_first,
        backwards=bool(spec.get("backwards", False)),
    )


def _strip_accents(s: str) -> str:
    # NFKD decomposes accented chars into base + combining marks; the
    # ``category('Mn')`` filter drops the marks, leaving the base.
    return "".join(c for c in unicodedata.normalize("NFKD", s) if unicodedata.category(c) != "Mn")


_NUMERIC_SPLIT = re.compile(r"(\d+)")


def _normalize_string(s: str, collation: Collation) -> Any:
    out = s
    if collation.accent_insensitive:
        out = _strip_accents(out)
    if collation.case_insensitive:
        out = out.casefold()
    if collation.numeric_ordering:
        # Split into runs of digits vs non-digits; digit runs become
        # ints so ``"a10" > "a2"`` orders correctly. Tuple is hashable
        # and comparable (Python compares (str, str) and (int, int)
        # within positionally-aligned tuples).
        parts = _NUMERIC_SPLIT.split(out)
        return tuple((int(p) if p.isdigit() else p) for p in parts if p != "")
    return out


#: The order ICU gives the common Latin combining marks at the SECONDARY level.
#: Codepoint order is not it -- mongod sorts ``á`` before ``à`` where the
#: codepoints run the other way (U+0300 grave, U+0301 acute). Derived by sorting
#: ``a`` plus each mark against mongod 8.2.11 (2026-09-01,
#: ``tools/probes/collation_order.py``), not copied from a specification, and
#: covering only the marks that probe reached. Anything not listed falls back to
#: its codepoint, which keeps the key total and deterministic -- and wrong only
#: in the same way the whole table was before.
_MARK_ORDER: tuple[int, ...] = (
    0x332,  # low line
    0x301,  # acute
    0x300,  # grave
    0x306,  # breve
    0x302,  # circumflex
    0x30C,  # caron
    0x30A,  # ring above
    0x308,  # diaeresis
    0x30B,  # double acute
    0x303,  # tilde
    0x307,  # dot above
    0x327,  # cedilla
    0x328,  # ogonek
    0x304,  # macron
    0x309,  # hook above
    0x30F,  # double grave
    0x311,  # inverted breve
    0x323,  # dot below
    0x326,  # comma below
    0x331,  # macron below
)
_MARK_RANK: dict[int, int] = {cp: i for i, cp in enumerate(_MARK_ORDER)}
#: Unlisted marks sort after every listed one, by codepoint.
_MARK_RANK_FLOOR = len(_MARK_ORDER)


def _mark_weight(codepoint: int) -> tuple[int, int]:
    """Secondary weight for one combining mark: (table rank, codepoint)."""
    rank = _MARK_RANK.get(codepoint)
    if rank is None:
        return (_MARK_RANK_FLOOR, codepoint)
    return (rank, 0)


@functools.lru_cache(maxsize=8192)
def sort_levels(s: str, collation: Collation) -> tuple[Any, ...]:
    """A multi-level ORDERING key for ``s``, in the shape ICU uses.

    Ordering is not the same problem as matching, and this module only ever
    solved matching: ``_normalize_string`` folds a string down to one value, so
    two strings that differ only in an accent fall back to comparing whole
    CODEPOINTS -- which puts every accented word after ``z`` instead of beside
    its base letter, drops tertiary case order entirely, and ignores
    ``caseFirst`` / ``backwards`` / ``numericOrdering``. Measured against mongod
    8.2.11 on 2026-09-01, that was wrong on every ordering probe:
    ``["a","á","ä","az","b"]`` came back ``a az b á ä``.

    Three levels, compared in order:

    * **primary** -- the base letters: accents removed, case folded (or the
      digit-run tuple when ``numericOrdering`` is on, so ``"a2" < "a10"``).
    * **secondary** -- the accents, as one entry per base character holding
      that character's combining marks, weighted by ``_MARK_ORDER`` (codepoint
      order is NOT ICU's -- acute sorts before grave and the codepoints run the
      other way). ``backwards`` (French) REVERSES this level, which is what
      makes ``cote < côte < coté`` instead of ``cote < coté < côte``.
    * **tertiary** -- case, one rank per base character. ``caseFirst: "upper"``
      flips it.

    ``strength`` truncates the tuple: 1 keeps the primary alone (so ``a``,
    ``A`` and ``á`` tie, and mongod's stable order among them is the input
    order we also produce), 2 adds the secondary, 3 adds the tertiary.
    ``caseLevel`` re-adds the case rank at strength 1/2.

    NOT a substitute for :func:`_normalize_string`. Equality still goes through
    that one, unchanged: an index's on-disk bytes are built from it, and moving
    equality here would change which rows an index lookup finds. What this does
    change is that a collated SORT is no longer served by an index walk (see
    ``storage.find_matching``) -- the byte order in the entries table is the
    single-level normalisation and cannot reproduce these levels.

    What is still missing without ICU is the LOCALE: Swedish sorts ``ä`` after
    ``z`` rather than beside ``a``, and no amount of decomposition finds that
    out. Locale-specific ordering needs the CLDR data.

    Memoised because a comparison sort calls this O(n log n) times over the same
    n strings, and building the key is the expensive half. ``Collation`` is a
    frozen dataclass, so it is a valid cache key.
    """
    nfd = unicodedata.normalize("NFD", s)
    bases: list[str] = []
    marks: list[tuple[tuple[int, int], ...]] = []
    cases: list[int] = []
    for ch in nfd:
        if unicodedata.combining(ch):
            if marks:
                marks[-1] = marks[-1] + (_mark_weight(ord(ch)),)
            continue
        bases.append(ch)
        marks.append(())
        cases.append(1 if ch.isupper() else 0)
    primary_str = "".join(bases).casefold()
    primary: Any = primary_str
    if collation.numeric_ordering:
        parts = _NUMERIC_SPLIT.split(primary_str)
        primary = tuple((int(p) if p.isdigit() else p) for p in parts if p != "")

    secondary: tuple[Any, ...] = tuple(marks)
    if collation.backwards:
        secondary = tuple(reversed(secondary))

    case_ranks = tuple(cases)
    if collation.case_first == "upper":
        case_ranks = tuple(1 - c for c in case_ranks)

    if collation.strength <= 1:
        return (primary, case_ranks) if collation.case_level else (primary,)
    if collation.strength == 2:
        return (primary, secondary, case_ranks) if collation.case_level else (primary, secondary)
    return (primary, secondary, case_ranks)


def normalize_for_index_bytes(s: str, collation: Collation) -> bytes:
    """Normalised UTF-8 bytes for ``s`` under ``collation``, suitable
    for sortkey index encoding.

    Only valid when ``collation.supports_index_encoding`` is True.
    Applies ``_strip_accents`` (strength 1) and ``casefold``
    (case-insensitive) in the same order as :func:`_normalize_string`,
    so two strings that compare-equal under ``equal()`` produce the
    same bytes here — which is the invariant index lookups rely on.

    NOT a replacement for :func:`_normalize_string`. That helper
    returns Python values for in-memory comparison (and a tuple form
    for ``numericOrdering``); this returns bytes for on-disk index
    keys and intentionally skips ``numericOrdering`` (the caller has
    pre-checked ``supports_index_encoding``).
    """
    if not collation.supports_index_encoding:
        raise ValueError(
            "normalize_for_index_bytes: collation has numericOrdering, "
            "which can't be baked into byte-sortable index entries"
        )
    out = s
    if collation.accent_insensitive:
        out = _strip_accents(out)
    if collation.case_insensitive:
        out = out.casefold()
    return out.encode("utf-8")


def cmp_key(value: Any, collation: Collation | None) -> Any:
    """A hashable / comparable key for a single value under ``collation``.

    Non-string values pass through unchanged. Strings normalise per
    the collation flags. Used by ``distinct`` and ``$group`` to bucket
    equivalent values together.
    """
    if collation is None or not isinstance(value, str):
        return value
    return _normalize_string(value, collation)


def equal(a: Any, b: Any, collation: Collation | None) -> bool:
    """Collation-aware equality.

    For non-string operands (or when ``collation`` is ``None``), falls
    through to ``==``. For string-vs-string operands, both sides are
    normalised then compared.
    """
    if collation is None:
        return a == b
    if isinstance(a, str) and isinstance(b, str):
        return _normalize_string(a, collation) == _normalize_string(b, collation)
    return a == b


def compare_keys(a: Any, b: Any, collation: Collation | None) -> int:
    """Three-way compare returning -1 / 0 / 1.

    Used by range operators (``$gt`` / ``$lt`` etc.) when at least
    one operand is a string. Non-string compares fall through to
    Python's ``<`` / ``>`` (which the caller can do without this
    helper).
    """
    if collation is None or not (isinstance(a, str) and isinstance(b, str)):
        if a < b:
            return -1
        if a > b:
            return 1
        return 0
    na, nb = _normalize_string(a, collation), _normalize_string(b, collation)
    if na < nb:
        return -1
    if na > nb:
        return 1
    return 0
