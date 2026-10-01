"""PostgreSQL full-text search: the ``tsvector`` / ``tsquery`` types, their
input and output functions, the ``to_tsvector`` / ``to_tsquery`` family, the
``@@`` match and the functions over the two types.

Ported from the Rust server's ``crates/secantus-pgplan/src/fts.rs`` (which is
transcribed from PostgreSQL's C source) and checked against PostgreSQL 15.
The configurations and the document parser live in ``fts_lang``; ranking and
``ts_headline`` in ``fts_rank``.

Storage. A ``tsvector`` is ``{"tsvector": {lexeme: [pos, ...]}}`` and, when a
position carries a weight other than D, ``"weights": {lexeme: [w, ...]}``
beside it (3 = A, 2 = B, 1 = C, 0 = D, one per position). The Rust server
reads the ``tsvector`` map and ignores the sibling, so the shared on-disk
shape is unchanged. A ``tsquery`` is ``{"tsquery": <node>}`` over
``{"lexeme": w}`` / ``{"prefix": w}`` leaves (optionally with a ``"weight"``
mask, A = 8 .. D = 1) and ``{"not": q}`` / ``{"and": [l, r]}`` /
``{"or": [l, r]}`` / ``{"phrase": {"left", "right", "distance"}}`` nodes.
A string in either position is the canonical TEXT the Rust server stores.
"""

from __future__ import annotations

import re
from typing import Any

from . import errors
from .fts_lang import DEFAULT_CONFIG, MAX_POS, config, is_word_char, parse_document

Vec = dict[str, list[tuple[int, int]]]

# Kept for callers that still catch it; every error raised here is a SQLError.
TSQueryError = errors.SQLError


# --------------------------------------------------------------------------- #
# tsvector
# --------------------------------------------------------------------------- #


def _normalise_vec(v: Vec) -> Vec:
    """Positions sorted, a repeated position kept once with its highest
    weight, at most 256 per lexeme -- what ``tsvector_in`` stores."""
    out: Vec = {}
    for lex in sorted(v):
        merged: list[tuple[int, int]] = []
        for p, w in sorted(v[lex]):
            if merged and merged[-1][0] == p:
                merged[-1] = (p, max(merged[-1][1], w))
            else:
                merged.append((p, w))
        out[lex] = merged[:256]
    return out


def _vec_out(v: Vec) -> dict[str, Any]:
    v = _normalise_vec(v)
    doc: dict[str, Any] = {"tsvector": {lex: [p for p, _ in ps] for lex, ps in v.items()}}
    if any(w for ps in v.values() for _, w in ps):
        doc["weights"] = {lex: [w for _, w in ps] for lex, ps in v.items()}
    return doc


def vec_of(v: Any) -> Vec:
    """A stored tsvector (either server's shape) as ``{lexeme: [(pos, w)]}``."""
    if isinstance(v, str):
        return parse_vector(v)
    if not isinstance(v, dict):
        return {}
    positions = v.get("tsvector") or {}
    weights = v.get("weights") or {}
    out: Vec = {}
    for lex, ps in positions.items():
        ws = weights.get(lex) or []
        out[lex] = [
            (min(int(p), MAX_POS), int(ws[k]) if k < len(ws) else 0) for k, p in enumerate(ps or [])
        ]
    return _normalise_vec(out)


def is_tsvector(v: Any) -> bool:
    return isinstance(v, dict) and "tsvector" in v


def is_tsquery(v: Any) -> bool:
    return isinstance(v, dict) and "tsquery" in v


def tsvector_lexemes(v: Any) -> dict[str, list[int]]:
    return {lex: [p for p, _ in ps] for lex, ps in vec_of(v).items()}


def _quote(lex: str) -> str:
    return "'" + "".join(c * 2 if c in "'\\" else c for c in lex) + "'"


_WEIGHT_LETTER = {3: "A", 2: "B", 1: "C", 0: ""}


def render_vector(v: Vec) -> str:
    parts = []
    for lex, ps in v.items():
        s = _quote(lex)
        if ps:
            s += ":" + ",".join(f"{p}{_WEIGHT_LETTER[w]}" for p, w in ps)
        parts.append(s)
    return " ".join(parts)


def render_tsvector(v: Any) -> str:
    return render_vector(vec_of(v))


def _syntax(kind: str, text: str) -> errors.SQLError:
    return errors.SQLError("42601", f'syntax error in {kind}: "{text}"')


def _read_word(text: str, i: int, stop: str) -> tuple[str, int] | None:
    """One lexeme -- quoted (``'it''s'``) or bare -- honouring backslash
    escapes; ``(word, next_index)``, or None for an unterminated quote."""
    n = len(text)
    out: list[str] = []
    if i < n and text[i] == "'":
        i += 1
        while True:
            if i >= n:
                return None
            c = text[i]
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
            elif c == "'":
                if i + 1 < n and text[i + 1] == "'":
                    out.append("'")
                    i += 2
                else:
                    return "".join(out), i + 1
            else:
                out.append(c)
                i += 1
    while i < n:
        c = text[i]
        if c.isspace() or c in stop:
            break
        if c == "\\" and i + 1 < n:
            out.append(text[i + 1])
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out), i


def parse_vector(text: str) -> Vec:
    """``tsvector_in``."""
    n = len(text)
    i = 0
    out: Vec = {}
    while True:
        while i < n and text[i].isspace():
            i += 1
        if i >= n:
            break
        got = _read_word(text, i, ":")
        if got is None or not got[0]:
            raise _syntax("tsvector", text)
        lex, i = got
        entry = out.setdefault(lex, [])
        if i < n and text[i] == ":":
            i += 1
            while True:
                start = i
                while i < n and "0" <= text[i] <= "9":
                    i += 1
                if i == start:
                    raise _syntax("tsvector", text)
                p = int(text[start:i])
                if p == 0:
                    raise errors.SQLError("42601", f'wrong position info in tsvector: "{text}"')
                w = 0
                if i < n:
                    c = text[i].upper()
                    if c in "ABC":
                        w = {"A": 3, "B": 2, "C": 1}[c]
                        i += 1
                    elif c in "D*":
                        i += 1
                entry.append((min(p, MAX_POS), w))
                if i < n and text[i] == ",":
                    i += 1
                    continue
                break
    return _normalise_vec(out)


def parse_tsvector(text: str) -> dict[str, Any]:
    return _vec_out(parse_vector(text))


def to_tsvector(text: str, config_name: Any = None) -> dict[str, Any]:
    cfg = config(config_name)
    v: Vec = {}
    for pos, lex in parse_document(text, cfg):
        if lex is not None:
            v.setdefault(lex, []).append((pos, 0))
    return _vec_out(v)


_TSVECTOR_ENTRY = r"'(?:[^']|'')*'(?::\d+[A-D]?(?:,\d+[A-D]?)*)?"
_CANONICAL_TSVECTOR_RE = re.compile(rf"^{_TSVECTOR_ENTRY}(?: {_TSVECTOR_ENTRY})*$")


def text_as_tsvector(text: str) -> dict[str, Any]:
    """A string on the document side of ``@@``: the canonical text of a stored
    tsvector (what the Rust server writes) is parsed; anything else is plain
    text, which ``text @@ tsquery`` indexes with ``to_tsvector``."""
    if _CANONICAL_TSVECTOR_RE.match(text):
        return parse_tsvector(text)
    return to_tsvector(text)


def setweight(v: Any, weight: Any, only: Any = None) -> dict[str, Any]:
    letter = str(weight)[:1].upper()
    if letter not in ("A", "B", "C", "D"):
        code = ord(str(weight)[:1]) if str(weight) else 0
        raise errors.SQLError("XX000", f"unrecognized weight: {code}")
    w = {"A": 3, "B": 2, "C": 1, "D": 0}[letter]
    keep = None if only is None else set(_strings(only))
    out = {
        lex: [(p, w) for p, _ in ps] if keep is None or lex in keep else ps
        for lex, ps in vec_of(v).items()
    }
    return _vec_out(out)


def _strings(items: Any) -> list[str]:
    if isinstance(items, (list, tuple)):
        out = []
        for x in items:
            if x is None:
                raise errors.SQLError("22004", "lexeme array may not contain nulls")
            out.append(str(x))
        return out
    s = str(items)
    if s.startswith("{") and s.endswith("}"):
        return [x.strip().strip('"') for x in s[1:-1].split(",") if x.strip().strip('"')]
    return [s]


def ts_delete(v: Any, lexemes: Any) -> dict[str, Any]:
    drop = set(_strings(lexemes))
    return _vec_out({k: ps for k, ps in vec_of(v).items() if k not in drop})


def ts_filter(v: Any, weights: Any) -> dict[str, Any]:
    keep = set()
    for w in _strings(weights):
        letter = w.upper()
        if letter not in ("A", "B", "C", "D"):
            raise errors.SQLError("XX000", f'unrecognized weight: "{w}"')
        keep.add({"A": 3, "B": 2, "C": 1, "D": 0}[letter])
    out: Vec = {}
    for lex, ps in vec_of(v).items():
        kept = [(p, w) for p, w in ps if w in keep]
        if kept:
            out[lex] = kept
    return _vec_out(out)


def strip_tsvector(v: Any) -> dict[str, Any]:
    return _vec_out({lex: [] for lex in vec_of(v)})


def tsvector_length(v: Any) -> int:
    return len(vec_of(v))


def tsvector_to_array(v: Any) -> list[str]:
    return list(vec_of(v))


def array_to_tsvector(items: Any) -> dict[str, Any]:
    out: Vec = {}
    for s in _strings(items or []):
        if s == "":
            raise errors.SQLError("2200F", "lexeme array may not contain empty strings")
        out[s] = []
    return _vec_out(out)


def tsvector_concat(a: Any, b: Any) -> dict[str, Any]:
    """``tsvector || tsvector``: the right side's positions shift past the
    left's largest."""
    left, right = vec_of(a), vec_of(b)
    shift = max((p for ps in left.values() for p, _ in ps), default=0)
    out: Vec = {lex: list(ps) for lex, ps in left.items()}
    for lex, ps in right.items():
        entry = out.setdefault(lex, [])
        entry.extend((min(p + shift, MAX_POS), w) for p, w in ps)
    return _vec_out(out)


# --------------------------------------------------------------------------- #
# tsquery
# --------------------------------------------------------------------------- #

_STOP: dict[str, Any] = {"stop": True}


def _node(q: Any) -> Any:
    """The bare tree of a stored tsquery (either server's shape)."""
    if isinstance(q, str):
        return parse_query(q)
    if isinstance(q, dict) and "tsquery" in q:
        return q["tsquery"]
    return q


def _binary(node: dict[str, Any]) -> tuple[str, Any, Any, int]:
    """``(kind, left, right, distance)`` of an and / or / phrase node; an
    n-ary and / or (an older stored shape) folds to the left."""
    if "phrase" in node:
        ph = node["phrase"]
        return "phrase", ph["left"], ph["right"], int(ph.get("distance", 1))
    kind = "and" if "and" in node else "or"
    items = list(node[kind])
    left = items[0]
    for nxt in items[1:-1]:
        left = {kind: [left, nxt]}
    return kind, left, items[-1] if len(items) > 1 else None, 0


def _mk(kind: str, left: Any, right: Any, distance: int = 1) -> dict[str, Any]:
    if kind == "phrase":
        return {"phrase": {"left": left, "right": right, "distance": distance}}
    return {kind: [left, right]}


def _priority(node: dict[str, Any]) -> int:
    if "or" in node:
        return 1
    if "and" in node:
        return 2
    if "phrase" in node:
        return 3
    if "not" in node:
        return 4
    return 5


def _leaf(node: dict[str, Any]) -> tuple[str, bool, int] | None:
    if "lexeme" in node:
        return node["lexeme"], False, int(node.get("weight", 0))
    if "prefix" in node:
        return node["prefix"], True, int(node.get("weight", 0))
    return None


def _render(node: Any, parent: int, right_phrase: bool, out: list[str]) -> None:
    if not isinstance(node, dict) or "stop" in node:
        return
    leaf = _leaf(node)
    if leaf is not None:
        lex, prefix, weight = leaf
        out.append(_quote(lex))
        if prefix or weight:
            out.append(":" + ("*" if prefix else ""))
            out.append(
                "".join(ch for bit, ch in ((8, "A"), (4, "B"), (2, "C"), (1, "D")) if weight & bit)
            )
        return
    p = _priority(node)
    if "not" in node:
        paren = p < parent
        out.append("( !" if paren else "!")
        _render(node["not"], p, False, out)
        if paren:
            out.append(" )")
        return
    kind, left, right, d = _binary(node)
    if right is None:
        _render(left, parent, right_phrase, out)
        return
    is_phrase = kind == "phrase"
    paren = p < parent or (is_phrase and right_phrase)
    if paren:
        out.append("( ")
    _render(left, p, False, out)
    if kind == "and":
        out.append(" & ")
    elif kind == "or":
        out.append(" | ")
    else:
        out.append(" <-> " if d == 1 else f" <{d}> ")
    _render(right, p, is_phrase, out)
    if paren:
        out.append(" )")


def render_query(node: Any) -> str:
    out: list[str] = []
    if node is not None:
        _render(node, 0, False, out)
    return "".join(out)


def render_tsquery(v: Any) -> str:
    return render_query(_node(v))


def _wrap_query(node: Any) -> dict[str, Any]:
    return {"tsquery": node}


class _QueryParser:
    """``tsquery_in`` / ``to_tsquery``: ``or := and ('|' and)*``,
    ``and := phrase ('&' phrase)*``, ``phrase := unary (('<->'|'<N>') unary)*``,
    ``unary := '!' unary | '(' or ')' | operand``."""

    def __init__(self, text: str, cfg: str | None) -> None:
        self.text = text
        self.i = 0
        self.cfg = cfg

    def ws(self) -> None:
        while self.i < len(self.text) and self.text[self.i].isspace():
            self.i += 1

    def peek(self) -> str | None:
        return self.text[self.i] if self.i < len(self.text) else None

    def parse_or(self) -> Any:
        left = self.parse_and()
        while True:
            self.ws()
            if self.peek() != "|":
                return left
            self.i += 1
            left = {"or": [left, self.parse_and()]}

    def parse_and(self) -> Any:
        left = self.parse_phrase()
        while True:
            self.ws()
            if self.peek() != "&":
                return left
            self.i += 1
            left = {"and": [left, self.parse_phrase()]}

    def phrase_op(self) -> int | None:
        self.ws()
        if self.peek() != "<":
            return None
        start = self.i
        self.i += 1
        if self.text[self.i : self.i + 2] == "->":
            self.i += 2
            return 1
        ds = self.i
        while self.peek() is not None and "0" <= self.peek() <= "9":  # type: ignore[operator]
            self.i += 1
        if self.i > ds and self.peek() == ">":
            d = int(self.text[ds : self.i])
            self.i += 1
            if d > 16384:
                raise errors.SQLError(
                    "22023",
                    "distance in phrase operator must be an integer value between zero "
                    "and 16384 inclusive",
                )
            return d
        self.i = start
        raise _syntax("tsquery", self.text)

    def parse_phrase(self) -> Any:
        left = self.parse_unary()
        while (d := self.phrase_op()) is not None:
            left = _mk("phrase", left, self.parse_unary(), d)
        return left

    def parse_unary(self) -> Any:
        self.ws()
        c = self.peek()
        if c is None:
            raise errors.SQLError("42601", f'no operand in tsquery: "{self.text}"')
        if c == "!":
            self.i += 1
            return {"not": self.parse_unary()}
        if c == "(":
            self.i += 1
            q = self.parse_or()
            self.ws()
            if self.peek() != ")":
                raise _syntax("tsquery", self.text)
            self.i += 1
            return q
        if c in "&|)<":
            raise _syntax("tsquery", self.text)
        return self.operand()

    def operand(self) -> Any:
        got = _read_word(self.text, self.i, "&|!()<:")
        if got is None:
            raise _syntax("tsquery", self.text)
        word, self.i = got
        prefix, weight = False, 0
        if self.peek() == ":":
            self.i += 1
            while (c := self.peek()) is not None:
                u = c.upper()
                if u == "*":
                    prefix = True
                elif u in "ABCD" and len(u) == 1:
                    weight |= {"A": 8, "B": 4, "C": 2, "D": 1}[u]
                else:
                    break
                self.i += 1
        if not word:
            raise _syntax("tsquery", self.text)
        if self.cfg is None:
            return _val(word, prefix, weight)
        return _morph(word, self.cfg, prefix, weight, True)


def _val(lex: str, prefix: bool, weight: int) -> dict[str, Any]:
    node: dict[str, Any] = {"prefix": lex} if prefix else {"lexeme": lex}
    if weight:
        node["weight"] = weight
    return node


def _morph(text: str, cfg: str, prefix: bool, weight: int, phrase: bool) -> Any:
    """An operand's text through the configuration: its lexemes joined by
    ``<->`` (or ``&``) at their position distances, or a stop-word placeholder
    when it has none -- PostgreSQL's ``pushval_morph``."""
    out: Any = None
    prev = 0
    for pos, lex in parse_document(text, cfg):
        if lex is None:
            continue
        v = _val(lex, prefix, weight)
        if out is None:
            out = v
        elif phrase:
            out = _mk("phrase", out, v, pos - prev)
        else:
            out = _mk("and", out, v)
        prev = pos
    return _STOP if out is None else out


def _clean_stop(node: Any) -> tuple[Any, int, int]:
    """Remove stop-word placeholders, widening the phrases around them --
    ``clean_stopword_intree``. ``(node, ladd, radd)``."""
    if not isinstance(node, dict) or "stop" in node:
        return None, 0, 0
    if _leaf(node) is not None:
        return node, 0, 0
    if "not" in node:
        inner, la, ra = _clean_stop(node["not"])
        return (None, 0, 0) if inner is None else ({"not": inner}, la, ra)
    kind, l_node, r_node, dist = _binary(node)
    if r_node is None:
        return _clean_stop(l_node)
    is_phrase = kind == "phrase"
    left, lladd, lradd = _clean_stop(l_node)
    right, rladd, rradd = _clean_stop(r_node)
    if left is None and right is None:
        add = lladd + dist + rradd if is_phrase else 0
        return None, add, add
    if left is None:
        return right, (lladd + dist + rladd if is_phrase else 0), rradd
    if right is None:
        return left, lladd, (lradd + dist + rradd if is_phrase else 0)
    if is_phrase:
        return _mk("phrase", left, right, dist + lradd + rladd), lladd, rradd
    return _mk(kind, left, right), 0, 0


def _finish(node: Any) -> Any:
    return _clean_stop(node)[0]


def _parse_with(text: str, cfg: str | None) -> Any:
    p = _QueryParser(text, cfg)
    p.ws()
    if p.i >= len(text):
        return None
    q = p.parse_or()
    p.ws()
    if p.i < len(text):
        raise _syntax("tsquery", text)
    return q if cfg is None else _finish(q)


def parse_query(text: str) -> Any:
    """``tsquery_in``: operands taken as written."""
    return _parse_with(text, None)


def parse_tsquery(text: str) -> dict[str, Any]:
    return _wrap_query(parse_query(text))


def to_tsquery(text: str, config_name: Any = None) -> dict[str, Any]:
    return _wrap_query(_parse_with(text, config(config_name)))


def plainto_tsquery(text: str, config_name: Any = None) -> dict[str, Any]:
    return _wrap_query(_finish(_morph(text, config(config_name), False, 0, False)))


def phraseto_tsquery(text: str, config_name: Any = None) -> dict[str, Any]:
    return _wrap_query(_finish(_morph(text, config(config_name), False, 0, True)))


def websearch_to_tsquery(text: str, config_name: Any = None) -> dict[str, Any]:
    """Words AND together, ``"..."`` is a phrase, a leading ``-`` negates, and
    the word ``or`` separates alternatives."""
    cfg = config(config_name)
    n = len(text)
    items: list[Any] = []
    i = 0
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
            continue
        negate = c == "-"
        j = i + 1 if negate else i
        if j < n and text[j] == '"':
            k = text.find('"', j + 1)
            k = n if k < 0 else k
            q = _morph(text[j + 1 : k], cfg, False, 0, True)
            i = min(k + 1, n)
            items.append({"not": q} if negate else q)
            continue
        k = j
        while k < n and not text[k].isspace() and text[k] != '"':
            k += 1
        word = text[j:k]
        i = max(k, i + 1)
        if not negate and word.lower() == "or":
            items.append("or")
            continue
        if not word:
            continue
        q = _morph(word, cfg, False, 0, True)
        if q is _STOP and not any(is_word_char(ch) for ch in word):
            continue
        items.append({"not": q} if negate else q)
    groups: list[list[Any]] = [[]]
    for it in items:
        if it == "or":
            if groups[-1]:
                groups.append([])
        else:
            groups[-1].append(it)
    out: Any = None
    for g in groups:
        if not g:
            continue
        acc = g[0]
        for nxt in g[1:]:
            acc = _mk("and", acc, nxt)
        out = acc if out is None else _mk("or", out, acc)
    return _wrap_query(None if out is None else _finish(out))


# --------------------------------------------------------------------------- #
# Matching
# --------------------------------------------------------------------------- #


def _weight_ok(mask: int, w: int) -> bool:
    return mask == 0 or bool(mask & (1 << w))


def _entries(v: Vec, lex: str, prefix: bool) -> list[str]:
    if prefix:
        return [k for k in v if k.startswith(lex)]
    return [lex] if lex in v else []


def _leaf_positions(v: Vec, lex: str, prefix: bool, weight: int) -> tuple[list[int], bool]:
    out: set[int] = set()
    present = False
    for k in _entries(v, lex, prefix):
        ps = v[k]
        if not ps:
            if weight == 0 or weight & 1:
                present = True
            continue
        for p, w in ps:
            if _weight_ok(weight, w):
                out.add(p)
                present = True
    return sorted(out), present


class _Hits:
    __slots__ = ("lossy", "negate", "positions", "width")

    def __init__(self, positions: list[int], width: int, negate: bool, lossy: bool) -> None:
        self.positions = positions
        self.width = width
        self.negate = negate
        self.lossy = lossy


def _phrase_hits(v: Vec, q: Any) -> _Hits:
    if not isinstance(q, dict) or "stop" in q:
        return _Hits([], 0, False, False)
    leaf = _leaf(q)
    if leaf is not None:
        positions, present = _leaf_positions(v, *leaf)
        return _Hits(positions, 0, False, present and not positions)
    if "not" in q:
        h = _phrase_hits(v, q["not"])
        return _Hits(h.positions, h.width, not h.negate, h.lossy)
    kind, l_node, r_node, d = _binary(q)
    if r_node is None:
        return _phrase_hits(v, l_node)
    a, b = _phrase_hits(v, l_node), _phrase_hits(v, r_node)
    lossy = a.lossy or b.lossy
    width = max(a.width, b.width)
    if kind == "or":
        if a.negate or b.negate:
            return _Hits([], width, True, lossy)
        return _Hits(sorted(set(a.positions) | set(b.positions)), width, False, lossy)
    if kind == "and":
        sa, sb = set(a.positions), set(b.positions)
        if a.negate and b.negate:
            return _Hits(sorted(sa | sb), width, True, lossy)
        if not a.negate and not b.negate:
            pos = sa & sb
        elif not a.negate:
            pos = sa - sb
        else:
            pos = sb - sa
        return _Hits(sorted(pos), width, False, lossy)
    width = a.width + d + b.width
    if lossy:
        return _Hits([], width, False, True)
    sa, sb = set(a.positions), set(b.positions)
    out: set[int] = set()
    if b.negate:
        if a.negate:
            return _Hits([], width, True, False)
        for lp in a.positions:
            r = lp + d + b.width
            if 0 < r <= MAX_POS and r not in sb:
                out.add(r)
    else:
        for rp in b.positions:
            end = rp - b.width - d
            left_ok = a.negate if end < 1 else ((end in sa) != a.negate)
            if left_ok:
                out.add(rp)
    return _Hits(sorted(out), width, False, False)


def eval_query(v: Vec, q: Any) -> bool:
    if not isinstance(q, dict) or "stop" in q:
        return False
    leaf = _leaf(q)
    if leaf is not None:
        return _leaf_positions(v, *leaf)[1]
    if "not" in q:
        return not eval_query(v, q["not"])
    kind, l_node, r_node, _d = _binary(q)
    if r_node is None:
        return eval_query(v, l_node)
    if kind == "and":
        return eval_query(v, l_node) and eval_query(v, r_node)
    if kind == "or":
        return eval_query(v, l_node) or eval_query(v, r_node)
    h = _phrase_hits(v, q)
    if h.lossy:
        return False
    return True if h.negate else bool(h.positions)


def matches(tsvector: Any, tsquery: Any) -> bool:
    """``tsvector @@ tsquery``."""
    node = _node(tsquery)
    return node is not None and eval_query(vec_of(tsvector), node)


# --------------------------------------------------------------------------- #
# Functions over tsquery
# --------------------------------------------------------------------------- #


def numnode(q: Any) -> int:
    def count(node: Any) -> int:
        if not isinstance(node, dict):
            return 0
        if _leaf(node) is not None or "stop" in node:
            return 1
        if "not" in node:
            return 1 + count(node["not"])
        _k, left, right, _d = _binary(node)
        if right is None:
            return count(left)
        return 1 + count(left) + count(right)

    return count(_node(q))


def querytree(q: Any) -> str:
    """The query with its negated parts removed, ``T`` when nothing indexable
    is left."""

    def clean(node: Any) -> Any:
        if not isinstance(node, dict) or "stop" in node or "not" in node:
            return None
        if _leaf(node) is not None:
            return node
        kind, left, right, d = _binary(node)
        if right is None:
            return clean(left)
        a, b = clean(left), clean(right)
        if kind == "or":
            return None if a is None or b is None else _mk("or", a, b)
        if a is None or b is None:
            return a if b is None else b
        return _mk(kind, a, b, d)

    node = _node(q)
    if node is None:
        return ""
    c = clean(node)
    return "T" if c is None else render_query(c)


def _combine(kind: str, a: Any, b: Any, distance: int = 1) -> dict[str, Any]:
    na, nb = _node(a), _node(b)
    if na is None:
        return _wrap_query(nb)
    if nb is None:
        return _wrap_query(na)
    return _wrap_query(_mk(kind, na, nb, distance))


def tsquery_and(a: Any, b: Any) -> dict[str, Any]:
    return _combine("and", a, b)


def tsquery_or(a: Any, b: Any) -> dict[str, Any]:
    return _combine("or", a, b)


def tsquery_phrase(a: Any, b: Any, distance: Any = 1) -> dict[str, Any]:
    d = 1 if distance is None else int(distance)
    if d < 0 or d > 16384:
        raise errors.SQLError(
            "22023",
            "distance in phrase operator must be an integer value between zero and 16384 inclusive",
        )
    return _combine("phrase", a, b, d)


def tsquery_not(a: Any) -> dict[str, Any]:
    node = _node(a)
    return _wrap_query(None if node is None else {"not": node})


def get_current_ts_config() -> str:
    return DEFAULT_CONFIG


def ts_rank(*args: Any) -> float:
    from .fts_rank import rank_call

    return rank_call(list(args), cover_density=False)


def ts_rank_cd(*args: Any) -> float:
    from .fts_rank import rank_call

    return rank_call(list(args), cover_density=True)


def ts_headline(*args: Any) -> str:
    from .fts_rank import headline_call

    return headline_call(list(args))
