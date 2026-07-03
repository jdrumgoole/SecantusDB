"""Parse and format ``COPY`` stream payloads — the text and CSV on-the-wire
representations Postgres uses for ``COPY … FROM/TO STDIN/STDOUT``.

Pure string in / string out (no I/O, no SQL): the wire server hands raw bytes
here and gets back rows of string cells (a ``None`` cell is SQL NULL), and vice
versa. Kept small and dependency-free — the default *text* format plus CSV.
"""

from __future__ import annotations

import csv
import io

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
) -> list[list[str | None]]:
    """Parse COPY CSV format. An unquoted field equal to ``null`` is NULL; a
    quoted empty field is the empty string. ``header`` skips the first line."""
    reader = csv.reader(io.StringIO(data), delimiter=delimiter, quotechar=quote)
    raw_rows = [r for r in reader if r]  # csv yields [] for blank trailing lines
    if header and raw_rows:
        raw_rows = raw_rows[1:]
    # csv can't tell an unquoted empty field from a quoted one, so re-scan the
    # source to know which empty cells were quoted (quoted empty = "" not NULL).
    quoted_empties = _quoted_empty_positions(data, delimiter, quote, header)
    rows: list[list[str | None]] = []
    for i, row in enumerate(raw_rows):
        cells: list[str | None] = []
        for j, cell in enumerate(row):
            if cell == null and (i, j) not in quoted_empties:
                cells.append(None)
            else:
                cells.append(cell)
        rows.append(cells)
    return rows


def _quoted_empty_positions(
    data: str, delimiter: str, quote: str, header: bool
) -> set[tuple[int, int]]:
    """Positions (row, col) where an empty CSV field was written quoted (`""`) —
    those are the empty string, not NULL."""
    positions: set[tuple[int, int]] = set()
    lines = [ln for ln in data.split("\n") if ln != ""]
    start = 1 if header else 0
    for ri, line in enumerate(lines[start:]):
        col = 0
        i = 0
        n = len(line)
        while i <= n:
            if (
                i < n
                and line[i] == quote
                and i + 1 < n
                and line[i + 1] == quote
                and (i + 2 >= n or line[i + 2] == delimiter)
            ):
                positions.add((ri, col))
                i += 2
            # advance to next delimiter
            while i < n and line[i] != delimiter:
                if line[i] == quote:  # skip a quoted region
                    i += 1
                    while i < n and line[i] != quote:
                        i += 1
                i += 1
            col += 1
            i += 1
    return positions


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
    """Render rows as COPY CSV format. A NULL cell writes ``null`` (unquoted); a
    non-NULL value is quoted only when it needs to be (csv.QUOTE_MINIMAL)."""
    buf = io.StringIO()
    writer = csv.writer(buf, delimiter=delimiter, quotechar=quote, lineterminator="\n")
    if header is not None:
        writer.writerow(header)
    for row in rows:
        writer.writerow([null if v is None else v for v in row])
    return buf.getvalue()
