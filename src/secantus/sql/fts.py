"""Postgres full-text search: ``tsvector`` / ``tsquery`` types and the
``to_tsvector`` / ``to_tsquery`` / ``plainto_tsquery`` builders, the ``@@`` match
operator, and ``ts_rank``.

Storage. A ``tsvector`` is ``{"tsvector": {lexeme: [pos, …], …}}`` — normalised
lexemes (lower-cased, stop-words dropped) with 1-based token positions. A
``tsquery`` is a small boolean tree of ``{"lexeme": w}`` / ``{"and": [..]}`` /
``{"or": [..]}`` / ``{"not": q}`` nodes under ``{"tsquery": <node>}``.

Simplifications vs real Postgres: the text-search configuration is fixed
(English stop-words, **no stemming** — ``cats`` and ``cat`` stay distinct), and
``ts_rank`` is a simple normalised match count rather than the cover-density
algorithm. Weights (``:A`` / ``setweight``), prefix (``cat:*``) and phrase
(``<->``) queries are out of scope.
"""

from __future__ import annotations

import math
import re
from typing import Any

# A small English stop-word set (a subset of Postgres' ``english`` list — enough
# for the common cases without shipping the full 100+ word table).
_STOPWORDS = frozenset(
    {
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in",
        "into", "is", "it", "no", "not", "of", "on", "or", "such", "that", "the",
        "their", "then", "there", "these", "they", "this", "to", "was", "will", "with",
    }
)  # fmt: skip

_TOKEN_RE = re.compile(r"[0-9A-Za-z]+")


class TSQueryError(ValueError):
    """A malformed ``tsquery`` text."""


def _lexemes(text: str) -> list[str]:
    """Tokenise text into normalised lexemes (lower-case words, stop-words kept so
    the caller can decide — positions count every token in Postgres)."""
    return [t.lower() for t in _TOKEN_RE.findall(text or "")]


def to_tsvector(text: str) -> dict[str, Any]:
    """Build a ``tsvector`` subdocument from text: lower-cased non-stop-word tokens
    mapped to their 1-based positions."""
    positions: dict[str, list[int]] = {}
    for pos, lex in enumerate(_lexemes(text), start=1):
        if lex in _STOPWORDS:
            continue
        positions.setdefault(lex, []).append(pos)
    return {"tsvector": positions}


def is_tsvector(v: Any) -> bool:
    return isinstance(v, dict) and "tsvector" in v


def is_tsquery(v: Any) -> bool:
    return isinstance(v, dict) and "tsquery" in v


def tsvector_lexemes(v: Any) -> dict[str, list[int]]:
    return v.get("tsvector", {}) if isinstance(v, dict) else {}


def render_tsvector(v: Any) -> str:
    """Render a ``tsvector`` as Postgres' text form ``'cat':2 'sat':3`` (lexemes in
    sort order, positions ascending)."""
    lexemes = tsvector_lexemes(v)
    parts = []
    for lex in sorted(lexemes):
        pos = lexemes[lex]
        if pos:
            parts.append(f"'{lex}':" + ",".join(str(p) for p in pos))
        else:
            parts.append(f"'{lex}'")
    return " ".join(parts)


def parse_tsvector(text: str) -> dict[str, Any]:
    """Parse a ``tsvector`` text literal (``'cat':2 'sat' foo``) into the
    subdocument form. Accepts bare or quoted lexemes with optional ``:pos`` lists."""
    positions: dict[str, list[int]] = {}
    for token in text.split():
        if ":" in token:
            lex, _, poss = token.partition(":")
            pos_list = [int(p) for p in re.findall(r"\d+", poss)]
        else:
            lex, pos_list = token, []
        lex = lex.strip().strip("'").lower()
        if not lex:
            continue
        positions.setdefault(lex, []).extend(pos_list)
    return {"tsvector": positions}


# --------------------------------------------------------------------------- #
# tsquery
# --------------------------------------------------------------------------- #


def plainto_tsquery(text: str) -> dict[str, Any]:
    """``plainto_tsquery`` — AND together every non-stop-word lexeme in the text."""
    terms = [{"lexeme": lex} for lex in _lexemes(text) if lex not in _STOPWORDS]
    if not terms:
        return {"tsquery": None}
    node = terms[0] if len(terms) == 1 else {"and": terms}
    return {"tsquery": node}


def to_tsquery(text: str) -> dict[str, Any]:
    """``to_tsquery`` — parse a boolean query over ``& | !`` and parentheses."""
    parser = _TSQueryParser(text)
    node = parser.parse()
    return {"tsquery": node}


_QUERY_TOKEN_RE = re.compile(r"\s*(&|\||!|\(|\)|[0-9A-Za-z]+)")


class _TSQueryParser:
    """A tiny recursive-descent parser: ``expr := term (('|') term)*`` where a term
    is an AND-chain and a factor is ``!factor`` / ``(expr)`` / a lexeme."""

    def __init__(self, text: str) -> None:
        self._tokens = self._tokenize(text)
        self._i = 0

    def _tokenize(self, text: str) -> list[str]:
        out: list[str] = []
        pos = 0
        while pos < len(text):
            m = _QUERY_TOKEN_RE.match(text, pos)
            if not m:
                if text[pos].isspace():
                    pos += 1
                    continue
                raise TSQueryError(f"invalid tsquery token near {text[pos:]!r}")
            out.append(m.group(1))
            pos = m.end()
        return out

    def _peek(self) -> str | None:
        return self._tokens[self._i] if self._i < len(self._tokens) else None

    def _next(self) -> str | None:
        tok = self._peek()
        if tok is not None:
            self._i += 1
        return tok

    def parse(self) -> Any:
        node = self._parse_or()
        if self._peek() is not None:
            raise TSQueryError(f"trailing tsquery input near {self._peek()!r}")
        return node

    def _parse_or(self) -> Any:
        node = self._parse_and()
        while self._peek() == "|":
            self._next()
            node = {"or": [node, self._parse_and()]}
        return node

    def _parse_and(self) -> Any:
        node = self._parse_factor()
        while self._peek() == "&":
            self._next()
            node = {"and": [node, self._parse_factor()]}
        return node

    def _parse_factor(self) -> Any:
        tok = self._next()
        if tok is None:
            raise TSQueryError("unexpected end of tsquery")
        if tok == "!":
            return {"not": self._parse_factor()}
        if tok == "(":
            node = self._parse_or()
            if self._next() != ")":
                raise TSQueryError("unbalanced parentheses in tsquery")
            return node
        if tok in ("&", "|", ")"):
            raise TSQueryError(f"unexpected token {tok!r} in tsquery")
        return {"lexeme": tok.lower()}


def render_tsquery(v: Any) -> str:
    """Render a ``tsquery`` as its Postgres text form (``'cat' & 'dog'``)."""
    return _render_query_node(v.get("tsquery") if isinstance(v, dict) else None)


def _render_query_node(node: Any) -> str:
    if node is None:
        return ""
    if "lexeme" in node:
        return f"'{node['lexeme']}'"
    if "not" in node:
        return "!" + _render_query_node(node["not"])
    if "and" in node:
        return " & ".join(_wrap(a) for a in node["and"])
    if "or" in node:
        return " | ".join(_wrap(a) for a in node["or"])
    return ""


def _wrap(node: Any) -> str:
    inner = _render_query_node(node)
    return f"( {inner} )" if ("and" in node or "or" in node) else inner


# --------------------------------------------------------------------------- #
# Match + rank
# --------------------------------------------------------------------------- #


def matches(tsvector: Any, tsquery: Any) -> bool:
    """Does the ``tsvector`` satisfy the ``tsquery``? The ``@@`` operator."""
    lexemes = set(tsvector_lexemes(tsvector))
    node = tsquery.get("tsquery") if isinstance(tsquery, dict) else None
    return _eval_query(node, lexemes)


def _eval_query(node: Any, lexemes: set[str]) -> bool:
    if node is None:
        return False
    if "lexeme" in node:
        return node["lexeme"] in lexemes
    if "not" in node:
        return not _eval_query(node["not"], lexemes)
    if "and" in node:
        return all(_eval_query(a, lexemes) for a in node["and"])
    if "or" in node:
        return any(_eval_query(a, lexemes) for a in node["or"])
    return False


def _query_terms(node: Any) -> list[str]:
    """Every positive (non-negated) lexeme mentioned in a tsquery, for ranking."""
    if node is None:
        return []
    if "lexeme" in node:
        return [node["lexeme"]]
    if "not" in node:
        return []
    if "and" in node or "or" in node:
        out: list[str] = []
        for a in node.get("and", node.get("or", [])):
            out.extend(_query_terms(a))
        return out
    return []


def ts_rank(tsvector: Any, tsquery: Any) -> float:
    """A simplified relevance score: the log-dampened count of query-term
    occurrences in the document, 0.0 when the query doesn't match. (Real Postgres
    uses a cover-density algorithm; this keeps the monotonic 'more hits ranks
    higher' behaviour that ``ORDER BY ts_rank(...) DESC`` relies on.)"""
    if not matches(tsvector, tsquery):
        return 0.0
    lexemes = tsvector_lexemes(tsvector)
    node = tsquery.get("tsquery") if isinstance(tsquery, dict) else None
    hits = 0
    for term in set(_query_terms(node)):
        hits += len(lexemes.get(term, []))
    if hits == 0:
        return 0.0
    return round(1.0 - 1.0 / (1.0 + math.log1p(hits)), 6)
