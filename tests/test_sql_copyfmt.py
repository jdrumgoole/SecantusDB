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
