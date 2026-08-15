"""Postgres full-text search: ``tsvector`` / ``tsquery`` types and the
``to_tsvector`` / ``to_tsquery`` / ``plainto_tsquery`` builders, the ``@@`` match
operator, and ``ts_rank``.

Storage. A ``tsvector`` is ``{"tsvector": {lexeme: [pos, …], …}}`` — normalised
lexemes (lower-cased, stop-words dropped) with 1-based token positions. A
``tsquery`` is a small boolean tree of ``{"lexeme": w}`` / ``{"and": [..]}`` /
``{"or": [..]}`` / ``{"not": q}`` nodes under ``{"tsquery": <node>}``.

Prefix (``cat:*``) and phrase (``foo <-> bar`` / ``foo <N> bar``) queries are
supported (positions are tracked in the tsvector), as are ``phraseto_tsquery``
and ``ts_headline``.

Simplifications vs real Postgres: the text-search configuration is fixed
(English stop-words, **no stemming** — ``cats`` and ``cat`` stay distinct), and
``ts_rank`` is a simple normalised match count rather than the cover-density
algorithm. Lexeme weights (``:A`` / ``setweight`` / weighted ``ts_rank``) are out
of scope (the tsvector stores no per-lexeme weight).
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


#: Real PG silently refuses to index words longer than 2046 bytes (with a
#: client NOTICE we don't emit) — to_tsvector of a 10 kB token yields an
#: empty tsvector, which pgx's trigger-maintenance test relies on.
_MAX_LEXEME_LEN = 2046


def _lexemes(text: str) -> list[str]:
    """Tokenise text into normalised lexemes (lower-case words, stop-words kept so
    the caller can decide — positions count every token in Postgres)."""
    return [t.lower() for t in _TOKEN_RE.findall(text or "")]


def to_tsvector(text: str) -> dict[str, Any]:
    """Build a ``tsvector`` subdocument from text: lower-cased non-stop-word tokens
    mapped to their 1-based positions."""
    positions: dict[str, list[int]] = {}
    for pos, lex in enumerate(_lexemes(text), start=1):
        if lex in _STOPWORDS or len(lex) > _MAX_LEXEME_LEN:
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


def phraseto_tsquery(text: str) -> dict[str, Any]:
    """``phraseto_tsquery`` — chain the text's non-stop-word lexemes with the phrase
    operator ``<->`` (adjacency), so word order matters. A dropped stop-word widens
    the distance between its neighbours (``<2>``), matching Postgres."""
    tokens = _lexemes(text)
    terms: list[tuple[str, int]] = []  # (lexeme, gap-since-previous-kept-term)
    gap = 1
    for lex in tokens:
        if lex in _STOPWORDS:
            gap += 1
            continue
        terms.append((lex, gap))
        gap = 1
    if not terms:
        return {"tsquery": None}
    node: Any = {"lexeme": terms[0][0]}
    for lex, distance in terms[1:]:
        node = {"phrase": {"left": node, "right": {"lexeme": lex}, "distance": distance}}
    return {"tsquery": node}


def to_tsquery(text: str) -> dict[str, Any]:
    """``to_tsquery`` — parse a boolean query over ``& | !`` and parentheses."""
    parser = _TSQueryParser(text)
    node = parser.parse()
    return {"tsquery": node}


# The web-search grammar: bare words AND together, ``"quoted phrases"`` become
# phrase (``<->``) queries, the bare word ``or`` is an OR, and a leading ``-``
# negates the following word/phrase. Any other punctuation is ignored.
_WEBSEARCH_TOKEN_RE = re.compile(r'\s*(-?"[^"]*"|-?[^\s"]+)')


def websearch_to_tsquery(text: str) -> dict[str, Any]:
    """``websearch_to_tsquery`` — parse a web-search-style query."""
    items: list[Any] = []  # a mix of query-nodes and the sentinel "or"
    for m in _WEBSEARCH_TOKEN_RE.finditer(text):
        tok = m.group(1)
        negate = tok.startswith("-")
        if negate:
            tok = tok[1:]
        if not tok:
            continue
        if tok.startswith('"') and tok.endswith('"'):
            node = phraseto_tsquery(tok[1:-1])["tsquery"]
        elif tok.lower() == "or" and not negate:
            items.append("or")
            continue
        else:
            lexemes = [lex for lex in _lexemes(tok) if lex not in _STOPWORDS]
            node = (
                {"and": [{"lexeme": x} for x in lexemes]}
                if len(lexemes) > 1
                else ({"lexeme": lexemes[0]} if lexemes else None)
            )
        if node is None:
            continue
        items.append({"not": node} if negate else node)

    # Split on the ``or`` sentinels into AND-groups, then OR the groups together.
    groups: list[list[Any]] = [[]]
    for it in items:
        if it == "or":
            groups.append([])
        else:
            groups[-1].append(it)
    or_terms: list[Any] = []
    for grp in groups:
        if not grp:
            continue
        or_terms.append(grp[0] if len(grp) == 1 else {"and": grp})
    if not or_terms:
        return {"tsquery": None}
    node = or_terms[0] if len(or_terms) == 1 else {"or": or_terms}
    return {"tsquery": node}


_QUERY_TOKEN_RE = re.compile(r"\s*(<->|<\d+>|&|\||!|\(|\)|[0-9A-Za-z]+(?::\*)?)")
_PHRASE_OP_RE = re.compile(r"^<(-|\d+)>$")


class _TSQueryParser:
    """A tiny recursive-descent parser: ``or := and ('|' and)*``; ``and := phrase
    ('&' phrase)*``; ``phrase := factor (('<->' | '<N>') factor)*``; a factor is
    ``!factor`` / ``(or)`` / a lexeme (optionally ``lex:*`` for a prefix match)."""

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
        node = self._parse_phrase()
        while self._peek() == "&":
            self._next()
            node = {"and": [node, self._parse_phrase()]}
        return node

    def _parse_phrase(self) -> Any:
        node = self._parse_factor()
        while (tok := self._peek()) is not None and _PHRASE_OP_RE.match(tok):
            self._next()
            distance = 1 if tok == "<->" else int(tok[1:-1])
            right = self._parse_factor()
            node = {"phrase": {"left": node, "right": right, "distance": distance}}
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
        if tok in ("&", "|", ")") or _PHRASE_OP_RE.match(tok):
            raise TSQueryError(f"unexpected token {tok!r} in tsquery")
        if tok.endswith(":*"):
            return {"prefix": tok[:-2].lower()}
        return {"lexeme": tok.lower()}


def render_tsquery(v: Any) -> str:
    """Render a ``tsquery`` as its Postgres text form (``'cat' & 'dog'``)."""
    return _render_query_node(v.get("tsquery") if isinstance(v, dict) else None)


def _render_query_node(node: Any) -> str:
    if node is None:
        return ""
    if "lexeme" in node:
        return f"'{node['lexeme']}'"
    if "prefix" in node:
        return f"'{node['prefix']}':*"
    if "phrase" in node:
        ph = node["phrase"]
        op = "<->" if ph["distance"] == 1 else f"<{ph['distance']}>"
        return f"{_wrap(ph['left'])} {op} {_wrap(ph['right'])}"
    if "not" in node:
        return "!" + _render_query_node(node["not"])
    if "and" in node:
        return " & ".join(_wrap(a) for a in node["and"])
    if "or" in node:
        return " | ".join(_wrap(a) for a in node["or"])
    return ""


def _wrap(node: Any) -> str:
    inner = _render_query_node(node)
    return f"( {inner} )" if ("and" in node or "or" in node or "phrase" in node) else inner


# --------------------------------------------------------------------------- #
# Match + rank
# --------------------------------------------------------------------------- #


def matches(tsvector: Any, tsquery: Any) -> bool:
    """Does the ``tsvector`` satisfy the ``tsquery``? The ``@@`` operator."""
    posmap = tsvector_lexemes(tsvector)
    node = tsquery.get("tsquery") if isinstance(tsquery, dict) else None
    return _eval_query(node, posmap)


def _eval_query(node: Any, posmap: dict[str, list[int]]) -> bool:
    if node is None:
        return False
    if "lexeme" in node:
        return node["lexeme"] in posmap
    if "prefix" in node:
        return any(k.startswith(node["prefix"]) for k in posmap)
    if "phrase" in node:
        return bool(_phrase_positions(node["phrase"], posmap))
    if "not" in node:
        return not _eval_query(node["not"], posmap)
    if "and" in node:
        return all(_eval_query(a, posmap) for a in node["and"])
    if "or" in node:
        return any(_eval_query(a, posmap) for a in node["or"])
    return False


def _end_positions(node: Any, posmap: dict[str, list[int]]) -> set[int]:
    """The set of token positions at which ``node`` (a lexeme / prefix / phrase)
    matches — the phrase operator uses these to check adjacency."""
    if node is None:
        return set()
    if "lexeme" in node:
        return set(posmap.get(node["lexeme"], []))
    if "prefix" in node:
        out: set[int] = set()
        for k, ps in posmap.items():
            if k.startswith(node["prefix"]):
                out.update(ps)
        return out
    if "phrase" in node:
        return _phrase_positions(node["phrase"], posmap)
    return set()


def _phrase_positions(ph: dict[str, Any], posmap: dict[str, list[int]]) -> set[int]:
    """End positions where ``left <distance> right`` is satisfied — ``right`` sits
    exactly ``distance`` tokens after ``left``."""
    left = _end_positions(ph["left"], posmap)
    right = _end_positions(ph["right"], posmap)
    d = ph["distance"]
    return {pb for pa in left for pb in right if pb == pa + d}


def _count_hits(node: Any, posmap: dict[str, list[int]]) -> int:
    """Total positive-term occurrences a query contributes, for ranking."""
    if node is None:
        return 0
    if "lexeme" in node:
        return len(posmap.get(node["lexeme"], []))
    if "prefix" in node:
        return sum(len(ps) for k, ps in posmap.items() if k.startswith(node["prefix"]))
    if "phrase" in node:
        return len(_phrase_positions(node["phrase"], posmap))
    if "not" in node:
        return 0
    if "and" in node or "or" in node:
        return sum(_count_hits(a, posmap) for a in node.get("and", node.get("or", [])))
    return 0


def ts_rank(tsvector: Any, tsquery: Any) -> float:
    """A simplified relevance score: the log-dampened count of query-term
    occurrences in the document, 0.0 when the query doesn't match. (Real Postgres
    uses a cover-density algorithm; this keeps the monotonic 'more hits ranks
    higher' behaviour that ``ORDER BY ts_rank(...) DESC`` relies on.)"""
    if not matches(tsvector, tsquery):
        return 0.0
    posmap = tsvector_lexemes(tsvector)
    node = tsquery.get("tsquery") if isinstance(tsquery, dict) else None
    hits = _count_hits(node, posmap)
    if hits == 0:
        return 0.0
    return round(1.0 - 1.0 / (1.0 + math.log1p(hits)), 6)


def _query_lexemes_and_prefixes(node: Any) -> tuple[set[str], set[str]]:
    """The positive lexemes and prefixes in a query, for ``ts_headline``."""
    lexemes: set[str] = set()
    prefixes: set[str] = set()
    if node is None:
        return lexemes, prefixes
    if "lexeme" in node:
        lexemes.add(node["lexeme"])
    elif "prefix" in node:
        prefixes.add(node["prefix"])
    elif "phrase" in node:
        for side in (node["phrase"]["left"], node["phrase"]["right"]):
            l2, p2 = _query_lexemes_and_prefixes(side)
            lexemes |= l2
            prefixes |= p2
    elif "and" in node or "or" in node:
        for a in node.get("and", node.get("or", [])):
            l2, p2 = _query_lexemes_and_prefixes(a)
            lexemes |= l2
            prefixes |= p2
    return lexemes, prefixes


def ts_headline(
    document: str, tsquery: Any, *, start_sel: str = "<b>", stop_sel: str = "</b>"
) -> str:
    """``ts_headline(document, query)`` — return the document with every token that
    matches a query lexeme / prefix wrapped in ``StartSel`` / ``StopSel`` (default
    ``<b>`` / ``</b>``). Simplified: the whole document is returned (no fragment
    selection / MaxWords windowing)."""
    node = tsquery.get("tsquery") if isinstance(tsquery, dict) else None
    lexemes, prefixes = _query_lexemes_and_prefixes(node)
    out: list[str] = []
    for part in re.split(r"([0-9A-Za-z]+)", document or ""):
        low = part.lower()
        is_word = bool(part) and _TOKEN_RE.fullmatch(part)
        if is_word and (low in lexemes or any(low.startswith(p) for p in prefixes)):
            out.append(f"{start_sel}{part}{stop_sel}")
        else:
            out.append(part)
    return "".join(out)
