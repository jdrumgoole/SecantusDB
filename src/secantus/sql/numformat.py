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


#: Template tokens that occupy an output position. `D` / `G` / `L` are the
#: locale spellings of `.` / `,` / the currency symbol; under this server's `C`
#: locale the currency symbol is EMPTY, which is why `to_char(1234.5,
#: 'L9999D99')` is `'  1234.50'` in Postgres and not `' $1234.50'`.
_DIGIT, _SEP, _DEC, _CUR = "d", "s", "p", "c"


def _parse_numeric_template(fmt: str) -> tuple[list[tuple[str, str]], str, str]:
    """Split a numeric template into body tokens plus its sign spelling.

    Returns ``(tokens, lead_sign, trail_sign)`` where a sign spelling is one of
    ``""`` (implicit), ``"S"``, ``"MI"`` or ``"PR"``. Postgres reads `S` / `MI`
    / `PR` positionally: at the front they occupy the leading position, at the
    back they trail the number."""
    tokens: list[tuple[str, str]] = []
    lead = trail = ""
    i, n = 0, len(fmt)
    while i < n:
        two = fmt[i : i + 2].upper()
        ch = fmt[i]
        if two in ("MI", "PR"):
            if tokens:
                trail = two
            else:
                lead = two
            i += 2
            continue
        if ch in "90":
            tokens.append((_DIGIT, ch))
        elif ch in "." or ch.upper() == "D":
            tokens.append((_DEC, "."))
        elif ch == "," or ch.upper() == "G":
            tokens.append((_SEP, ","))
        elif ch == "$":
            tokens.append((_CUR, "$"))
        elif ch.upper() == "L":
            # The C locale's currency symbol is EMPTY, but the token still
            # occupies its position — `to_char(12, 'L9999D99')` is `'    12.00'`
            # in Postgres, one wider than the same template without the `L`.
            tokens.append((_CUR, " "))
        elif ch.upper() == "S":
            if tokens:
                trail = "S"
            else:
                lead = "S"
        i += 1
    return tokens, lead, trail


#: Roman numeral pieces, largest first.
_ROMAN: tuple[tuple[int, str], ...] = (
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
)

#: `RN` always renders in a fifteen-character right-aligned field — the width
#: of `MMMDCCCLXXXVIII`, the longest numeral it can produce.
_ROMAN_WIDTH = 15


def _to_roman(value: Decimal) -> str:
    """`RN`: the value as a Roman numeral, right-aligned in fifteen columns.

    Postgres rounds to an integer first and fills the whole field with `#` for
    anything outside 1..3999, which is every value a Roman numeral cannot
    express — zero and negatives included."""
    n = int(value.quantize(Decimal(1), rounding=ROUND_HALF_UP))
    if not 1 <= n <= 3999:
        return "#" * _ROMAN_WIDTH
    out: list[str] = []
    for amount, numeral in _ROMAN:
        count, n = divmod(n, amount)
        out.append(numeral * count)
    return "".join(out).rjust(_ROMAN_WIDTH)


def to_char_numeric(value: Any, fmt: str) -> str:
    """Format a number per a Postgres numeric template.

    Rewritten against a 300-case sweep of PostgreSQL 14.13, which the previous
    version matched 63 of. Four rules it did not implement at all:

    * **Overflow prints `#`.** A value too wide for the digit slots fills every
      one of them with `#` — `to_char(1234.5, '999')` is `' ###'`, not
      `' 1235'`. Printing the number anyway silently violated the template's
      own declared width.
    * **The sign sits against the digits**, in the position immediately left of
      the first one, not in front of the whole padded field:
      `to_char(-12, '999')` is `' -12'`, not `'- 12'`.
    * **A `0` slot zero-fills everything to its right**, so `'0999'` over 12 is
      `' 0012'` rather than `' 0 12'`.
    * **An all-`9` integer part renders BLANK when the value has none** —
      `to_char(0.5, '999.9')` is `'    .5'`, with no leading zero.

    Group separators only appear between two printed digits, and `S` / `MI` /
    `PR` are read positionally: at the front they take the leading position, at
    the back they trail the number."""
    fill = "FM" in fmt.upper()
    if fill:
        idx = fmt.upper().index("FM")
        fmt = fmt[:idx] + fmt[idx + 2 :]
    if "RN" in fmt.upper():
        dec = value if isinstance(value, Decimal) else Decimal(str(value))
        roman = _to_roman(dec)
        return roman.strip() if fill else roman
    tokens, lead_sign, trail_sign = _parse_numeric_template(fmt)

    digit_slots = [t for t in tokens if t[0] == _DIGIT]
    dec_at = next((i for i, t in enumerate(tokens) if t[0] == _DEC), None)
    n_int = sum(
        1 for i, t in enumerate(tokens) if t[0] == _DIGIT and (dec_at is None or i < dec_at)
    )
    n_frac = len(digit_slots) - n_int

    dec = value if isinstance(value, Decimal) else Decimal(str(value))
    rounded = dec.quantize(Decimal(1).scaleb(-n_frac), rounding=ROUND_HALF_UP)
    neg = rounded < 0
    digits = f"{abs(rounded):.{n_frac}f}"
    int_digits, _, frac_digits = digits.partition(".")
    overflow = len(int_digits) > n_int

    # Lay the integer digits into their slots, right-aligned. Everything from
    # the leftmost `0` slot rightwards is zero-filled; to its left, blanks.
    int_slots = [
        t[1] for i, t in enumerate(tokens) if t[0] == _DIGIT and (dec_at is None or i < dec_at)
    ]
    zero_from = next((i for i, c in enumerate(int_slots) if c == "0"), None)
    if overflow:
        int_field = ["#"] * n_int
    elif int_digits == "0" and zero_from is None and dec_at is not None:
        # A zero integer part prints as BLANK when the template has a decimal
        # point and no `0` slot forcing it: `to_char(0.5, '999.9')` is `'    .5'`.
        int_field = [" "] * n_int
    else:
        pad = n_int - len(int_digits)
        int_field = list(int_digits.rjust(n_int, " ")) if pad >= 0 else list(int_digits)
        for i in range(pad):
            int_field[i] = "0" if zero_from is not None and i >= zero_from else " "
        if zero_from is not None:
            for i in range(zero_from, pad):
                int_field[i] = "0"

    frac_field = ["#"] * n_frac if overflow else list(frac_digits)
    if fill and not overflow and n_frac:
        # FM drops trailing fractional zeros — `to_char(0.5, 'FM0.99')` is `0.5`.
        while frac_field and frac_field[-1] == "0":
            frac_field.pop()

    # Walk the template, consuming the laid-out digits, so separators and the
    # currency symbol land where the template puts them.
    # A LEADING currency symbol is not padding, so `FM` must not strip it —
    # `to_char(1234.56, 'FML9999.99')` keeps the blank the empty C-locale
    # symbol occupies. Held aside and re-attached after the strip.
    lead_currency = tokens[0][1] if tokens and tokens[0][0] == _CUR else None
    out: list[str] = []
    ii = fi = 0
    for pos, (kind, text) in enumerate(tokens):
        if pos == 0 and lead_currency is not None:
            continue
        if kind == _DIGIT:
            if dec_at is not None and pos > dec_at:
                out.append(frac_field[fi] if fi < len(frac_field) else "")
                fi += 1
            else:
                out.append(int_field[ii])
                ii += 1
        elif kind == _SEP:
            # A separator prints only between two printed digits.
            left = "".join(out).strip()
            out.append("," if left and left[-1] not in " " else " ")
        elif kind == _DEC:
            # The point stays even when FM has stripped every fractional digit:
            # `to_char(12, 'FM999.99')` is `'12.'`.
            out.append(".")
        else:
            out.append(text)
    body = "".join(out)
    if fill:
        body = body.strip()
        # Under FM the blanks are gone, so a wholly blank integer part would
        # leave a bare point: PG emits the zero instead — `to_char(0,
        # 'FM999.99')` is `'0.'` while `to_char(0.5, 'FM999.99')` is `'.5'`.
        if body.startswith(".") and not body[1:].strip("."):
            body = "0" + body
    if lead_currency is not None:
        body = lead_currency + body

    return _apply_numeric_sign(body, neg, lead_sign, trail_sign, fill)


def _apply_numeric_sign(body: str, neg: bool, lead: str, trail: str, fill: bool) -> str:
    """Place the sign, which Postgres puts AGAINST the digits rather than in
    front of the padded field."""
    if trail == "PR" or lead == "PR":
        if not neg:
            return body if fill else f" {body} "
        if fill:
            return f"<{body.strip()}>"
        # `<` sits against the digits exactly as a sign does, and `>` follows.
        return _sign_against_digits(body, "<") + ">"
    if trail:
        mark = "-" if neg else ("+" if trail == "S" else " ")
        return body + ("" if fill and mark == " " else mark)
    mark = "-" if neg else ("+" if lead == "S" else " ")
    if fill:
        return (mark if mark != " " else "") + body
    if lead == "MI":
        # A LEADING `MI` takes the leftmost position outright, unlike the
        # implicit sign, which sits against the digits: `to_char(-12, 'MI999')`
        # is `'- 12'` where `to_char(-12, '999')` is `' -12'`.
        return mark + body
    return _sign_against_digits(body, mark)


def _sign_against_digits(body: str, mark: str) -> str:
    """Put ``mark`` in the position immediately left of the first printed
    character, widening the field by one.

    A CURRENCY symbol stays in front of the sign — `to_char(-12, '$9999.99')`
    is `'$  -12.00'`, not `'-$  12.00'` — so it is split off first."""
    prefix = ""
    if body[:1] == "$":
        prefix, body = body[0], body[1:]
    padded = " " + body
    first = next((i for i, c in enumerate(padded) if c != " "), len(padded) - 1)
    return prefix + padded[: first - 1] + mark + padded[first:]
