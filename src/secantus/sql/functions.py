"""Scalar session/info functions for FROM-less SELECT.

Covers the niladic and simple functions Postgres clients fire to introspect the
session — ``version()``, ``current_database()``, ``current_schema()``,
``current_user``, ``current_setting(name)``, ``set_config(name, value, local)``,
``pg_backend_pid()`` — resolved against the connection ``Session``. Returns
``(output_column_name, value, type_tag)``; unknown functions raise
``feature_not_supported``.
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus.sql import errors
from secantus.sql.session import VERSION_STRING, Session


def _arg_literals(node: exp.Anonymous) -> list[Any]:
    out: list[Any] = []
    for e in node.expressions:
        if isinstance(e, exp.Literal):
            out.append(e.this if e.is_string else _num(e.this))
        elif isinstance(e, exp.Boolean):
            out.append(bool(e.this))
        elif isinstance(e, exp.Null):
            out.append(None)
        else:
            raise errors.feature_not_supported(f"unsupported function argument: {e.sql()}")
    return out


def _num(text: str) -> Any:
    return float(text) if ("." in text or "e" in text.lower()) else int(text)


def evaluate_scalar(node: exp.Expression, session: Session) -> tuple[str, Any, str]:
    if isinstance(node, exp.Paren):
        return evaluate_scalar(node.this, session)
    if isinstance(node, exp.Dot):
        # A schema-qualified call like ``pg_catalog.version()`` — evaluate the
        # rightmost call and ignore the catalog qualifier.
        return evaluate_scalar(node.expression, session)
    if isinstance(node, exp.CurrentVersion):
        return ("version", VERSION_STRING, "text")
    if isinstance(node, exp.CurrentDatabase):
        return ("current_database", session.database, "text")
    if isinstance(node, exp.CurrentSchema):
        return ("current_schema", session.current_schema, "text")
    if isinstance(node, exp.CurrentUser):
        return ("current_user", session.user, "text")

    if isinstance(node, exp.Anonymous):
        name = (node.this if isinstance(node.this, str) else node.name).lower()
        # Drop a pg_catalog. qualifier if present.
        name = name.rsplit(".", 1)[-1]
        args = _arg_literals(node)
        return _evaluate_named(name, args, session)

    raise errors.feature_not_supported(f"unsupported expression in SELECT: {node.sql()}")


def _evaluate_named(name: str, args: list[Any], session: Session) -> tuple[str, Any, str]:
    if name == "version":
        return ("version", VERSION_STRING, "text")
    if name in ("current_database", "current_catalog"):
        return (name, session.database, "text")
    if name == "current_schema":
        return ("current_schema", session.current_schema, "text")
    if name in ("current_user", "current_role", "session_user", "user"):
        return (name, session.user, "text")
    if name == "current_setting":
        if not args:
            raise errors.syntax_error("current_setting() requires a setting name")
        return ("current_setting", session.get_setting(str(args[0])), "text")
    if name == "set_config":
        if len(args) < 2:
            raise errors.syntax_error("set_config() requires (name, value, is_local)")
        session.settings[str(args[0])] = "" if args[1] is None else str(args[1])
        return ("set_config", session.settings[str(args[0])], "text")
    if name == "pg_backend_pid":
        return ("pg_backend_pid", session.backend_pid, "int4")
    if name in ("pg_is_in_recovery",):
        return (name, False, "bool")
    if name in ("jsonb_build_object", "json_build_object"):
        out: dict[str, Any] = {}
        for i in range(0, len(args) - 1, 2):
            out[str(args[i])] = args[i + 1]
        return (name, out, "json")
    if name in ("jsonb_build_array", "json_build_array"):
        return (name, list(args), "json")
    raise errors.feature_not_supported(f"function {name}() is not supported")


# Anonymous calls that the full scalar evaluator (``scalar._call_func``) handles,
# so a FROM-less ``SELECT <fn>(...)`` must defer to it rather than the session path.
_SCALAR_EVAL_ANON = frozenset(
    {
        "isempty",
        "to_jsonb",
        "to_json",
        "row_to_json",
        "range_merge",
        "to_tsvector",
        "to_tsquery",
        "plainto_tsquery",
        "ts_rank",
        "ts_rank_cd",
        "masklen",
        "network",
        "netmask",
        "broadcast",
        "family",
        "abbrev",
        "hostmask",
    }
)


def is_scalar_function(node: exp.Expression) -> bool:
    """Whether ``plan_constant_select`` should evaluate ``node`` as a function."""
    if isinstance(node, exp.Dot):
        return is_scalar_function(node.expression)
    if isinstance(node, exp.Anonymous):
        # Range constructors (``int4range(1,5)`` …) and range predicates like
        # ``isempty(...)`` are handled by the full scalar evaluator, not the
        # session/info-function path — let them fall through.
        from secantus.sql import typemap

        name = str(node.this).lower()
        # Range constructors / isempty and the jsonb builders (to_jsonb / to_json /
        # row_to_json) are implemented by the full scalar evaluator, not here.
        if (
            name in typemap._RANGE_TAGS
            or name in typemap._MULTIRANGE_TAGS
            or name in _SCALAR_EVAL_ANON
        ):
            return False
    return isinstance(
        node,
        exp.CurrentVersion
        | exp.CurrentDatabase
        | exp.CurrentSchema
        | exp.CurrentUser
        | exp.Anonymous,
    )
