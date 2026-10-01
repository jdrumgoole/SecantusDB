"""Full-text ranking (``ts_rank`` / ``ts_rank_cd``) and ``ts_headline``.

Ported from the Rust server's ``fts.rs``, which transcribes PostgreSQL's
``tsrank.c`` (``calc_rank`` and ``calc_rank_cd``) in its float4 arithmetic so
the last digits agree. Every float4 operation is rounded to single precision
here with ``_f32``; for one ``+ - * /`` or ``sqrt`` the round-trip through a
double is exact, so the results match C's.
"""

from __future__ import annotations

import math
import struct
from typing import Any

from . import errors
from .fts import Vec, _binary, _leaf, _node, eval_query, parse_query, vec_of
from .fts_lang import DEFAULT_CONFIG, config, is_word_char, normalise

_DEFAULT_WEIGHTS = (0.1, 0.2, 0.4, 1.0)


def _f32(x: float) -> float:
    return struct.unpack("f", struct.pack("f", x))[0]


def float4_value(x: float) -> float:
    """The shortest decimal that round-trips the float4, as ``float4out``
    prints it."""
    x = _f32(x)
    if x == 0 or not math.isfinite(x):
        return x
    for digits in range(1, 10):
        s = f"{x:.{digits}g}"
        if _f32(float(s)) == x:
            return float(s)
    return x


def _operands(q: Any) -> list[tuple[str, bool, int]]:
    """The distinct operands of a query, by lexeme bytes (one per lexeme)."""
    out: list[tuple[str, bool, int]] = []

    def walk(node: Any) -> None:
        if not isinstance(node, dict) or "stop" in node:
            return
        leaf = _leaf(node)
        if leaf is not None:
            out.append(leaf)
            return
        if "not" in node:
            walk(node["not"])
            return
        _k, left, right, _d = _binary(node)
        walk(left)
        if right is not None:
            walk(right)

    walk(q)
    seen: dict[str, tuple[str, bool, int]] = {}
    for item in sorted(out, key=lambda t: t[0].encode("utf-8")):
        seen.setdefault(item[0], item)
    return list(seen.values())


def _found(v: Vec, lex: str, prefix: bool) -> list[list[tuple[int, int]]]:
    if prefix:
        return [ps for k, ps in v.items() if k.startswith(lex)]
    return [v[lex]] if lex in v else []


def _or_null(ps: list[tuple[int, int]]) -> list[tuple[int, int]]:
    return ps if ps else [(0, 0)]


def _word_distance(w: int) -> float:
    if w > 100:
        return _f32(1e-30)
    return _f32(1.0 / (1.005 + 0.05 * math.exp(_f32(float(w)) / 1.5 - 2.0)))


def _rank_or(w: tuple[float, ...], v: Vec, q: Any) -> float:
    items = _operands(q)
    res = 0.0
    for lex, prefix, _ in items:
        for entry in _found(v, lex, prefix):
            post = _or_null(entry)
            resj = 0.0
            wjm = -1.0
            jm = 0
            for j, (_, wt) in enumerate(post):
                wp = w[wt]
                resj = _f32(resj + _f32(wp / _f32(float((j + 1) * (j + 1)))))
                if wp > wjm:
                    wjm, jm = wp, j
            inner = _f32(_f32(wjm + resj) - _f32(wjm / _f32(float((jm + 1) * (jm + 1)))))
            res = _f32(res + inner / 1.64493406685)
    if items:
        res = _f32(res / _f32(float(len(items))))
    return res


def _rank_and(w: tuple[float, ...], v: Vec, q: Any) -> float:
    items = _operands(q)
    if len(items) < 2:
        return _rank_or(w, v, q)
    pos: list[tuple[list[tuple[int, int]], bool] | None] = [None] * len(items)
    res = -1.0
    for i, (lex, prefix, _) in enumerate(items):
        for entry in _found(v, lex, prefix):
            inull = not entry
            post = _or_null(entry)
            pos[i] = (post, inull)
            for k in range(i):
                got = pos[k]
                if got is None:
                    continue
                ct, knull = got
                for lp, lw in post:
                    for pp, pw in ct:
                        dist = abs(lp - pp)
                        if dist != 0 or inull or knull:
                            if dist == 0:
                                dist = 16384
                            prod = _f32(_f32(w[lw] * w[pw]) * _word_distance(dist))
                            curw = _f32(math.sqrt(prod))
                            res = curw if res < 0 else _f32(1.0 - (1.0 - res) * (1.0 - curw))
    return res


def _is_and_or_phrase(q: Any) -> bool:
    return isinstance(q, dict) and ("and" in q or "phrase" in q)


def _lengths(v: Vec) -> tuple[int, int]:
    return sum(max(len(ps), 1) for ps in v.values()), len(v)


def rank(weights: tuple[float, ...] | None, v: Vec, q: Any, method: int) -> float:
    w = weights or tuple(_f32(x) for x in _DEFAULT_WEIGHTS)
    if q is None or not v:
        return 0.0
    res = _rank_and(w, v, q) if _is_and_or_phrase(q) else _rank_or(w, v, q)
    if res < 0:
        res = _f32(1e-20)
    length, uniq = _lengths(v)
    if method & 1:
        res = _f32(res / (math.log(length + 1.0) / math.log(2.0)))
    if method & 2 and length > 0:
        res = _f32(res / _f32(float(length)))
    if method & 8:
        res = _f32(res / _f32(float(uniq)))
    if method & 16:
        res = _f32(res / (math.log(uniq + 1.0) / math.log(2.0)))
    if method & 32:
        res = _f32(res / _f32(res + 1.0))
    return res


def _docrep(v: Vec, q: Any) -> list[tuple[int, int, list[str]]]:
    out: list[tuple[int, int, list[str]]] = []
    for lex, prefix, _ in _operands(q):
        keys = [k for k in v if k.startswith(lex)] if prefix else ([lex] if lex in v else [])
        for k in keys:
            for p, wt in v[k]:
                out.append((p, wt, [k]))
    out.sort(key=lambda d: d[0])
    merged: list[tuple[int, int, list[str]]] = []
    for d in out:
        if merged and merged[-1][0] == d[0]:
            for lex in d[2]:
                if lex not in merged[-1][2]:
                    merged[-1][2].append(lex)
        else:
            merged.append((d[0], d[1], list(d[2])))
    return merged


def _window_holds(doc: list[tuple[int, int, list[str]]], lo: int, hi: int, q: Any) -> bool:
    sub: Vec = {}
    for p, wt, lexes in doc[lo : hi + 1]:
        for lex in lexes:
            entry = sub.setdefault(lex, [])
            if p > 0:
                entry.append((p, wt))
    return eval_query({k: sorted(set(ps)) for k, ps in sorted(sub.items())}, q)


def rank_cd(weights: tuple[float, ...] | None, v: Vec, q: Any, method: int) -> float:
    w = weights or tuple(_f32(x) for x in _DEFAULT_WEIGHTS)
    invws = [1.0 / x for x in w]
    if q is None:
        return 0.0
    doc = _docrep(v, q)
    if not doc:
        return 0.0
    wdoc = sum_dist = prev_ext = 0.0
    n_extent = 0
    start = 0
    while start < len(doc):
        end = next((i for i in range(start, len(doc)) if _window_holds(doc, start, i, q)), None)
        if end is None:
            break
        begin = next((i for i in range(end, start - 1, -1) if _window_holds(doc, i, end, q)), None)
        if begin is None:
            break
        p, qpos = doc[begin][0], doc[end][0]
        if p > qpos:
            start += 1
            continue
        start = begin + 1
        inv_sum = sum(invws[d[1]] for d in doc[begin : end + 1])
        cpos = (end - begin + 1) / inv_sum
        noise = (qpos - p) - (end - begin)
        if noise < 0:
            noise = (end - begin) // 2
        wdoc += cpos / (1 + noise)
        cur = (qpos + p) / 2.0
        if n_extent > 0 and cur > prev_ext:
            sum_dist += 1.0 / (cur - prev_ext)
        prev_ext = cur
        n_extent += 1
    length, uniq = _lengths(v)
    if method & 1 and uniq > 0:
        wdoc /= math.log(length + 1.0)
    if method & 2 and length > 0:
        wdoc /= length
    if method & 4 and n_extent > 0 and sum_dist > 0:
        wdoc /= n_extent / sum_dist
    if method & 8 and uniq > 0:
        wdoc /= uniq
    if method & 16 and uniq > 0:
        wdoc /= math.log(uniq + 1.0) / math.log(2.0)
    if method & 32:
        wdoc /= wdoc + 1.0
    return _f32(wdoc)


def _weights_arg(value: Any) -> tuple[float, ...]:
    if isinstance(value, (list, tuple)):
        raw = list(value)
    elif isinstance(value, str):
        raw = [x for x in value.strip().strip("{}").split(",") if x.strip()]
    else:
        raise errors.SQLError("22023", "array of weight is too short")
    if len(raw) < 4:
        raise errors.SQLError("22023", "array of weight is too short")
    out = []
    for x in raw[:4]:
        if x is None:
            raise errors.SQLError("22004", "array of weight must not contain nulls")
        try:
            f = float(str(x).strip()) if not isinstance(x, (int, float)) else float(x)
        except ValueError:
            raise errors.SQLError("22P02", "invalid weight") from None
        if not 0.0 <= f <= 1.0:
            raise errors.SQLError("22023", "weight out of range")
        out.append(_f32(f))
    return tuple(out)


def _is_weights(value: Any) -> bool:
    return isinstance(value, (list, tuple)) or (isinstance(value, str) and value.startswith("{"))


def rank_call(args: list[Any], *, cover_density: bool) -> Any:
    """``ts_rank([weights,] vector, query [, normalization])``."""
    if any(a is None for a in args):
        return None
    weights = None
    if len(args) == 4 or (len(args) == 3 and _is_weights(args[0])):
        weights, args = _weights_arg(args[0]), args[1:]
    v = vec_of(args[0])
    q = _node(args[1])
    method = int(str(args[2]).strip()) if len(args) > 2 else 0
    r = (rank_cd if cover_density else rank)(weights, v, q, method)
    return float4_value(r)


def headline(cfg: str, doc: str, q: Any) -> str:
    """``ts_headline`` with the default options (``StartSel=<b>``,
    ``StopSel=</b>``, ``MaxWords=35``): a document of at most 35 words is
    returned whole with each matching word wrapped; a longer one is cut to the
    first window of 35 words that holds a match."""
    wanted: list[tuple[str, bool]] = []

    def collect(node: Any) -> None:
        if not isinstance(node, dict) or "stop" in node or "not" in node:
            return
        leaf = _leaf(node)
        if leaf is not None:
            wanted.append((leaf[0], leaf[1]))
            return
        _k, left, right, _d = _binary(node)
        collect(left)
        if right is not None:
            collect(right)

    collect(q)

    def hit(word: str) -> bool:
        lex = normalise(word, True, cfg)
        if lex is None:
            return False
        return any(lex.startswith(w) if prefix else lex == w for w, prefix in wanted)

    pieces: list[tuple[str, bool]] = []
    cur = ""
    in_word = False
    for c in doc:
        wc = is_word_char(c)
        if wc != in_word and cur:
            pieces.append((cur, in_word))
            cur = ""
        in_word = wc
        cur += c
    if cur:
        pieces.append((cur, in_word))
    words = sum(1 for _, w in pieces if w)
    lo, hi = 0, len(pieces)
    if words > 35:
        lo = next((k for k, (t, w) in enumerate(pieces) if w and hit(t)), 0)
        hi, count = lo, 0
        while hi < len(pieces) and count < 35:
            if pieces[hi][1]:
                count += 1
            hi += 1
    return "".join(f"<b>{t}</b>" if w and hit(t) else t for t, w in pieces[lo:hi])


def headline_call(args: list[Any]) -> Any:
    """``ts_headline([config,] document, query [, options])``."""
    if any(a is None for a in args[:3]):
        return None
    cfg = DEFAULT_CONFIG
    rest = args[:2]
    if len(args) >= 4:
        cfg, rest = config(args[0]), args[1:3]
    elif len(args) == 3:
        third = args[2]
        third_is_query = isinstance(third, dict) and "tsquery" in third
        if not third_is_query and isinstance(third, str):
            try:
                parse_query(third)
                third_is_query = True
            except errors.SQLError:
                third_is_query = False
        if third_is_query:
            cfg, rest = config(args[0]), args[1:3]
    doc, query = rest
    return headline(cfg, str(doc), _node(query))
