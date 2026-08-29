"""A compact SQL/JSON path evaluator — the subset of the ``jsonpath`` language
that ``jsonb_path_query`` / ``jsonb_path_exists`` / ``@?`` / ``@@`` need.

Supported: ``$`` (root), ``.key`` / ``."key"`` member access, ``[n]`` array index
(negative counts from the end), ``[*]`` all array elements, ``.*`` all members,
and a ``? (<predicate>)`` filter whose predicate is ``@`` / ``@.path`` compared
(``== != < <= > >=``) to a literal (number / string / true / false / null) or
combined with ``&&`` / ``||``. ``query(doc, path)`` returns the list of matched
values; ``exists`` and ``match`` build on it.

Out of scope (raises ``JsonPathError`` → the caller defers / errors): arithmetic,
functions (``.size()``), recursive ``**``, ``like_regex``, ``exists(...)`` inside
a predicate, and ``$var`` bindings.
"""

from __future__ import annotations

import re
from typing import Any


class JsonPathError(ValueError):
    """A path this evaluator doesn't support (surfaced to the SQL layer)."""


# -- tokenizer / parser ------------------------------------------------------- #

_TOKEN_RE = re.compile(
    r"""
      \s+                                   # whitespace (skipped)
    | (?P<root>\$)
    | (?P<current>@)
    | (?P<dot>\.)
    | (?P<star>\*)
    | (?P<lbrack>\[)
    | (?P<rbrack>\])
    | (?P<lparen>\()
    | (?P<rparen>\))
    | (?P<question>\?)
    | (?P<op><=|>=|==|!=|<|>)
    | (?P<and>&&)
    | (?P<or>\|\|)
    | (?P<num>-?\d+(?:\.\d+)?)
    | (?P<str>"(?:[^"\\]|\\.)*")
    | (?P<ident>[A-Za-z_][A-Za-z0-9_]*)
    """,
    re.VERBOSE,
)


def _tokenize(text: str) -> list[tuple[str, str]]:
    tokens, pos = [], 0
    while pos < len(text):
        m = _TOKEN_RE.match(text, pos)
        if m is None:
            raise JsonPathError(f"cannot tokenize jsonpath at: {text[pos:]!r}")
        pos = m.end()
        kind = m.lastgroup
        if kind is not None:  # None = whitespace-only match
            tokens.append((kind, m.group()))
    return tokens


def _unquote(text: str) -> str:
    return text[1:-1].replace('\\"', '"').replace("\\\\", "\\")


class _Parser:
    def __init__(self, tokens: list[tuple[str, str]]) -> None:
        self.tokens = tokens
        self.i = 0

    def _peek(self) -> tuple[str, str] | None:
        return self.tokens[self.i] if self.i < len(self.tokens) else None

    def _next(self) -> tuple[str, str]:
        tok = self._peek()
        if tok is None:
            raise JsonPathError("unexpected end of jsonpath")
        self.i += 1
        return tok

    def parse(self) -> list[dict]:
        steps: list[dict] = []
        tok = self._peek()
        if tok and tok[0] in ("root", "current"):
            self.i += 1
        while self._peek() is not None:
            steps.append(self._step())
        return steps

    def parse_predicate(self) -> dict:
        """A top-level boolean predicate (``@@`` / ``jsonb_path_match`` mode), e.g.
        ``$.a == 5`` — the whole path is a boolean expression, not a value path."""
        pred = self._predicate()
        if self._peek() is not None:
            raise JsonPathError("trailing tokens after jsonpath predicate")
        return pred

    def _step(self) -> dict:
        kind, val = self._next()
        if kind == "dot":
            nxt = self._next()
            if nxt[0] == "star":
                return {"op": "wild_member"}
            if nxt[0] == "ident":
                return {"op": "key", "key": nxt[1]}
            if nxt[0] == "str":
                return {"op": "key", "key": _unquote(nxt[1])}
            raise JsonPathError(f"unexpected token after '.': {nxt[1]!r}")
        if kind == "lbrack":
            inner = self._next()
            if inner[0] == "star":
                self._expect("rbrack")
                return {"op": "wild_array"}
            if inner[0] == "num":
                self._expect("rbrack")
                return {"op": "index", "index": int(inner[1])}
            if inner[0] == "str":
                self._expect("rbrack")
                return {"op": "key", "key": _unquote(inner[1])}
            raise JsonPathError(f"unsupported subscript: {inner[1]!r}")
        if kind == "question":
            self._expect("lparen")
            pred = self._predicate()
            self._expect("rparen")
            return {"op": "filter", "pred": pred}
        raise JsonPathError(f"unexpected jsonpath token: {val!r}")

    def _expect(self, kind: str) -> tuple[str, str]:
        tok = self._next()
        if tok[0] != kind:
            raise JsonPathError(f"expected {kind}, got {tok[1]!r}")
        return tok

    # predicate := term (( && | || ) term)*
    def _predicate(self) -> dict:
        node = self._term()
        while (tok := self._peek()) is not None and tok[0] in ("and", "or"):
            self.i += 1
            right = self._term()
            node = {"kind": tok[0], "left": node, "right": right}
        return node

    def _term(self) -> dict:
        tok = self._peek()
        if tok and tok[0] == "lparen":
            self.i += 1
            node = self._predicate()
            self._expect("rparen")
            return node
        left_path = self._operand_path()
        op_tok = self._next()
        if op_tok[0] != "op":
            raise JsonPathError(f"expected a comparison operator, got {op_tok[1]!r}")
        value = self._literal()
        return {"kind": "cmp", "path": left_path, "op": op_tok[1], "value": value}

    def _operand_path(self) -> list[dict]:
        # ``@`` (filter mode) or ``$`` (top-level predicate mode) optionally
        # followed by ``.key`` / ``[n]`` steps.
        if self._next()[0] not in ("current", "root"):
            raise JsonPathError("predicate operand must start with '@' or '$'")
        steps: list[dict] = []
        while (tok := self._peek()) is not None and tok[0] in ("dot", "lbrack"):
            steps.append(self._step())
        return steps

    def _literal(self) -> Any:
        kind, val = self._next()
        if kind == "num":
            return float(val) if ("." in val) else int(val)
        if kind == "str":
            return _unquote(val)
        if kind == "ident":
            low = val.lower()
            if low == "true":
                return True
            if low == "false":
                return False
            if low == "null":
                return None
        raise JsonPathError(f"unsupported filter literal: {val!r}")


def _parse(path: str) -> list[dict]:
    return _Parser(_tokenize(path)).parse()


# -- evaluation --------------------------------------------------------------- #


def _apply_steps(values: list[Any], steps: list[dict]) -> list[Any]:
    current = values
    for step in steps:
        nxt: list[Any] = []
        for v in current:
            nxt.extend(_apply_one(v, step))
        current = nxt
    return current


def _apply_one(value: Any, step: dict) -> list[Any]:
    op = step["op"]
    if op == "key":
        if isinstance(value, dict) and step["key"] in value:
            return [value[step["key"]]]
        return []
    if op == "index":
        if isinstance(value, list):
            idx = step["index"]
            if -len(value) <= idx < len(value):
                return [value[idx]]
        return []
    if op == "wild_array":
        return list(value) if isinstance(value, list) else []
    if op == "wild_member":
        return list(value.values()) if isinstance(value, dict) else []
    if op == "filter":
        return [value] if _eval_pred(step["pred"], value) else []
    raise JsonPathError(f"unsupported step: {op}")


def _eval_pred(pred: dict, value: Any) -> bool:
    kind = pred["kind"]
    if kind == "and":
        return _eval_pred(pred["left"], value) and _eval_pred(pred["right"], value)
    if kind == "or":
        return _eval_pred(pred["left"], value) or _eval_pred(pred["right"], value)
    # comparison
    matches = _apply_steps([value], pred["path"])
    op, target = pred["op"], pred["value"]
    return any(_compare(got, op, target) for got in matches)


def _compare(got: Any, op: str, target: Any) -> bool:
    if op == "==":
        return got == target
    if op == "!=":
        return got != target
    if isinstance(got, bool) or isinstance(target, bool) or got is None or target is None:
        return False  # ordering comparisons are only defined for numbers / strings
    try:
        if op == "<":
            return got < target
        if op == "<=":
            return got <= target
        if op == ">":
            return got > target
        if op == ">=":
            return got >= target
    except TypeError:
        return False
    return False


def canonicalize(text: str) -> str:
    """PG's canonical jsonpath text (jsonPathToCstring): member accessors are
    always quoted (``$.abc`` -> ``$."abc"``), subscripts verbatim, filters as
    ``?(@."k" == v)``. An empty path is a syntax error, like real PG."""
    s = text.strip()
    if not s:
        raise JsonPathError("syntax error at end of jsonpath input")
    return "$" + _render_steps(_parse(s))


def _render_steps(steps: list[dict]) -> str:
    out: list[str] = []
    for st in steps:
        op = st["op"]
        if op == "key":
            out.append('."' + str(st["key"]).replace('"', '\\"') + '"')
        elif op == "wild_member":
            out.append(".*")
        elif op == "index":
            out.append(f"[{st['index']}]")
        elif op == "wild_array":
            out.append("[*]")
        elif op == "filter":
            out.append(f"?({_render_pred(st['pred'])})")
    return "".join(out)


def _render_pred(p: dict) -> str:
    kind = p["kind"]
    if kind in ("and", "or"):
        op = "&&" if kind == "and" else "||"
        return f"{_render_pred(p['left'])} {op} {_render_pred(p['right'])}"
    val = p["value"]
    if val is None:
        lit = "null"
    elif isinstance(val, bool):
        lit = "true" if val else "false"
    elif isinstance(val, str):
        lit = '"' + val.replace('"', '\\"') + '"'
    elif isinstance(val, float) and val.is_integer():
        lit = str(int(val))
    else:
        lit = str(val)
    return f"@{_render_steps(p['path'])} {p['op']} {lit}"


def query(doc: Any, path: str) -> list[Any]:
    """All values in ``doc`` matched by ``path`` (the SQL ``jsonb_path_query`` set)."""
    return _apply_steps([doc], _parse(path))


def exists(doc: Any, path: str) -> bool:
    """``jsonb_path_exists`` / ``@?`` — does ``path`` match anything in ``doc``?"""
    return len(query(doc, path)) > 0


def match(doc: Any, path: str) -> bool:
    """``jsonb_path_match`` / ``@@`` — evaluate a top-level boolean predicate (e.g.
    ``$.a == 5``) against ``doc``."""
    pred = _Parser(_tokenize(path)).parse_predicate()
    return _eval_pred(pred, doc)
