"""Postgres ``money`` type and numeric ``to_char`` formatting.

``money`` is stored as a ``Decimal`` (BSON ``Decimal128``) — the same underlying
value as ``numeric`` — and renders as ``$1,234.56``. ``to_char(numeric, fmt)``
implements the common subset of Postgres' numeric template patterns. Both live
here; ``secantus.sql`` ``scalar`` / ``typemap`` / ``planner`` wire them in.

Supported ``to_char`` patterns: ``9`` / ``0`` (digit positions), ``.`` (decimal
point), ``,`` (group separator), ``$`` (currency), ``S`` (anchored sign), ``MI``
(trailing minus), ``PR`` (angle-bracket negatives), and the ``FM`` prefix
(suppress padding). Out of scope: ``EEEE`` scientific, ``RN`` roman, ``V``
(implied scale), ``TH`` / ``th`` ordinals, and locale patterns (``L`` / ``D`` /
``G`` beyond the ASCII forms).
"""

from __future__ import annotations

from decimal import ROUND_HALF_UP, Decimal, InvalidOperation
from typing import Any


class MoneyError(ValueError):
    """A malformed money literal."""


def parse_money(value: Any) -> Decimal:
    """Parse a money literal (``$1,234.56`` / ``-1234.56`` / ``(1234.56)``) to a
    two-decimal ``Decimal``."""
    if isinstance(value, Decimal):
        return value.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)
    s = str(value).strip()
    neg = s.startswith("(") and s.endswith(")")
    s = s.strip("()").replace("$", "").replace(",", "").replace(" ", "")
    try:
        d = Decimal(s or "0")
    except InvalidOperation as e:
        raise MoneyError(f"invalid money value: {value!r}") from e
    if neg:
        d = -d
    return d.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)


def render_money(value: Any) -> str:
    """Render a money value as ``$1,234.56`` (negatives as ``-$1,234.56``)."""
    d = value if isinstance(value, Decimal) else Decimal(str(value))
    d = d.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)
    neg = d < 0
    body = f"{abs(d):,.2f}"
    return f"{'-' if neg else ''}${body}"


def _group_int(digits: str) -> str:
    """Insert ``,`` group separators every three digits from the right."""
    rev = digits[::-1]
    chunks = [rev[i : i + 3] for i in range(0, len(rev), 3)]
    return ",".join(chunks)[::-1]


def to_char_numeric(value: Any, fmt: str) -> str:
    """Format a number per a Postgres numeric template (see the module docstring
    for the supported subset)."""
    f = fmt
    fill = False
    up = f.upper()
    if "FM" in up:
        idx = up.index("FM")
        f = f[:idx] + f[idx + 2 :]
        fill = True
        up = f.upper()

    has_dollar = "$" in f or "L" in up  # ``L`` is the locale currency (``$`` here)
    has_s = "S" in up
    trailing_mi = up.rstrip().endswith("MI")
    trailing_pr = up.rstrip().endswith("PR")

    # Split the digit template into integer / fraction around the decimal point.
    dot = f.find(".")
    int_tmpl = f if dot < 0 else f[:dot]
    frac_tmpl = "" if dot < 0 else f[dot + 1 :]
    int_slots = [c for c in int_tmpl if c in "90"]
    frac_slots = [c for c in frac_tmpl if c in "90"]
    n_int, n_frac = len(int_slots), len(frac_slots)
    grouping = "," in int_tmpl

    dec = value if isinstance(value, Decimal) else Decimal(str(value))
    quant = Decimal(1).scaleb(-n_frac) if n_frac else Decimal(1)
    r = dec.quantize(quant, rounding=ROUND_HALF_UP)
    neg = r < 0
    r = abs(r)

    s = f"{r:.{n_frac}f}"
    idig, _, fdig = s.partition(".")

    # Lay the integer digits into their slots (right-aligned). A leading unused
    # ``0`` slot renders ``0``; a leading unused ``9`` slot renders a space (or
    # nothing under FM).
    if len(idig) >= n_int:
        int_field = idig
    else:
        lead = n_int - len(idig)
        pad = "".join("0" if int_slots[i] == "0" else ("" if fill else " ") for i in range(lead))
        int_field = pad + idig
    if grouping:
        # Group only the significant (non-blank) digits.
        stripped = int_field.lstrip(" ")
        blanks = len(int_field) - len(stripped)
        int_field = (" " * blanks if not fill else "") + _group_int(stripped)

    body = int_field
    if n_frac:
        body += "." + fdig
    if has_dollar:
        body = "$" + body

    if trailing_pr:
        return f"<{body}>" if neg else (body if fill else f" {body} ")
    if trailing_mi:
        return f"{body}{'-' if neg else (' ' if not fill else '')}"
    if has_s:
        return f"{'-' if neg else '+'}{body}"
    if neg:
        return f"-{body}"
    return body if fill else f" {body}"
