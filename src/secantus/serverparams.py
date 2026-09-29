"""``setParameter`` — the runtime-settable half of the server parameters.

``getParameter`` reports the parameters this server has; this module is what
lets a client CHANGE the ones mongod lets you change at runtime, and refuse the
rest the way mongod refuses them.

Every code, message and coercion rule below was **measured** against mongod
8.2.11 on 2026-09-29 (a standalone and a single-node replica set answered
identically). Five of them contradict the obvious guess, which is why they are
spelled out here rather than inferred:

===========================================  ==========================================
request                                      answer
===========================================  ==========================================
a settable parameter                         ``ok: 1`` plus ``was``, the PREVIOUS value
a parameter that exists but is startup-only  20 ``IllegalOperation``
a name the server does not register          72 ``InvalidOptions``
no parameter at all                          72 ``InvalidOptions``
a settable parameter, wrong type             2 ``BadValue``
any of the above outside ``admin``           13 ``Unauthorized``
===========================================  ==========================================

``logLevel`` **coerces** any number or bool, truncating toward zero (``1.9`` →
``1``, ``-0.5`` → ``0``, ``True`` → ``1``) and **clamping** the top (``6`` and
``99`` both store ``5``); it rejects only str / list / dict / None, and a
negative that does not truncate to zero (``-1``) is rejected rather than
clamped. A boolean parameter accepts **every** BSON type and stores its
truthiness — ``None``, ``0``, ``0.0`` and ``False`` are false, while an EMPTY
STRING, a list and a dict are all true.

**We deliberately do not register a parameter we do not honour.** The
``ingressConnectionEstablishment*`` family that mongo-go-driver's
``TestConnectionPoolBackpressure`` sets is real on mongod and tunes a
connection rate limiter SecantusDB has no equivalent of; accepting those names
would let a client ask for rate limiting and silently get none, which is the
half-implemented feature this project prefers an honest refusal to. A server
that does not register them answers 72, and so do we.
"""

from __future__ import annotations

import threading
from typing import Any

# Trailing space included -- mongod emits one, and a driver comparing the
# message verbatim sees it.
_HELP_SUFFIX = ", use help:true to see options "

#: mongod's highest log level; ``setParameter`` clamps rather than refusing.
MAX_LOG_LEVEL = 5

#: MongoDB's *generic command arguments* -- the envelope every command may
#: carry -- which are NOT parameter names. Filtering only ``$``-prefixed keys is
#: not enough: ``pymongo`` attaches an ``lsid`` to every command, so
#: ``{setParameter: 1, logLevel: 0}`` arrives as three keys and a naive handler
#: rejects the whole call with "unrecognized parameter [lsid]". Every real
#: driver call would fail while a unit test built from a bare dict passed.
_GENERIC_ARGS = frozenset(
    {
        "lsid",
        "txnNumber",
        "autocommit",
        "startTransaction",
        "stmtId",
        "readConcern",
        "writeConcern",
        "maxTimeMS",
        "comment",
        "apiVersion",
        "apiStrict",
        "apiDeprecationErrors",
    }
)

#: Parameters this server reports but mongod refuses to change at runtime.
#: Answering 20 rather than 72 matters: 72 claims the parameter does not exist,
#: which ``getParameter`` reporting it would immediately contradict.
_STARTUP_ONLY = frozenset(
    {"enableTestCommands", "featureCompatibilityVersion", "authenticationMechanisms"}
)

_LOG_LEVEL = "logLevel"
_BOOL = "bool"

#: ``name -> kind`` for every runtime-settable parameter. Keep in lockstep with
#: ``commands._get_parameter``: a name settable here must be reported there, or
#: ``getParameter`` will not show the value a client just set.
_SETTABLE: dict[str, str] = {"logLevel": _LOG_LEVEL, "quiet": _BOOL}


class SetParameterError(Exception):
    """A ``setParameter`` refusal carrying mongod's code and codeName."""

    def __init__(self, message: str, code: int, code_name: str) -> None:
        super().__init__(message)
        self.code = code
        self.code_name = code_name


def is_generic_arg(key: str) -> bool:
    """Whether ``key`` is envelope rather than a parameter to set."""
    return key.startswith("$") or key in _GENERIC_ARGS


def _coerce(kind: str, value: Any) -> Any:
    """The value mongod would STORE, or raise if it refuses it."""
    if kind is _BOOL or kind == _BOOL:
        # Accepts every type; only these four are falsey. Note `0.0` is false
        # but an empty string is TRUE, which Python's own truthiness gets wrong.
        if value is None or value is False:
            return False
        if isinstance(value, bool):
            return value
        return not (isinstance(value, (int, float)) and value == 0)
    # logLevel
    if isinstance(value, bool):
        number: float = 1.0 if value else 0.0
    elif isinstance(value, (int, float)):
        number = float(value)
    else:
        raise _invalid_value(_LOG_LEVEL, value)
    # Truncate toward zero FIRST, then reject a negative: -0.5 lands on 0 and is
    # accepted, while -1 is refused.
    truncated = int(number)
    if truncated < 0:
        raise _invalid_value(_LOG_LEVEL, value)
    return min(truncated, MAX_LOG_LEVEL)


def _render_value(value: Any) -> str:
    """mongod's shell-style rendering of a BSON value for an error message."""
    if isinstance(value, str):
        return f'"{value}"'
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        return f"{value:.1f}" if value.is_integer() else repr(value)
    if isinstance(value, list):
        if not value:
            return "[]"
        return "[ " + ", ".join(_render_value(v) for v in value) + " ]"
    if isinstance(value, dict):
        if not value:
            return "{}"
        return "{ " + ", ".join(f"{k}: {_render_value(v)}" for k, v in value.items()) + " }"
    return str(value)


def _invalid_value(name: str, value: Any) -> SetParameterError:
    # The parameter name appears TWICE -- once in the sentence and once as the
    # rendered element's key. That is mongod's shape, not a typo.
    return SetParameterError(
        f"Invalid value for {name}: {name}: {_render_value(value)}", 2, "BadValue"
    )


class ServerParams:
    """Server-wide store for the settable parameters' current values.

    Holds only the ones that have been CHANGED; an unset parameter reads its
    default from ``getParameter``'s table, so the two cannot drift apart.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._values: dict[str, Any] = {}

    def get(self, name: str) -> Any:
        with self._lock:
            return self._values.get(name)

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            return dict(self._values)

    def _set(self, name: str, value: Any) -> None:
        with self._lock:
            self._values[name] = value

    def apply(self, doc: dict[str, Any], defaults: dict[str, Any]) -> dict[str, Any]:
        """Validate and apply a ``setParameter`` command, returning its reply.

        Raises :class:`SetParameterError` for every refusal. ``defaults`` is
        ``getParameter``'s table, used to report ``was`` for a parameter that
        has not been changed yet.
        """
        requested = [
            (k, v) for k, v in doc.items() if k != "setParameter" and not is_generic_arg(k)
        ]
        if not requested:
            raise SetParameterError(f"no option found to set{_HELP_SUFFIX}", 72, "InvalidOptions")

        # mongod validates every named parameter BEFORE applying any of them, so
        # a batch naming one bad parameter changes nothing.
        coerced: list[tuple[str, Any]] = []
        for name, value in requested:
            if name in _STARTUP_ONLY:
                raise SetParameterError(
                    f"not allowed to change [{name}] at runtime", 20, "IllegalOperation"
                )
            kind = _SETTABLE.get(name)
            if kind is None:
                raise SetParameterError(
                    f"attempted to set unrecognized parameter [{name}]{_HELP_SUFFIX}",
                    72,
                    "InvalidOptions",
                )
            coerced.append((name, _coerce(kind, value)))

        reply: dict[str, Any] = {}
        for index, (name, value) in enumerate(coerced):
            previous = self.get(name)
            if previous is None:
                previous = defaults.get(name)
            self._set(name, value)
            # mongod reports a single ``was``, the FIRST named parameter's
            # previous value, even when several are set in one call.
            if index == 0:
                reply["was"] = previous
        reply["ok"] = 1.0
        return reply
