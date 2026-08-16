"""Unit tests for ``secantus.sql.copyfmt`` — the COPY text / CSV row codec."""

from __future__ import annotations

from secantus.sql import copyfmt


def test_parse_text_basic():
    assert copyfmt.parse_text("1\talice\tt\n2\tbob\tf\n") == [
        ["1", "alice", "t"],
        ["2", "bob", "f"],
    ]


def test_parse_text_null_and_escapes():
    # \N is NULL; \t and \\ inside a field unescape.
    assert copyfmt.parse_text("1\t\\N\n") == [["1", None]]
    assert copyfmt.parse_text("a\\tb\tc\n") == [["a\tb", "c"]]


def test_parse_text_ignores_terminator_and_blank():
    assert copyfmt.parse_text("1\tx\n\\.\n") == [["1", "x"]]


def test_format_text_escapes_and_null():
    assert copyfmt.format_text([["a\tb", None]]) == "a\\tb\t\\N\n"


def test_csv_null_vs_empty_string():
    # An unquoted empty field is NULL; a quoted empty field is the empty string.
    assert copyfmt.parse_csv('1,,x\n2,"",y\n') == [["1", None, "x"], ["2", "", "y"]]


def test_csv_header_skipped():
    assert copyfmt.parse_csv("id,name\n1,alice\n", header=True) == [["1", "alice"]]


def test_format_csv_quotes_when_needed():
    assert copyfmt.format_csv([["has,comma", "x"]]) == '"has,comma",x\n'


def test_format_csv_header_and_null():
    out = copyfmt.format_csv([["1", None]], header=["id", "note"])
    assert out == "id,note\n1,\n"


def test_text_roundtrip():
    rows = [["1", "a\tb", None], ["2", "plain", "z"]]
    assert copyfmt.parse_text(copyfmt.format_text(rows)) == rows


def test_csv_quoted_null_token_is_not_null():
    # pgtest copy corpus: with NULL 'N', an unquoted N is NULL but a QUOTED
    # "N" is the string N, and an unquoted empty cell is the empty string.
    assert copyfmt.parse_csv('4,""\n5,\n6,N\n7,"N"\n', null="N") == [
        ["4", ""],
        ["5", ""],
        ["6", None],
        ["7", "N"],
    ]


def test_csv_custom_escape_char():
    # pgtest copy corpus: CSV ESCAPE 'x' — escape+quote / escape+escape are
    # literal inside a quoted field; a bare quote ends the field.
    assert copyfmt.parse_csv('1,"x""\n2,"xxx","\n3,"xxx",xx"\n', escape="x") == [
        ["1", '"'],
        ["2", 'x",'],
        ["3", 'x",x'],
    ]


def test_csv_terminator_line_ends_data():
    assert copyfmt.parse_csv("1,a\n\\.\n2,b\n") == [["1", "a"]]


def test_csv_unterminated_quote_is_22P04():
    import pytest

    from secantus.sql import errors

    with pytest.raises(errors.SQLError) as exc:
        copyfmt.parse_csv('1,"one\n')
    assert exc.value.sqlstate == "22P04"


def test_format_csv_quotes_empty_string():
    # PG's COPY CSV output writes a non-NULL empty string as "" (quoted) so
    # it stays distinct from NULL (bare empty).
    assert copyfmt.format_csv([["2", ""], ["3", None]]) == '2,""\n3,\n'


def test_text_hex_and_octal_escapes():
    # pgtest copy corpus: \xHH and \OOO byte escapes decode in TEXT mode.
    assert copyfmt.parse_text("2,two\\x54\n3,ab\\011\\143d\n", delimiter=",", null="") == [
        ["2", "twoT"],
        ["3", "ab\tcd"],
    ]


def test_csv_custom_quote_char():
    assert copyfmt.parse_csv("1,|a,b|\n", quote="|") == [["1", "a,b"]]
    assert copyfmt.format_csv([["1", "a,b"]], quote="|") == "1,|a,b|\n"
