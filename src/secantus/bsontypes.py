"""mongod's type vocabulary — the one place that names a BSON type.

Error messages across every engine quote the type of an offending value
(``BSON field 'count.skip' is the wrong type 'objectId'``). That vocabulary is
mongod's, not Python's, and it had drifted into **three** partial copies —
`update`, `expressions` and `changestreams` each covered the types its own
messages happened to hit and fell through to ``type(v).__name__`` for the rest.
That fallback is what put Python class names on the wire: ``'ObjectId'`` for
``objectId``, ``'datetime'`` for ``date``, ``'bytes'`` for ``binData``.

Every name below was probed against mongod 8.2.11 (2026-08-31) by sending the
value into a numeric slot and reading back the type it named::

    ObjectId -> objectId    Binary/bytes -> binData    Code -> javascript
    datetime -> date        Regex/re.Pattern -> regex  Timestamp -> timestamp
    MinKey -> minKey        MaxKey -> maxKey           DBRef -> object

Two orderings are load-bearing, because pymongo subclasses builtins:

* ``Code`` is a subclass of ``str`` — check it FIRST or javascript reads as
  string;
* ``Binary`` is a subclass of ``bytes``, and ``bool`` of ``int``.

`crates/secantus-core/src/query.rs::bson_type_name` is the Rust counterpart and
already had these names; this module brings Python to it rather than the other
way round.
"""

from __future__ import annotations

import datetime as _dt
import re as _re
from collections.abc import Mapping
from typing import Any

import bson
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp


def bson_type_name(v: Any) -> str:
    """Name ``v``'s BSON type the way mongod does in an error message."""
    # bool before int: bool is an int subclass in Python, not in BSON.
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, bson.Int64):
        return "long"
    if isinstance(v, int):
        # An int outside the int32 range encodes as an int64, and mongod names
        # it 'long'. The changestreams copy said 'int' for every int.
        return "int" if -(2**31) <= v < 2**31 else "long"
    if isinstance(v, float):
        return "double"
    if isinstance(v, bson.Decimal128):
        return "decimal"
    # Code before str (subclass), Binary before bytes (subclass).
    if isinstance(v, bson.Code):
        # A Code CARRYING a scope is BSON type 15, not 13, and mongod names the
        # two differently on both surfaces that quote a type -- the error text
        # ($concat only supports strings, not javascriptWithScope) and the
        # `$type` operator. `query._is_javascript` already drew this line for
        # the query language; this helper did not, so every error message about
        # a scoped Code named the wrong type (probed 8.2.11, 2026-09-03).
        return "javascript" if v.scope is None else "javascriptWithScope"
    if isinstance(v, str):
        return "string"
    if v is None:
        return "null"
    if isinstance(v, bson.ObjectId):
        return "objectId"
    if isinstance(v, _dt.datetime):
        return "date"
    if isinstance(v, (bson.Binary, bytes, bytearray)):
        return "binData"
    if isinstance(v, (bson.Regex, _re.Pattern)):
        return "regex"
    if isinstance(v, bson.Timestamp):
        return "timestamp"
    if isinstance(v, bson.MinKey):
        return "minKey"
    if isinstance(v, bson.MaxKey):
        return "maxKey"
    # A DBRef encodes as a subdocument, and mongod calls it 'object'.
    if isinstance(v, bson.DBRef):
        return "object"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, (list, tuple)):
        return "array"
    return type(v).__name__


def render_bson(v: Any) -> str:
    """Render a value the way mongod echoes it back inside an error message.

    `createIndexes` quotes the offending index spec verbatim —
    ``Error in specification { key: { a: 1 }, name: "i", collation: 5 } ::
    caused by :: ...`` — in mongod's shell syntax, which is neither JSON nor
    Python's ``repr``. Every rule below was probed against 8.2.11 (2026-08-31)
    by sending the value and reading the echo:

        5 / Int64(7)     5 / 7          {}                  {}
        1.5 / 2.0        1.5 / 2.0      {a: 1}              { a: 1 }
        1e30             1e+30          []                  []
        Decimal("2.5")   2.5            [1, 2]              [ 1, 2 ]
        "x" / ""         "x" / ""       ObjectId(...)       ObjectId('...')
        True / None      true / null    datetime            new Date(<ms>)
        Binary(b"\\xab")  BinData(0, AB) Binary(..., 4)      UUID("...")
        Regex("^a","i")  /^a/i          Code("f(){}")       f(){}
        Timestamp(1, 2)  Timestamp(1, 2) MinKey/MaxKey      MinKey/MaxKey

    Two details are deliberate, not oversights:

    * a non-empty document or array gets INNER spaces (``{ a: 1 }``,
      ``[ 1, 2 ]``) while an empty one does not (``{}``, ``[]``);
    * strings are **not escaped** — mongod emits ``"he said "hi""`` for a
      string containing quotes, and ``"a\\b"`` for one containing a backslash.
      Reproducing that means not calling a JSON encoder here.

    Python's ``repr`` already matches mongod for every float probed (``2.0``
    keeps its ``.0``, ``1e30`` renders ``1e+30``), so floats pass through it.
    """
    if isinstance(v, bool):
        return "true" if v else "false"
    if v is None:
        return "null"
    if isinstance(v, bson.Int64):
        return str(int(v))
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, bson.Decimal128):
        return str(v)
    if isinstance(v, bson.Code):
        return str(v)
    if isinstance(v, str):
        return f'"{v}"'
    if isinstance(v, bson.ObjectId):
        return f"ObjectId('{v}')"
    if isinstance(v, _dt.datetime):
        return f"new Date({_epoch_millis(v)})"
    if isinstance(v, (bson.Binary, bytes, bytearray)):
        subtype = getattr(v, "subtype", 0)
        if subtype == 4:
            return f'UUID("{bson.Binary(bytes(v), 4).as_uuid()}")'
        return f"BinData({subtype}, {bytes(v).hex().upper()})"
    if isinstance(v, bson.Regex):
        return f"/{v.pattern}/{_regex_flag_string(v.flags)}"
    if isinstance(v, _re.Pattern):
        return f"/{v.pattern}/{_regex_flag_string(v.flags)}"
    if isinstance(v, bson.Timestamp):
        return f"Timestamp({v.time}, {v.inc})"
    if isinstance(v, bson.MinKey):
        return "MinKey"
    if isinstance(v, bson.MaxKey):
        return "MaxKey"
    if isinstance(v, Mapping):
        if not v:
            return "{}"
        inner = ", ".join(f"{k}: {render_bson(val)}" for k, val in v.items())
        return "{ " + inner + " }"
    if isinstance(v, (list, tuple)):
        if not v:
            return "[]"
        return "[ " + ", ".join(render_bson(x) for x in v) + " ]"
    return str(v)


def _epoch_millis(v: _dt.datetime) -> int:
    """Milliseconds since the epoch, the way BSON stores a date.

    A naive datetime is UTC here, matching how pymongo encodes one.
    """
    if v.tzinfo is None:
        v = v.replace(tzinfo=_dt.timezone.utc)
    return int(v.timestamp() * 1000)


def _regex_flag_string(flags: Any) -> str:
    """mongod's trailing regex flags, in its own order (``imxlsu``)."""
    if isinstance(flags, int):
        out = ""
        for bit, ch in ((_re.I, "i"), (_re.M, "m"), (_re.X, "x"), (_re.S, "s"), (_re.U, "u")):
            if flags & bit:
                out += ch
        return out
    return str(flags or "")


def fmt_double_g(value: float) -> str:
    """A double as mongod streams it into an ARITHMETIC error message: C++'s
    ``ostream <<`` at its default six significant digits, so ``1234567890123.0``
    prints ``1.23457e+12``.

    NOT the rendering a query / update PARSE error uses -- see
    :func:`fmt_double_parse`, and do not merge the two. Probed 8.2.11
    (2026-09-01): ``$toInt: 1234567890123.0`` overflows with ``1.23457e+12``
    while ``$size: 1234567890123.0`` reports ``1234567890123.0``.
    """
    return f"{value:g}"


def fmt_double_parse(value: float) -> str:
    """A double as mongod echoes it when reflecting a stage's own SPECIFICATION.

    ``%.16g``, with a ``.0`` appended when that leaves no ``.`` or ``e`` -- so
    ``-1.0`` stays ``-1.0`` (the VALUE form, :func:`fmt_double_value`, says
    ``-1``) and ``1e16`` stays ``1e+16``.

    This used to be ``repr``, described here as "the shortest round-trip form",
    and the two agree for every ordinary value -- which is why it stood. They
    part company at the bottom of the range, where the shortest form that
    round-trips is SHORTER than sixteen significant digits: mongod echoes
    ``1e-308`` as ``9.999999999999999e-309`` and ``5e-324`` as
    ``4.940656458412465e-324``, because it prints the double's actual value to
    16 digits rather than the shortest string that parses back to it. Caught by
    the differential gate, not by the unit tests, which had been written from
    the same wrong assumption as the code (measured 8.2.11, 2026-09-07; the
    rule matches on 38 values including the denormals).
    """
    if value != value:
        return "nan"
    if value in (float("inf"), float("-inf")):
        return "inf" if value > 0 else "-inf"
    rendered = f"{value:.16g}"
    return rendered if ("." in rendered or "e" in rendered) else rendered + ".0"


def fmt_double_value(value: float) -> str:
    """A double as mongod echoes it in a VALUE error -- C's ``%g``, precision 6.

    mongod has **two** double renderings and they are not interchangeable
    (measured against 8.2.11, 2026-09-07):

    * the VALUE form, this one -- ``mongo::Value::toString`` -- prints
      ``-0``, ``-1``, ``1.23457e+06``, ``-2.14748e+09``, ``0.000123457``;
    * the SPEC form, :func:`fmt_double_parse`, echoes a stage's own
      specification back in the shortest round-trip form and keeps a whole
      double's ``.0``: ``-0.0``, ``-1.0``, ``1234567.0``, ``-2147483648.0``.

    One renderer used to serve both, so every VALUE message rendered a double
    the SPEC way -- ``$mergeObjects``, ``$replaceRoot``, ``$ln``, ``$log10``.
    Python's ``:g`` IS C's ``%g``; verified against mongod on 37 values at zero
    mismatches, so this deliberately does not hand-roll the algorithm.
    """
    if value != value:
        return "nan"
    if value in (float("inf"), float("-inf")):
        return "inf" if value > 0 else "-inf"
    return f"{value:g}"


def is_bson_string(value: Any) -> bool:
    """A BSON string -- which ``bson.Code`` is NOT, however much it subclasses
    ``str``.

    An ``isinstance(v, str)`` that meant "a BSON string" has now produced
    eleven bugs across four batches: a crash wherever the value then reaches a
    dict key or a set, and a wrong answer wherever it is treated as text. Use
    this instead of a bare ``isinstance`` anywhere the distinction is a BSON
    one.
    """
    return isinstance(value, str) and not isinstance(value, Code)


def bson_value_repr_stage(value: Any) -> str:
    """Render a BSON value the way an AGGREGATION STAGE error echoes it.

    mongod has **two** renderings and they are not interchangeable. Probed
    side by side on 8.2.11 (2026-09-01) -- `$size` for the query family,
    `$redact` for this one -- they differ in six places::

        type        query family              stage family
        array       [ 1 ]                     [1]
        document    { a: 1 }                  {a: 1}
        binary      BinData(0, 7A)            BinData(0, "7A")
        objectId    ObjectId('507f…')         507f…
        date        new Date(1577923200000)   2020-01-02T00:00:00.000Z
        javascript  x=1                       Code("x=1")
        double      -1.0                      -1

    The DOUBLE row was added 2026-09-07 and is the one that had been wrong in
    both engines: this family is C's ``%g`` (:func:`fmt_double_value`) and the
    query family is the shortest round-trip form (:func:`fmt_double_parse`).

    Everything else -- strings, ints, bools, null, regex, MinKey / MaxKey,
    Timestamp, Decimal128 -- is identical, which is what makes the difference
    easy to miss: reusing :func:`bson_value_repr` here fixes the types a
    probe happens to cover and quietly breaks the seven above.
    """
    if isinstance(value, float):
        # The VALUE form, not `bson_value_repr`'s SPEC form -- see
        # `fmt_double_value`. Falling through to it printed `-0.0` where mongod
        # writes `-0` and `1234567.0` where it writes `1.23457e+06`.
        return fmt_double_value(value)
    if isinstance(value, Code):
        return f'Code("{value}")'
    if isinstance(value, ObjectId):
        return str(value)
    if isinstance(value, _dt.datetime):
        aware = value if value.tzinfo else value.replace(tzinfo=_dt.timezone.utc)
        return aware.astimezone(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + (
            f"{aware.microsecond // 1000:03d}Z"
        )
    if isinstance(value, (bytes, Binary, bytearray)):
        subtype = getattr(value, "subtype", 0)
        return f'BinData({subtype}, "{bytes(value).hex().upper()}")'
    if isinstance(value, list):
        return "[" + ", ".join(bson_value_repr_stage(v) for v in value) + "]"
    if isinstance(value, Mapping):
        return "{" + ", ".join(f"{k}: {bson_value_repr_stage(v)}" for k, v in value.items()) + "}"
    return bson_value_repr(value)


def bson_value_repr(value: Any) -> str:
    """Render a BSON value the way mongod echoes an offending one back.

    This is the shell-ish form its parse errors use, and it is NOT Python's
    ``repr``: strings take DOUBLE quotes, a document prints ``{ a: 1 }`` with
    inner spaces, a regex prints ``/a/i``, a date prints ``new Date(<millis>)``.
    Probed across every BSON type against 8.2.11 (2026-09-01).

    Messages that echoed a value with ``repr`` therefore said ``'x'`` where
    mongod says ``"x"`` -- a difference a client comparing error text sees.
    """
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str) and not isinstance(value, Code):
        return f'"{value}"'
    if isinstance(value, Code):
        return str(value)
    if isinstance(value, float):
        return fmt_double_parse(value)
    if isinstance(value, Decimal128):
        return str(value)
    if isinstance(value, (int, Int64)):
        return str(int(value))
    if isinstance(value, ObjectId):
        return f"ObjectId('{value}')"
    if isinstance(value, MinKey):
        return "MinKey"
    if isinstance(value, MaxKey):
        return "MaxKey"
    if isinstance(value, Timestamp):
        return f"Timestamp({value.time}, {value.inc})"
    if isinstance(value, _dt.datetime):
        millis = int(value.replace(tzinfo=value.tzinfo or _dt.timezone.utc).timestamp() * 1000)
        return f"new Date({millis})"
    if isinstance(value, Regex):
        return f"/{value.pattern}/{_regex_flag_text(value.flags)}"
    if isinstance(value, (bytes, Binary, bytearray)):
        subtype = getattr(value, "subtype", 0)
        return f"BinData({subtype}, {bytes(value).hex().upper()})"
    if isinstance(value, list):
        if not value:
            return "[]"
        return "[ " + ", ".join(bson_value_repr(v) for v in value) + " ]"
    if isinstance(value, Mapping):
        if not value:
            return "{}"
        return "{ " + ", ".join(f"{k}: {bson_value_repr(v)}" for k, v in value.items()) + " }"
    return str(value)


#: `re` flag bits to the regex-literal letters mongod prints, in its order.
_REGEX_FLAG_LETTERS = (
    (_re.IGNORECASE, "i"),
    (_re.MULTILINE, "m"),
    (_re.DOTALL, "s"),
    (_re.VERBOSE, "x"),
)


def _regex_flag_text(flags: int | str) -> str:
    if isinstance(flags, str):
        return flags
    return "".join(letter for bit, letter in _REGEX_FLAG_LETTERS if flags & bit)


class Int64CoercionError(Exception):
    """A numeric argument mongod refuses to read as a 64-bit integer.

    Carries the message and code so each caller can raise its own error class
    without restating the four-way ladder.
    """

    def __init__(self, message: str, code: int = 9) -> None:
        super().__init__(message)
        self.message = message
        self.code = code


def coerce_int64_argument(value: Any, label: str) -> int:
    """mongod's numeric-argument ladder, shared by `$pop` and the `$bits*` mask.

    Four distinct failures, in this order (probed 8.2.11, 2026-09-01, and
    identical for both operators -- only the ``label`` before the colon
    differs)::

        NaN                       Expected an integer, but found NaN in: <label>: nan
        non-finite / out of range Cannot represent as a 64-bit integer: <label>: 1e+20
        fractional                Expected an integer: <label>: 1.5
        non-integral Decimal128   Cannot represent as a 64-bit integer: <label>: 1.5

    A whole ``Decimal128`` is accepted, which is easy to miss: both callers
    rejected it outright. Raises `Int64CoercionError`; a non-numeric type is
    the caller's to report, because the two word it differently.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float, Decimal128)):
        raise TypeError("not a numeric argument")
    if isinstance(value, Decimal128):
        dec = value.to_decimal()
        if not dec.is_finite() or dec != dec.to_integral_value():
            raise Int64CoercionError(
                f"Cannot represent as a 64-bit integer: {label}: {bson_value_repr(value)}"
            )
        as_int = int(dec)
        if not (_INT64_MIN <= as_int <= _INT64_MAX):
            raise Int64CoercionError(
                f"Cannot represent as a 64-bit integer: {label}: {bson_value_repr(value)}"
            )
        return as_int
    if isinstance(value, float):
        if value != value:  # NaN — `math.isnan` without the import
            raise Int64CoercionError(f"Expected an integer, but found NaN in: {label}: nan")
        if value in (float("inf"), float("-inf")) or not (_INT64_MIN <= value <= _INT64_MAX):
            raise Int64CoercionError(
                f"Cannot represent as a 64-bit integer: {label}: {fmt_double_parse(value)}"
            )
        if not value.is_integer():
            raise Int64CoercionError(f"Expected an integer: {label}: {fmt_double_parse(value)}")
        return int(value)
    return value


#: The 64-bit window `coerce_int64_argument` enforces.
_INT64_MIN = -(2**63)
_INT64_MAX = 2**63 - 1

# mongod stores a regex's options ALPHABETICALLY SORTED, which is also ascending
# bit order for the flags `bson.Regex` normalises an option string into. Both
# the in-memory sort (`ordering._regex_sort_key`) and the persisted index-entry
# encoder (`sortkey._regex_options`) have to render options the same way, or an
# index changes the sort answer -- so the table lives here rather than in either
# of them. Probed 8.2.11 (2026-09-01): `/a/i < /a/im < /a/m`.
REGEX_OPTION_CHARS: tuple[tuple[int, str], ...] = (
    (_re.I, "i"),
    (_re.L, "l"),
    (_re.M, "m"),
    (_re.S, "s"),
    (_re.U, "u"),
    (_re.X, "x"),
)


def regex_options_string(flags: Any) -> str:
    """The normalised option string for a `bson.Regex`'s flags."""
    if isinstance(flags, str):
        return flags
    if isinstance(flags, (bytes, bytearray)):
        return bytes(flags).decode("utf-8", "replace")
    return "".join(c for bit, c in REGEX_OPTION_CHARS if flags & bit)
