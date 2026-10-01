"""Full-text search: text-search configurations and PostgreSQL's default
document parser.

Ported from the Rust server's ``crates/secantus-pgplan/src/fts.rs`` (itself
measured against PostgreSQL) and checked against PostgreSQL 15:

* The parser emits a hyphenated word as the compound AND its parts
  (``foo-bar`` is ``'foo-bar':1 'foo':2 'bar':3``); numbers, emails, hosts,
  URLs and paths are single tokens indexed unstemmed. A stop-word consumes a
  position; a blank does not.
* ``english`` lower-cases, drops the 127 words of ``english.stop`` and stems
  with Snowball's English (Porter2) algorithm (``secantus.sql.snowball``).
* Every other Snowball configuration (``french``, ``russian``, ...) drops its
  ``<language>.stop`` words (``fts_stopwords``) and stems with the Snowball
  algorithm of that language. Those algorithms come from the
  ``snowballstemmer`` package -- version 2.2.0 is the Snowball release
  PostgreSQL 15 ships, and matched ``ts_lexize`` on all 8,702 words sampled
  across the 27 languages. Without the package such a configuration is
  refused (0A000) rather than answered with unstemmed lexemes.
"""

from __future__ import annotations

import unicodedata
from functools import cache
from typing import Any

from . import errors
from . import snowball as _snowball
from .fts_stopwords import STOP_TEXT

#: The Snowball languages PostgreSQL ships a configuration for (besides
#: ``english`` and ``simple``).
LANGS = frozenset(
    {
        "arabic", "armenian", "basque", "catalan", "danish", "dutch", "finnish",
        "french", "german", "greek", "hindi", "hungarian", "indonesian", "irish",
        "italian", "lithuanian", "nepali", "norwegian", "portuguese", "romanian",
        "russian", "serbian", "spanish", "swedish", "tamil", "turkish", "yiddish",
    }
)  # fmt: skip

#: Configurations that send their ASCII words to ``english_stem``.
_ASCII_ENGLISH = frozenset({"russian", "hindi"})

#: The session default, ``default_text_search_config``.
DEFAULT_CONFIG = "english"

#: The longest indexable lexeme, in bytes (PostgreSQL rejects ``>= 2047``).
_MAX_LEXEME_BYTES = 2046
#: The largest position; later ones are clamped to it.
MAX_POS = 16383


def config(name: Any) -> str:
    """Resolve a configuration name (``english``, ``pg_catalog.simple``, ...);
    42704 for one PostgreSQL does not have."""
    if name is None:
        return DEFAULT_CONFIG
    raw = str(name).strip()
    n = raw.strip('"').lower()
    if n.startswith("pg_catalog."):
        n = n[len("pg_catalog.") :]
    if n in LANGS or n in ("english", "simple"):
        return n
    raise errors.SQLError("42704", f'text search configuration "{raw}" does not exist')


@cache
def _stopwords(lang: str) -> frozenset[str]:
    return frozenset(STOP_TEXT.get(lang, "").split())


@cache
def _stemmer(lang: str) -> Any:
    try:
        import snowballstemmer
    except ImportError:  # pragma: no cover - depends on the install
        raise errors.SQLError(
            "0A000",
            f'text search configuration "{lang}" needs the snowballstemmer package',
        ) from None
    return snowballstemmer.stemmer(lang)


def _letter(c: str) -> bool:
    """A letter to PostgreSQL's parser under a UTF-8 ``LC_CTYPE``: any Unicode
    letter, or a combining mark (which stays inside its word -- Devanagari's
    virama), but not the punctuation or symbols around it."""
    if c.isascii():
        return c.isalpha()
    return c.isalpha() or unicodedata.category(c)[0] == "M"


def _word_char(c: str) -> bool:
    return ("0" <= c <= "9") or _letter(c)


def _lower(s: str) -> str:
    """Simple (one-to-one) lower-casing, as ``lower()`` under ``C.UTF-8``."""
    out = []
    for c in s:
        low = c.lower()
        out.append(low if len(low) == 1 else ("i" if c == "İ" else c))
    return "".join(out)


_PROTOCOLS = ("https://", "http://", "ftp://")
_JOINERS = frozenset("-.@/_+")


def tokens(text: str) -> list[tuple[str, bool]]:
    """``(token, is_word)`` for every token the default parser indexes, in
    order. ``is_word`` tokens go through the configuration's stemmer; the rest
    (numbers, emails, hosts, URLs) are indexed as written."""
    chars = text
    n = len(chars)
    out: list[tuple[str, bool]] = []
    i = 0
    while i < n:
        c = chars[i]
        if c.isascii() and c.isalpha():
            lower = chars[i : i + 10].lower()
            proto = next((p for p in _PROTOCOLS if lower.startswith(p)), None)
            if proto is not None:
                i += len(proto)
                continue
        at_boundary = i == 0 or not _word_char(chars[i - 1])
        signed = c == "-" and i + 1 < n and chars[i + 1].isdigit() and at_boundary
        signed = signed and chars[i + 1].isascii()
        path = (
            c == "/"
            and i + 1 < n
            and _word_char(chars[i + 1])
            and (i == 0 or chars[i - 1].isspace())
        )
        if not _word_char(c) and not signed and not path:
            i += 1
            continue
        start = j = i
        if chars[j] in "-/":
            j += 1
        while j < n:
            c2 = chars[j]
            joiner = c2 in _JOINERS and j + 1 < n and _word_char(chars[j + 1]) and j > start
            if _word_char(c2) or joiner:
                j += 1
            else:
                break
        i = j
        _emit(chars[start:j], out)
    return out


def _all_digits(s: str) -> bool:
    return bool(s) and all("0" <= c <= "9" for c in s)


def _is_number(s: str) -> bool:
    body = s[1:] if s.startswith("-") else s
    if _all_digits(body):
        return True
    p = next((k for k, c in enumerate(body) if c in "eE"), None)
    mant, exp = (body, None) if p is None else (body[:p], body[p + 1 :])
    mant_ok = bool(mant) and all(_all_digits(x) for x in mant.split("."))
    exp_ok = exp is None or _all_digits(exp[1:] if exp[:1] in ("-", "+") else exp)
    return mant_ok and exp_ok


def _is_host(s: str) -> bool:
    labels = s.split(".")
    return (
        len(labels) >= 2
        and all(
            lab and all((c.isascii() and c.isalnum()) or c == "-" for c in lab) for lab in labels
        )
        and len(labels[-1]) >= 2
        and all(c.isascii() and c.isalpha() for c in labels[-1])
    )


def _has_digit(s: str) -> bool:
    return any("0" <= c <= "9" for c in s)


def _emit(raw: str, out: list[tuple[str, bool]]) -> None:
    """Classify one connected run into the tokens PostgreSQL's parser emits."""
    if _is_number(raw):
        out.append((raw, False))
        return
    if "@" in raw:
        local, _, host = raw.partition("@")
        if local and _is_host(host) and "@" not in host:
            out.append((raw, False))
            return
    slash = raw.find("/")
    if slash >= 0:
        host, path = raw[:slash], raw[slash:]
        if _is_host(host):
            out.extend([(raw, False), (host, False), (path, False)])
            return
    if _is_host(raw):
        out.append((raw, False))
        return
    if raw.startswith("/") or (
        "." in raw and all(seg and all(_word_char(c) for c in seg) for seg in raw.split("."))
    ):
        out.append((raw, False))
        return
    pieces: list[str] = []
    cur = ""
    for c in raw:
        if c in ".@/_+":
            if cur:
                pieces.append(cur)
            cur = ""
        else:
            cur += c
    if cur:
        pieces.append(cur)
    for seg in pieces:
        parts = [p for p in seg.split("-") if p]
        if (
            len(parts) > 1
            and all(seg.split("-"))
            and any(any(_letter(c) for c in p) for p in parts)
        ):
            numeric = any(_has_digit(p) for p in parts)
            out.append((seg, not numeric))
            out.extend((p, not _has_digit(p)) for p in parts)
        else:
            out.extend((p, not (_is_number(p) or _has_digit(p))) for p in parts)


def normalise(token: str, is_word: bool, cfg: str) -> str | None:
    """One token through the configuration's dictionary: ``None`` for a
    stop-word (which still takes a position)."""
    lower = _lower(token)
    if cfg == "simple" or not is_word:
        return lower
    if cfg == "english" or (cfg in _ASCII_ENGLISH and lower.isascii()):
        if lower in _stopwords("english"):
            return None
        return _snowball.stem(lower)
    if lower in _stopwords(cfg):
        return None
    return _stemmer(cfg).stemWord(lower)


def parse_document(text: str, cfg: str) -> list[tuple[int, str | None]]:
    """``(position, lexeme)`` for every indexed token; a stop-word yields a
    position with no lexeme."""
    out: list[tuple[int, str | None]] = []
    pos = 0
    for tok, is_word in tokens(text or ""):
        if len(tok.encode("utf-8")) > _MAX_LEXEME_BYTES:
            continue
        pos = min(pos + 1, MAX_POS)
        out.append((pos, normalise(tok, is_word, cfg)))
    return out


def is_word_char(c: str) -> bool:
    return _word_char(c)
