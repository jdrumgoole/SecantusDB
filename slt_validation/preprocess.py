"""Preprocess the sqlite sqllogictest corpus for sqllogictest-rs.

The corpus itself is NEVER modified (``vendor/sqllogictest`` stays pristine) —
this rewrites a copy under ``.validation/slt-corpus/``. Three runner
incompatibilities, established empirically (tasks/sql-gauges-plan.md §6):

1. Trailing comments on condition lines (``skipif mysql # comment``) fail the
   0.29.1 parser — strip everything from `` #`` on ``skipif``/``onlyif`` lines.
2. Expected result blocks: the sqlite corpus writes ONE VALUE PER LINE in
   row-major order; for ``nosort``/``rowsort`` records sqllogictest-rs compares
   ONE ROW PER LINE (values whitespace-separated) — chunk the value lines into
   rows of N where N = len(type string) from the ``query`` line. ``valuesort``
   records ARE compared value-per-line, so those stay untouched, as do hash
   blocks (``N values hashing to <md5>`` — the runner hashes the
   one-value-per-line canonical form itself).
3. sqlite's runner defaults ``hash-threshold`` to 8; sqllogictest-rs defaults
   to 0 (never hash) — a file with hash-form expectations but no directive
   (select1.test) gets the sqlite default injected.
"""

from __future__ import annotations

import re
from pathlib import Path

HASH_RE = re.compile(r"^\d+ values hashing to [0-9a-f]{32}$")
COND_RE = re.compile(r"^(skipif|onlyif)\s+\S+")


def preprocess(text: str) -> str:
    out: list[str] = []
    lines = text.split("\n")
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        m = COND_RE.match(line)
        if m and " #" in line:
            out.append(line.split(" #", 1)[0].rstrip())
            i += 1
            continue
        if line.startswith("query "):
            parts = line.split()
            ncols = len(parts[1]) if len(parts) > 1 else 1
            sortmode = (
                parts[2] if len(parts) > 2 and parts[2] in ("nosort", "rowsort", "valuesort") else "nosort"
            )
            out.append(line)
            i += 1
            while i < n and lines[i] != "----":
                out.append(lines[i])
                i += 1
            if i >= n:
                break
            out.append(lines[i])  # ----
            i += 1
            block: list[str] = []
            while i < n and lines[i] != "":
                block.append(lines[i])
                i += 1
            if len(block) == 1 and HASH_RE.match(block[0]):
                out.extend(block)
            elif ncols > 1 and sortmode != "valuesort":
                for j in range(0, len(block), ncols):
                    out.append(" ".join(block[j : j + ncols]))
            else:
                out.extend(block)
            continue
        out.append(line)
        i += 1
    text_out = "\n".join(out)
    if "values hashing to" in text_out and not any(
        line.startswith("hash-threshold") for line in out
    ):
        text_out = "hash-threshold 8\n\n" + text_out
    return text_out


def preprocess_files(src_root: Path, dst_root: Path, rel_paths: list[str]) -> None:
    """Preprocess ``rel_paths`` (relative to ``src_root``) into ``dst_root``."""
    for rel in rel_paths:
        src = src_root / rel
        dst = dst_root / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(preprocess(src.read_text()))
