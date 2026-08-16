"""Parse and format ``COPY`` stream payloads — the text and CSV on-the-wire
representations Postgres uses for ``COPY … FROM/TO STDIN/STDOUT``.

Pure string in / string out (no I/O, no SQL): the wire server hands raw bytes
here and gets back rows of string cells (a ``None`` cell is SQL NULL), and vice
versa. Kept small and dependency-free — the default *text* format plus CSV.
"""

from __future__ import annotations

from secantus.sql import errors

# Text-format backslash escapes (Postgres COPY TEXT). Applied on read (unescape)
# and mirrored on write (escape).
_TEXT_UNESCAPE = {"b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t", "v": "\v", "\\": "\\"}
_TEXT_ESCAPE = {"\\": "\\\\", "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t"}


def _unescape_text_field(field: str) -> str:
    out: list[str] = []
    i = 0
    while i < len(field):
        ch = field[i]
        if ch == "\\" and i + 1 < len(field):
            nxt = field[i + 1]
            # PG's COPY TEXT byte escapes: \xH{1,2} hex and \O{1,3} octal
            # (the pgtest copy corpus loads \x54 and \011\143 into bytea AND
            # text columns and reads the decoded bytes back).
            if nxt in ("x", "X"):
                j = i + 2
                while j < len(field) and j < i + 4 and field[j] in "0123456789abcdefABCDEF":
                    j += 1
                if j > i + 2:
                    out.append(chr(int(field[i + 2 : j], 16)))
                    i = j
                    continue
            if nxt in "01234567":
                j = i + 2
                while j < len(field) and j < i + 4 and field[j] in "01234567":
                    j += 1
                out.append(chr(int(field[i + 1 : j], 8)))
                i = j
                continue
            out.append(_TEXT_UNESCAPE.get(nxt, nxt))
            i += 2
        else:
            out.append(ch)
            i += 1
    return "".join(out)


def _escape_text_field(value: str) -> str:
    return "".join(_TEXT_ESCAPE.get(ch, ch) for ch in value)


def parse_text(data: str, *, delimiter: str = "\t", null: str = "\\N") -> list[list[str | None]]:
    """Parse COPY *text* format into rows of cells (``None`` = NULL). A trailing
    ``\\.`` line (the copy-from-file terminator) and a final empty line are
    ignored."""
    rows: list[list[str | None]] = []
    for line in data.split("\n"):
        if line == "\\." or line == "":
            continue
        cells: list[str | None] = []
        for raw in line.split(delimiter):
            cells.append(None if raw == null else _unescape_text_field(raw))
        rows.append(cells)
    return rows


def parse_csv(
    data: str,
    *,
    delimiter: str = ",",
    null: str = "",
    header: bool = False,
    quote: str = '"',
    escape: str | None = None,
) -> list[list[str | None]]:
    r"""Parse COPY CSV format. Quoting decides NULL-ness like PG: an UNQUOTED
    field equal to ``null`` is NULL; a quoted field never is (``"N"`` with
    ``NULL 'N'`` is the string N; a quoted empty field is the empty string).
    ``header`` skips the first line. ``escape`` defaults to the quote char
    ("" doubling); a different escape char switches to PG's escape semantics
    (escape+quote / escape+escape literal, bare quote ends the field). A line
    that is exactly ``\.`` ends the data (the copy-from-file terminator)."""
    lines = data.split("\n")
    for i, ln in enumerate(lines):
        if ln.rstrip("\r") == "\\.":
            data = "\n".join(lines[:i])
            if data and not data.endswith("\n"):
                data += "\n"
            break
    return _parse_csv_escape(
        data,
        delimiter=delimiter,
        null=null,
        header=header,
        quote=quote,
        escape=escape if escape is not None else quote,
    )


def _parse_csv_escape(
    data: str,
    *,
    delimiter: str,
    null: str,
    header: bool,
    quote: str,
    escape: str,
) -> list[list[str | None]]:
    """PG's CopyReadAttributesCSV as a state machine: inside a quoted field,
    escape followed by the quote or escape char is that literal char (with
    escape == quote this IS "" doubling); otherwise a bare quote ends the
    field. An unquoted field equal to ``null`` is NULL — a quoted one never
    is. The pgtest copy corpus pins the exact cell values."""
    rows: list[list[str | None]] = []
    row: list[str | None] = []
    cur: list[str] = []
    in_quotes = False
    was_quoted = False

    def end_field() -> None:
        nonlocal cur, was_quoted
        s = "".join(cur)
        row.append(s if (was_quoted or s != null) else None)
        cur = []
        was_quoted = False

    i, n = 0, len(data)
    while i < n:
        c = data[i]
        if in_quotes:
            if c == escape and i + 1 < n and data[i + 1] in (quote, escape):
                cur.append(data[i + 1])
                i += 2
                continue
            if c == quote:
                in_quotes = False
                i += 1
                continue
            cur.append(c)
            i += 1
            continue
        if c == quote and not cur and not was_quoted:
            in_quotes = True
            was_quoted = True
            i += 1
            continue
        if c == delimiter:
            end_field()
            i += 1
            continue
        if c == "\n":
            end_field()
            rows.append(row)
            row = []
            i += 1
            continue
        if c == "\r":
            i += 1
            continue
        cur.append(c)
        i += 1
    if in_quotes:
        raise errors.SQLError("22P04", "unterminated CSV quoted field")
    if cur or was_quoted or row:
        end_field()
        rows.append(row)
    if header and rows:
        rows = rows[1:]
    return rows


def format_text(rows: list[list[str | None]], *, delimiter: str = "\t", null: str = "\\N") -> str:
    """Render rows as COPY *text* format (trailing newline per row)."""
    out: list[str] = []
    for row in rows:
        cells = [null if v is None else _escape_text_field(v) for v in row]
        out.append(delimiter.join(cells))
    return "".join(line + "\n" for line in out)


def format_csv(
    rows: list[list[str | None]],
    *,
    delimiter: str = ",",
    null: str = "",
    header: list[str] | None = None,
    quote: str = '"',
) -> str:
    """Render rows as COPY CSV format. A NULL cell writes ``null`` (unquoted);
    a non-NULL EMPTY string is force-quoted (``""``) so it stays distinct from
    NULL — csv.QUOTE_MINIMAL would write both bare, and PG's COPY CSV output
    quotes the empty string (pgtest copy corpus reads the bytes verbatim).
    Other values are quoted only when they need to be, including a value that
    would collide with the ``null`` spelling."""

    def cell(v: str | None) -> str:
        if v is None:
            return null
        s = str(v)
        if s == "":
            return quote * 2
        if delimiter in s or quote in s or "\n" in s or "\r" in s or (null != "" and s == null):
            return quote + s.replace(quote, quote * 2) + quote
        return s

    lines = []
    if header is not None:
        lines.append(delimiter.join(cell(h) for h in header))
    for row in rows:
        lines.append(delimiter.join(cell(v) for v in row))
    return "".join(line + "\n" for line in lines)
