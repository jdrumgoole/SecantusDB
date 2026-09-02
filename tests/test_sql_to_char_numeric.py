"""`to_char(numeric, template)` matched PostgreSQL on 63 of 300 shapes.

A sweep of 30 templates x 10 values against PostgreSQL 14.13 found four rules
the implementation did not have at all:

* **Overflow prints `#`.** A value too wide for the digit slots fills every one
  of them — `to_char(1234.5, '999')` is `' ###'`, not `' 1235'`. Printing the
  number anyway silently violated the template's own declared width.
* **The sign sits against the digits**, in the position immediately left of the
  first one, not in front of the whole padded field: `to_char(-12, '999')` is
  `' -12'`, not `'- 12'`.
* **A `0` slot zero-fills everything to its right**, so `'0999'` over 12 is
  `' 0012'` rather than `' 0 12'`.
* **An all-`9` integer part renders blank when the value has none** —
  `to_char(0.5, '999.9')` is `'    .5'`, with no leading zero.

And the template never even reached the numeric formatter intact: sqlglot's
postgres dialect part-converts it to strftime first, so `MI999` arrived as
`%M999` and `9999D99` as `9999%u99`, and the tokens it did not recognise were
dropped.

All 300 shapes now match. Every expectation here was measured against
PostgreSQL 14.13.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql import numformat, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0][0][0]

    try:
        yield run
    finally:
        storage.close()


def _fmt(value, template):
    return numformat.to_char_numeric(Decimal(str(value)), template)


class TestOverflow:
    @pytest.mark.parametrize(
        ("value", "template", "want"),
        [
            (1234.5, "999", " ###"),
            (-1234.567, "999", "-###"),
            (999.99, "999", " ###"),
            (1234.5, "999.99", " ###.##"),
            (1234567, "0999", " ####"),
            (1234567, "9G999", " #,###"),
            # Just inside the width is not an overflow.
            (999, "999", " 999"),
            (999.99, "999.99", " 999.99"),
        ],
    )
    def test_hash_fill(self, value, template, want):
        assert _fmt(value, template) == want


class TestSignPlacement:
    @pytest.mark.parametrize(
        ("value", "template", "want"),
        [
            # The sign is adjacent to the digits, not in front of the padding.
            (-12, "999", " -12"),
            (-12, "99999", "   -12"),
            (12, "S999", " +12"),
            (-12, "S999", " -12"),
            (-0.5, "999", "  -1"),
            # A LEADING `MI` takes the leftmost position outright.
            (-12, "MI999", "- 12"),
            (12, "MI999", "  12"),
            # Trailing sign spellings put it after the number.
            (-12, "999MI", " 12-"),
            (12, "999MI", " 12 "),
            (-12, "999S", " 12-"),
            (12, "999S", " 12+"),
            (-12, "999PR", " <12>"),
            (12, "999PR", "  12 "),
        ],
    )
    def test_sign(self, value, template, want):
        assert _fmt(value, template) == want


class TestZeroSlots:
    @pytest.mark.parametrize(
        ("value", "template", "want"),
        [
            # A `0` slot zero-fills everything to its right.
            (12, "0999", " 0012"),
            (0, "0999", " 0000"),
            (-12, "0999", "-0012"),
            # An all-`9` integer part is blank when the value has none, but only
            # when the template has a decimal point to hold the value.
            (0.5, "999.9", "    .5"),
            (0, "999.99", "    .00"),
            (-0.5, "999.99", "   -.50"),
            (0, "999", "   0"),
        ],
    )
    def test_zero_fill(self, value, template, want):
        assert _fmt(value, template) == want


class TestSeparatorsAndCurrency:
    @pytest.mark.parametrize(
        ("value", "template", "want"),
        [
            # `G` is the locale group separator, `D` the decimal point.
            (1234.5, "9G999", " 1,235"),
            (12, "9G999", "    12"),
            (1234567, "9G999G999", " 1,234,567"),
            (12, "9999D99", "   12.00"),
            (0, "999D9", "    .0"),
            # `L` is the locale currency symbol — EMPTY in this server's C
            # locale, but it still occupies its position.
            (12, "L9999D99", "    12.00"),
            # `$` stays in front of the sign.
            (-12, "$9999.99", "$  -12.00"),
            (12, "$9999.99", "$   12.00"),
        ],
    )
    def test_tokens(self, value, template, want):
        assert _fmt(value, template) == want


class TestFillMode:
    @pytest.mark.parametrize(
        ("value", "template", "want"),
        [
            # FM drops padding AND trailing fractional zeros, but keeps the point.
            (12, "FM999.99", "12."),
            (12.5, "FM999.99", "12.5"),
            (0.5, "FM999.99", ".5"),
            # With nothing left but the point, PG emits the zero.
            (0, "FM999.99", "0."),
            (0, "FM0.99", "0."),
            (0, "FM999", "0"),
            (1234.5, "FM999,999.99", "1,234.5"),
            # The leading currency symbol is not padding and survives.
            (1234.56, "FML9999.99", " 1234.56"),
            (1234.56, "FM$9,999.99", "$1,234.56"),
            (-1234.5, "FM9999.99PR", "<1234.5>"),
        ],
    )
    def test_fm(self, value, template, want):
        assert _fmt(value, template) == want


class TestRoman:
    @pytest.mark.parametrize(
        ("value", "want"),
        [
            (12, "            XII"),
            (1234.5, "        MCCXXXV"),
            (999.99, "              M"),
            (0.5, "              I"),
            # Outside 1..3999 there is no numeral, so the field fills with `#`.
            (0, "###############"),
            (-12, "###############"),
            (1234567, "###############"),
        ],
    )
    def test_roman(self, value, want):
        assert _fmt(value, "RN") == want


class TestThroughTheEngine:
    """The template reaches the numeric formatter intact — sqlglot's postgres
    dialect part-converts it to strftime, so `MI` arrived as `%M`."""

    @pytest.mark.parametrize(
        ("sql", "want"),
        [
            ("SELECT to_char(-12::numeric, 'MI999')", "- 12"),
            ("SELECT to_char(-12::numeric, '999MI')", " 12-"),
            ("SELECT to_char(12::numeric, '9999D99')", "   12.00"),
            ("SELECT to_char(1234567::numeric, '9G999G999')", " 1,234,567"),
            ("SELECT to_char(-12::numeric, '999')", " -12"),
            ("SELECT to_char(12::numeric, 'RN')", "            XII"),
        ],
    )
    def test_engine(self, db, sql, want):
        assert db(sql) == want

    def test_timestamps_still_format(self, db):
        assert db("SELECT to_char('2020-01-01 10:30:45'::timestamp, 'YYYY-MM-DD HH24:MI:SS')") == (
            "2020-01-01 10:30:45"
        )
