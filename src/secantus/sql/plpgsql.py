"""A compact interpreter for ``LANGUAGE plpgsql`` scalar function bodies.

PostgreSQL's PL/pgSQL is a full procedural language; sqlglot does not parse it
(a plpgsql body arrives as an opaque dollar-quoted string). This module tokenises
and interprets the *scalar-function* subset that the vast majority of hand-written
and ORM-/migration-generated functions actually use:

- ``[ DECLARE decl ; … ] BEGIN stmt ; … END`` blocks (nestable);
- variable declarations ``name type [ := expr ]`` (the type is used only for an
  optional result coercion — variables are dynamically typed);
- assignment ``name := expr`` (also accepts ``name = expr``);
- ``IF cond THEN … [ ELSIF cond THEN … ] [ ELSE … ] END IF``;
- ``RETURN expr`` (and a bare ``RETURN`` / ``RETURN NULL``);
- embedded SQL: ``SELECT … INTO var[, …] …`` (assigns the query's first row to the
  targets), ``PERFORM query`` (runs a query for its side effects), and bare
  ``INSERT`` / ``UPDATE`` / ``DELETE`` statements;
- ``NULL ;`` no-op;
- refcursors: ``OPEN <cursor> FOR <query>`` (materializes into a session
  cursor named like PG's unnamed portals) and ``CLOSE <cursor>``.

A bare identifier in an expression or embedded query that matches a declared
variable or a function parameter resolves to that value; everything else is left
for the ordinary SQL machinery to resolve (table columns, functions, subqueries).

**Out of scope** (raises ``feature_not_supported`` / ``0A000``): loops
(``LOOP`` / ``WHILE`` / ``FOR``), ``RETURN QUERY`` / ``RETURN NEXT``
(set-returning functions), ``CASE`` statements, ``OPEN … FOR EXECUTE``,
exception handlers (``EXCEPTION WHEN``), and dynamic ``EXECUTE``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import errors, scalar, typemap
from secantus.sql import planner as _planner

# --------------------------------------------------------------------------- #
# Tokeniser
# --------------------------------------------------------------------------- #


@dataclass
class _Tok:
    kind: str  # 'word' | 'sym' | 'str' | 'param' | 'semi'
    val: str
    lo: int
    hi: int


_WORD_START = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"
_WORD_REST = _WORD_START + "0123456789$"


def _tokenize(s: str) -> list[_Tok]:
    toks: list[_Tok] = []
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c in " \t\r\n":
            i += 1
            continue
        # line comment
        if c == "-" and i + 1 < n and s[i + 1] == "-":
            j = s.find("\n", i)
            i = n if j < 0 else j + 1
            continue
        # block comment
        if c == "/" and i + 1 < n and s[i + 1] == "*":
            j = s.find("*/", i + 2)
            i = n if j < 0 else j + 2
            continue
        # string literal (with '' escaping)
        if c == "'":
            j = i + 1
            while j < n:
                if s[j] == "'":
                    if j + 1 < n and s[j + 1] == "'":
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            toks.append(_Tok("str", s[i:j], i, j))
            i = j
            continue
        # dollar-quoted string  $tag$ … $tag$  (tag may be empty)
        if c == "$":
            k = i + 1
            while k < n and s[k] in _WORD_REST:
                k += 1
            if k < n and s[k] == "$":  # a $tag$ open
                tag = s[i : k + 1]
                end = s.find(tag, k + 1)
                j = n if end < 0 else end + len(tag)
                toks.append(_Tok("str", s[i:j], i, j))
                i = j
                continue
            if i + 1 < n and s[i + 1].isdigit():  # positional param $N
                j = i + 1
                while j < n and s[j].isdigit():
                    j += 1
                toks.append(_Tok("param", s[i:j], i, j))
                i = j
                continue
        # := assignment
        if c == ":" and i + 1 < n and s[i + 1] == "=":
            toks.append(_Tok("sym", ":=", i, i + 2))
            i += 2
            continue
        if c == ";":
            toks.append(_Tok("semi", ";", i, i + 1))
            i += 1
            continue
        if c in _WORD_START:
            j = i + 1
            while j < n and s[j] in _WORD_REST:
                j += 1
            toks.append(_Tok("word", s[i:j], i, j))
            i = j
            continue
        # any other single character is a symbol (operators, parens, commas, .)
        toks.append(_Tok("sym", c, i, i + 1))
        i += 1
    return toks


# --------------------------------------------------------------------------- #
# AST
# --------------------------------------------------------------------------- #


@dataclass
class Decl:
    name: str
    type_tag: str | None
    init: str | None  # raw expression source, or None


@dataclass
class Assign:
    name: str
    expr: str


@dataclass
class Return:
    expr: str | None  # None == RETURN / RETURN NULL


@dataclass
class If:
    # each branch is (condition source, body statements); else_body may be empty
    branches: list[tuple[str, list[Any]]]
    else_body: list[Any] = field(default_factory=list)


@dataclass
class SqlInto:
    query: str  # SELECT text with the INTO clause removed
    targets: list[str]


@dataclass
class Raise:
    level: str  # NOTICE / WARNING / INFO / LOG / DEBUG / EXCEPTION
    template: str  # plpgsql format string ('%' = next argument)
    arg_exprs: list[str]


@dataclass
class OpenCursor:
    """``OPEN <var> FOR <query>`` — materialize the query into a server-side
    cursor and bind the variable to its generated name."""

    var: str
    query: str


@dataclass
class CloseCursor:
    var: str


@dataclass
class SqlExec:
    query: str  # a bare INSERT/UPDATE/DELETE/SELECT run for side effects


@dataclass
class Block:
    decls: list[Decl]
    body: list[Any]


# --------------------------------------------------------------------------- #
# Parser
# --------------------------------------------------------------------------- #


class _Parser:
    def __init__(self, toks: list[_Tok], src: str):
        self.t = toks
        self.src = src
        self.i = 0

    def _peek(self, off: int = 0) -> _Tok | None:
        j = self.i + off
        return self.t[j] if 0 <= j < len(self.t) else None

    def _is_kw(self, tok: _Tok | None, *words: str) -> bool:
        return tok is not None and tok.kind == "word" and tok.val.lower() in words

    def _raw(self, lo_tok: int, hi_tok: int) -> str:
        """Source text spanning tokens [lo_tok, hi_tok)."""
        if lo_tok >= hi_tok:
            return ""
        return self.src[self.t[lo_tok].lo : self.t[hi_tok - 1].hi]

    def parse(self) -> Block:
        # Optional leading  <<label>>  is skipped by the tokeniser producing syms;
        # accept an optional DECLARE section, then a BEGIN..END block.
        decls: list[Decl] = []
        if self._is_kw(self._peek(), "declare"):
            self.i += 1
            decls = self._parse_declarations()
        if not self._is_kw(self._peek(), "begin"):
            raise errors.feature_not_supported("plpgsql body must contain a BEGIN … END block")
        self.i += 1
        body = self._parse_statements(("end",))
        self._expect_kw("end")
        return Block(decls, body)

    def _expect_kw(self, word: str) -> None:
        if not self._is_kw(self._peek(), word):
            got = self._peek()
            raise errors.feature_not_supported(
                f"plpgsql: expected {word.upper()}, got {got.val if got else '<eof>'!r}"
            )
        self.i += 1

    def _parse_declarations(self) -> list[Decl]:
        decls: list[Decl] = []
        while True:
            tok = self._peek()
            if tok is None or self._is_kw(tok, "begin"):
                break
            if tok.kind != "word":
                raise errors.feature_not_supported(f"plpgsql declaration: unexpected {tok.val!r}")
            name = tok.val
            self.i += 1
            # type text runs up to  :=  or  ;
            type_lo = self.i
            init: str | None = None
            while True:
                cur = self._peek()
                if cur is None:
                    raise errors.syntax_error("plpgsql: unterminated declaration")
                if cur.kind == "semi":
                    type_hi = self.i
                    self.i += 1
                    break
                if cur.kind == "sym" and cur.val == ":=":
                    type_hi = self.i
                    self.i += 1
                    init_lo = self.i
                    self._skip_to_semi()
                    init = self._raw(init_lo, self.i)
                    self.i += 1  # consume ;
                    break
                self.i += 1
            type_text = self._raw(type_lo, type_hi).strip()
            decls.append(Decl(name, _type_tag(type_text), init))
        return decls

    def _skip_to_semi(self) -> None:
        """Advance to (but not past) the next top-level ``;`` (paren-aware)."""
        depth = 0
        while True:
            cur = self._peek()
            if cur is None:
                raise errors.syntax_error("plpgsql: missing ';'")
            if cur.kind == "sym" and cur.val == "(":
                depth += 1
            elif cur.kind == "sym" and cur.val == ")":
                depth -= 1
            elif cur.kind == "semi" and depth == 0:
                return
            self.i += 1

    def _parse_statements(self, terminators: tuple[str, ...]) -> list[Any]:
        stmts: list[Any] = []
        while True:
            tok = self._peek()
            if tok is None:
                raise errors.syntax_error("plpgsql: unexpected end of body")
            if self._is_kw(tok, *terminators):
                return stmts
            stmts.append(self._parse_statement())

    def _parse_statement(self) -> Any:
        tok = self._peek()
        assert tok is not None
        if self._is_kw(tok, "if"):
            return self._parse_if()
        if self._is_kw(tok, "return"):
            return self._parse_return()
        if self._is_kw(tok, "begin"):  # a nested block used as a statement
            self.i += 1
            body = self._parse_statements(("end",))
            self._expect_kw("end")
            self._consume_optional_semi()
            return Block([], body)
        if self._is_kw(tok, "null") and self._peek(1) and self._peek(1).kind == "semi":
            self.i += 2
            return SqlExec("")  # no-op
        if self._is_kw(tok, "perform"):
            self.i += 1
            lo = self.i
            self._skip_to_semi()
            query = "SELECT " + self._raw(lo, self.i)
            self.i += 1
            return SqlExec(query)
        if self._is_kw(tok, "raise"):
            return self._parse_raise()
        if self._is_kw(tok, "commit", "rollback"):
            # Transaction control inside a procedure. In the CALL's autocommit
            # context there is no in-flight block to end, so this is a no-op that
            # lets execution continue (a data-changing procedure that relies on a
            # mid-body COMMIT/ROLLBACK boundary is a documented simplification).
            self.i += 1
            self._consume_optional_semi()
            return SqlExec("")
        if self._is_kw(tok, "open"):
            # OPEN <var> FOR <query>;  — bind a refcursor variable to a
            # materialized server-side cursor. (The parameterized
            # ``OPEN c FOR EXECUTE`` form is not supported.)
            var_tok = self._peek(1)
            for_tok = self._peek(2)
            if var_tok is None or var_tok.kind != "word" or not self._is_kw(for_tok, "for"):
                raise errors.feature_not_supported(
                    "plpgsql OPEN supports only the OPEN <cursor> FOR <query> form"
                )
            self.i += 3
            lo = self.i
            self._skip_to_semi()
            query = self._raw(lo, self.i)
            self.i += 1
            return OpenCursor(var_tok.val, query)
        if self._is_kw(tok, "close"):
            var_tok = self._peek(1)
            if var_tok is None or var_tok.kind != "word":
                raise errors.feature_not_supported("plpgsql CLOSE requires a cursor variable")
            self.i += 2
            self._consume_optional_semi()
            return CloseCursor(var_tok.val)
        if self._is_kw(tok, "loop", "while", "for", "case", "execute", "foreach"):
            raise errors.feature_not_supported(
                f"plpgsql statement {tok.val.upper()} is not supported"
            )
        # assignment:  name := expr   (or  name = expr)
        if tok.kind == "word":
            nxt = self._peek(1)
            if nxt is not None and (nxt.kind == "sym" and nxt.val in (":=", "=")):
                name = tok.val
                self.i += 2
                lo = self.i
                self._skip_to_semi()
                expr = self._raw(lo, self.i)
                self.i += 1
                return Assign(name, expr)
            # record-field assignment:  new.field := expr  (a trigger function
            # mutating its NEW row).
            dot, fld, op = self._peek(1), self._peek(2), self._peek(3)
            if (
                dot is not None
                and dot.kind == "sym"
                and dot.val == "."
                and fld is not None
                and fld.kind == "word"
                and op is not None
                and op.kind == "sym"
                and op.val in (":=", "=")
            ):
                name = f"{tok.val}.{fld.val}"
                self.i += 4
                lo = self.i
                self._skip_to_semi()
                expr = self._raw(lo, self.i)
                self.i += 1
                return Assign(name, expr)
        if self._is_kw(tok, "select"):
            return self._parse_select_stmt()
        if self._is_kw(tok, "insert", "update", "delete", "with"):
            lo = self.i
            self._skip_to_semi()
            query = self._raw(lo, self.i)
            self.i += 1
            return SqlExec(query)
        raise errors.feature_not_supported(
            f"plpgsql statement starting with {tok.val!r} is not supported"
        )

    def _parse_raise(self) -> Raise:
        """``RAISE [level] 'format' [, expr]* ;`` — bare ``RAISE 'x'`` is an
        EXCEPTION, exactly PG's default level."""
        self.i += 1  # RAISE
        level = "exception"
        tok = self._peek()
        if (
            tok is not None
            and tok.kind == "word"
            and tok.val.lower() in ("debug", "log", "info", "notice", "warning", "exception")
        ):
            level = tok.val.lower()
            self.i += 1
        tok = self._peek()
        if tok is None or tok.kind != "str":
            raise errors.feature_not_supported("RAISE requires a string format in this interpreter")
        template = tok.val
        # The tokenizer keeps the literal's surrounding quotes and doubled
        # inner quotes; RAISE wants the decoded text.
        if len(template) >= 2 and template[0] == "'" and template[-1] == "'":
            template = template[1:-1].replace("''", "'")
        self.i += 1
        arg_exprs: list[str] = []
        while True:
            tok = self._peek()
            if tok is not None and tok.kind == "sym" and tok.val == ",":
                self.i += 1
                lo = self.i
                # one expression: up to the next top-level comma or semicolon
                depth = 0
                while True:
                    cur = self._peek()
                    if cur is None:
                        break
                    if cur.kind == "sym" and cur.val == "(":
                        depth += 1
                    elif cur.kind == "sym" and cur.val == ")":
                        depth -= 1
                    elif depth == 0 and (
                        cur.kind == "semi" or (cur.kind == "sym" and cur.val == ",")
                    ):
                        break
                    self.i += 1
                arg_exprs.append(self._raw(lo, self.i))
                continue
            break
        self._consume_optional_semi()
        return Raise(level.upper(), template, arg_exprs)

    def _consume_optional_semi(self) -> None:
        cur = self._peek()
        if cur is not None and cur.kind == "semi":
            self.i += 1

    def _parse_if(self) -> If:
        self.i += 1  # IF
        branches: list[tuple[str, list[Any]]] = []
        cond = self._read_until_kw(("then",))
        self._expect_kw("then")
        body = self._parse_statements(("elsif", "elseif", "else", "end"))
        branches.append((cond, body))
        else_body: list[Any] = []
        while True:
            nxt = self._peek()
            if self._is_kw(nxt, "elsif", "elseif"):
                self.i += 1
                c = self._read_until_kw(("then",))
                self._expect_kw("then")
                b = self._parse_statements(("elsif", "elseif", "else", "end"))
                branches.append((c, b))
                continue
            if self._is_kw(nxt, "else"):
                self.i += 1
                else_body = self._parse_statements(("end",))
            break
        self._expect_kw("end")
        self._expect_kw("if")
        self._consume_optional_semi()
        return If(branches, else_body)

    def _read_until_kw(self, words: tuple[str, ...]) -> str:
        lo = self.i
        depth = 0
        while True:
            cur = self._peek()
            if cur is None:
                raise errors.syntax_error(f"plpgsql: expected {words[0].upper()}")
            if cur.kind == "sym" and cur.val == "(":
                depth += 1
            elif cur.kind == "sym" and cur.val == ")":
                depth -= 1
            elif depth == 0 and self._is_kw(cur, *words):
                return self._raw(lo, self.i)
            self.i += 1

    def _parse_return(self) -> Return:
        self.i += 1  # RETURN
        nxt = self._peek()
        if self._is_kw(nxt, "query", "next"):
            raise errors.feature_not_supported(
                f"plpgsql RETURN {nxt.val.upper()} (set-returning) is not supported"
            )
        if nxt is not None and nxt.kind == "semi":
            self.i += 1
            return Return(None)
        lo = self.i
        self._skip_to_semi()
        expr = self._raw(lo, self.i)
        self.i += 1
        if expr.strip().lower() == "null":
            return Return(None)
        return Return(expr)

    def _parse_select_stmt(self) -> Any:
        """A SELECT statement, splitting off an ``INTO target[, …]`` clause."""
        start = self.i
        self.i += 1  # SELECT
        # Find a top-level INTO before FROM/WHERE/etc.
        depth = 0
        into_at = -1
        scan = self.i
        while scan < len(self.t):
            cur = self.t[scan]
            if cur.kind == "sym" and cur.val == "(":
                depth += 1
            elif cur.kind == "sym" and cur.val == ")":
                depth -= 1
            elif cur.kind == "semi" and depth == 0:
                break
            elif depth == 0 and cur.kind == "word" and cur.val.lower() == "into":
                into_at = scan
                break
            elif depth == 0 and cur.kind == "word" and cur.val.lower() in ("from", "where"):
                break
            scan += 1
        if into_at < 0:
            # plain SELECT run for side effects (discard result)
            self._skip_to_semi()
            query = self._raw(start, self.i)
            self.i += 1
            return SqlExec(query)
        # capture targets after INTO up to FROM / ; / next clause
        select_list = self._raw(self.i, into_at)  # between SELECT and INTO
        j = into_at + 1
        targets: list[str] = []
        while j < len(self.t):
            cur = self.t[j]
            if cur.kind == "word" and cur.val.lower() in ("from", "where"):
                break
            if cur.kind == "semi":
                break
            if cur.kind == "word":
                targets.append(cur.val)
            j += 1
        rest = ""
        if j < len(self.t) and self.t[j].kind != "semi":
            # slice from the FROM/WHERE token to the terminating ;
            self.i = j
            self._skip_to_semi()
            rest = self._raw(j, self.i)
            self.i += 1
        else:
            self.i = j
            self._consume_optional_semi()
        query = f"SELECT {select_list} {rest}".strip()
        return SqlInto(query, targets)


def _type_tag(type_text: str) -> str | None:
    if not type_text:
        return None
    try:
        parsed = sqlglot.parse_one(f"SELECT CAST(NULL AS {type_text})", read="postgres")
        cast = parsed.selects[0]
        if isinstance(cast, exp.Cast):
            return typemap.type_tag_for_sql(cast.to)
    except Exception:  # noqa: BLE001 — unknown type text just means no coercion
        return None
    return None


def parse(body: str) -> Block:
    """Tokenise and parse a plpgsql function body into a :class:`Block`."""
    return _Parser(_tokenize(body), body).parse()


# --------------------------------------------------------------------------- #
# Interpreter
# --------------------------------------------------------------------------- #


class _ReturnSignal(Exception):
    def __init__(self, value: Any):
        self.value = value


class _Runner:
    def __init__(self, ctx: scalar.ScalarContext, args: list[Any], func: dict):
        self.ctx = ctx
        self.args = args
        self.func = func
        # var name (lower) -> declared type tag, for assignment coercion
        self.types: dict[str, str | None] = {}

    def run(self, block: Block, env: dict[str, Any]) -> Any:
        try:
            self._run_block(block, env)
        except _ReturnSignal as sig:
            return sig.value
        return None

    def _run_block(self, block: Block, env: dict[str, Any]) -> None:
        for d in block.decls:
            self.types[d.name.lower()] = d.type_tag
            val = None if d.init is None else self._eval(d.init, env)
            env[d.name.lower()] = self._coerce(val, d.type_tag)
        self._run_stmts(block.body, env)

    def _run_stmts(self, stmts: list[Any], env: dict[str, Any]) -> None:
        for st in stmts:
            self._run_stmt(st, env)

    def _run_stmt(self, st: Any, env: dict[str, Any]) -> None:
        if isinstance(st, Assign):
            if "." in st.name:
                # ``new.field := expr`` — mutate a record variable's field (a
                # BEFORE ROW trigger shaping its NEW row).
                base, _, fld = st.name.lower().partition(".")
                record = env.get(base)
                if not isinstance(record, dict):
                    raise errors.SQLError("42703", f'"{st.name}" is not a known variable')
                record[fld] = self._eval(st.expr, env)
                return
            if st.name.lower() not in env and st.name.lower() not in self.types:
                raise errors.SQLError("42703", f'"{st.name}" is not a known variable')
            val = self._eval(st.expr, env)
            env[st.name.lower()] = self._coerce(val, self.types.get(st.name.lower()))
            return
        if isinstance(st, Return):
            raise _ReturnSignal(None if st.expr is None else self._eval(st.expr, env))
        if isinstance(st, If):
            for cond, body in st.branches:
                if _truthy(self._eval(cond, env)):
                    self._run_stmts(body, env)
                    return
            self._run_stmts(st.else_body, env)
            return
        if isinstance(st, Block):
            self._run_block(st, env)
            return
        if isinstance(st, SqlInto):
            self._run_into(st, env)
            return
        if isinstance(st, Raise):
            parts = st.template.split("%")
            vals = [self._eval(e, env) for e in st.arg_exprs]
            msg = parts[0]
            for i, part in enumerate(parts[1:]):
                v = vals[i] if i < len(vals) else ""
                msg += ("" if v is None else str(v)) + part
            if st.level == "EXCEPTION":
                raise errors.SQLError("P0001", msg)
            # Side-channel to the enclosing statement's SQLResult: the engine
            # drains ``session.plpgsql_notices`` into ``result.notices`` after
            # each statement, and the wire layer emits NoticeResponse from
            # there (pgjdbc's testRaiseNotice reads them via getWarnings()).
            session = getattr(self.ctx, "session", None)
            if session is not None:
                if not hasattr(session, "plpgsql_notices"):
                    session.plpgsql_notices = []
                session.plpgsql_notices.append((st.level, msg))
            return
        if isinstance(st, SqlExec):
            if st.query.strip():
                self._run_sql(st.query, env)
            return
        if isinstance(st, OpenCursor):
            self._open_cursor(st, env)
            return
        if isinstance(st, CloseCursor):
            session = getattr(self.ctx, "session", None)
            name = env.get(st.var.lower())
            if session is None or not isinstance(name, str) or name not in session.cursors:
                raise errors.SQLError("34000", f'cursor "{st.var}" does not exist')
            del session.cursors[name]
            return
        raise errors.feature_not_supported(f"plpgsql: cannot execute {type(st).__name__}")

    def _open_cursor(self, st: OpenCursor, env: dict[str, Any]) -> None:
        """Materialize ``OPEN <var> FOR <query>`` into a session cursor named
        like PG's unnamed portals and bind the variable to that name — the
        caller FETCHes from it by name (pgjdbc's refcursor round-trip)."""
        from secantus.sql import engine as _engine

        c = self.ctx
        session = getattr(c, "session", None)
        if session is None:
            raise errors.feature_not_supported("plpgsql OPEN needs a session")
        seq = getattr(session, "refcursor_seq", 0) + 1
        session.refcursor_seq = seq
        name = f"<unnamed portal {seq}>"
        stmt = sqlglot.parse_one(st.query, read="postgres")
        stmt = self._inline(stmt, env)
        _engine.materialize_cursor(
            name,
            stmt,
            c.storage,
            c.db,
            c.catalog,
            session,
            statement=f"OPEN {st.var} FOR {st.query}",
        )
        env[st.var.lower()] = name

    # -- expression + embedded-SQL evaluation -------------------------------- #

    def _eval(self, src: str, env: dict[str, Any]) -> Any:
        sel = sqlglot.parse_one(f"SELECT {src}", read="postgres")
        if not isinstance(sel, exp.Select) or not sel.selects:
            raise errors.syntax_error(f"plpgsql: bad expression {src!r}")
        node = self._sub_params(sel.selects[0])
        return scalar.evaluate(node, self._scope(env), self.ctx)

    def _scope(self, env: dict[str, Any]):
        def scope(col: Any) -> Any:
            if isinstance(col, exp.Column):
                if col.table:
                    # ``new.t`` — a record variable's field (trigger NEW row).
                    record = env.get(col.table.lower())
                    if isinstance(record, dict):
                        key = col.name.lower()
                        if key in record:
                            return record[key]
                else:
                    nm = col.name.lower()
                    if nm in env:
                        return env[nm]
            raise errors.SQLError("42703", f'column "{getattr(col, "name", col)}" does not exist')

        return scope

    def _sub_params(self, node: exp.Expression) -> exp.Expression:
        node = node.copy()
        for p in list(node.find_all(exp.Parameter)):
            name = str(p.name)
            if name.isdigit():
                idx = int(name) - 1
                if 0 <= idx < len(self.args):
                    p.replace(_planner._value_to_node(self.args[idx]))
        return node

    def _inline(self, node: exp.Expression, env: dict[str, Any]) -> exp.Expression:
        """Replace ``$N`` and unqualified variable columns with literal nodes so an
        embedded SQL statement can run through the ordinary engine path."""
        node = self._sub_params(node)
        for c in list(node.find_all(exp.Column)):
            if c.table:
                continue
            nm = c.name.lower()
            if nm in env:
                c.replace(_planner._value_to_node(env[nm]))
        return node

    def _run_into(self, st: SqlInto, env: dict[str, Any]) -> None:
        from secantus.sql import engine as _engine

        c = self.ctx
        stmt = sqlglot.parse_one(st.query, read="postgres")
        stmt = self._inline(stmt, env)
        result = _engine.run_inner_select(stmt, c.storage, c.db, c.catalog, c.session)
        row = result.rows[0] if result.rows else None
        for i, target in enumerate(st.targets):
            tv = row[i] if (row is not None and i < len(row)) else None
            env[target.lower()] = self._coerce(tv, self.types.get(target.lower()))

    def _run_sql(self, query: str, env: dict[str, Any]) -> None:
        from secantus.sql import engine as _engine

        c = self.ctx
        stmt = sqlglot.parse_one(query, read="postgres")
        stmt = self._inline(stmt, env)
        _engine._run_statement(stmt, c.storage, c.db, c.catalog, c.session)

    def _coerce(self, value: Any, tag: str | None) -> Any:
        if tag is None or value is None:
            return value
        import contextlib

        with contextlib.suppress(errors.SQLError, ValueError, TypeError):
            return typemap.coerce(value, tag)
        return value


def _truthy(value: Any) -> bool:
    """plpgsql IF: NULL is false; otherwise Python truthiness of the boolean."""
    if value is None:
        return False
    return bool(value)


def invoke_trigger(func: dict, new_record: dict, ctx: scalar.ScalarContext) -> dict | None:
    """Run a ``RETURNS trigger`` plpgsql function for a BEFORE ROW event.

    ``new_record`` is the row as a column-name-keyed dict, bound to the
    function's ``NEW`` variable — the body may read fields (``new.t``), assign
    them (``new.ts := …``), and ``RETURN NEW``. Returns the (possibly mutated)
    record, or None when the function returned NULL — PG's "skip this row"."""
    block = parse(func["body"])
    runner = _Runner(ctx, [], func)
    env: dict[str, Any] = {"new": dict(new_record)}
    result = runner.run(block, env)
    if result is None:
        return None
    if not isinstance(result, dict):
        raise errors.SQLError("42804", "trigger function must return NEW or NULL")
    return result


def invoke(func: dict, args: list[Any], ctx: scalar.ScalarContext) -> Any:
    """Run a ``LANGUAGE plpgsql`` function and return its scalar result."""
    block = parse(func["body"])
    env: dict[str, Any] = {}
    runner = _Runner(ctx, args, func)
    params = func.get("params") or []
    for i, name in enumerate(params):
        if name is not None and i < len(args):
            env[str(name).lower()] = args[i]
    result = runner.run(block, env)
    return_tag = func.get("return_tag")
    if return_tag and result is not None:
        import contextlib

        with contextlib.suppress(errors.SQLError, ValueError, TypeError):
            result = typemap.coerce(result, return_tag)
    return result


def invoke_procedure(func: dict, args: list[Any], ctx: scalar.ScalarContext) -> dict[str, Any]:
    """Run a ``LANGUAGE plpgsql`` PROCEDURE body and return the final variable
    environment — the caller reads the OUT / INOUT parameter values from it to
    build the CALL result row. A procedure has no RETURN value."""
    block = parse(func["body"])
    env: dict[str, Any] = {}
    runner = _Runner(ctx, args, func)
    params = func.get("params") or []
    for i, name in enumerate(params):
        if name is not None and i < len(args):
            env[str(name).lower()] = args[i]
    runner.run(block, env)
    return env
