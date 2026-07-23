"""Parse the gauges' generated validation reports into a score.

Each gauge writes ``docs/validation-report*.md`` with a per-category table whose
last row is a bolded ``**Overall**``. The Ops Board reads that row so the gauge
matrix can show how a driver actually scored, not merely that it ran.

The table shape is **not** uniform across gauges: the pymongo report carries an
extra ``Errored`` column, and the first column is variously ``Category`` /
``Package`` / ``Suite``. So the parser reads the header row and maps cells by
NAME rather than by position — a positional parser silently mis-reads the
6-column reports as soon as it's tuned for the 7-column one.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

_GENERATED_RE = re.compile(r"^Generated\s+(\S+)", re.M)


@dataclass(frozen=True)
class GaugeReport:
    passed: int
    failed: int
    skipped: int
    total: int
    rate: float  # percent, as printed in the report
    errored: int = 0
    generated: str = ""
    path: str = ""

    @property
    def ran(self) -> int:
        """Tests that actually executed (total minus skipped)."""
        return max(0, self.total - self.skipped)

    @property
    def summary(self) -> str:
        return f"{self.passed}/{self.ran} · {self.rate:.1f}%"

    @property
    def clean(self) -> bool:
        return self.failed == 0 and self.errored == 0


def report_filename(gauge_key: str, server: str) -> str:
    """Report file for a gauge/server pair, relative to ``docs/``.

    pymongo is the unsuffixed base report (it's the headline gauge); every other
    gauge is suffixed by its key. The Rust-server runs add ``-rust-server``.
    """
    stem = "validation-report" if gauge_key == "pymongo" else f"validation-report-{gauge_key}"
    if server == "rust":
        stem += "-rust-server"
    return f"{stem}.md"


def _cells(line: str) -> list[str]:
    parts = [c.strip() for c in line.strip().strip("|").split("|")]
    return [c.replace("*", "").strip() for c in parts]


def _to_int(value: str) -> int | None:
    try:
        return int(value.replace(",", ""))
    except ValueError:
        return None


def _is_separator(cells: list[str]) -> bool:
    return bool(cells) and all(set(c) <= set("-: ") and c for c in cells)


def parse(text: str, *, path: str = "") -> GaugeReport | None:
    """Extract the overall counts, or None if the report has no usable row.

    Handles the three report shapes in this repo:

    * a per-category table ending in a bolded ``**Overall**`` row (most gauges);
    * the same, plus an extra ``Errored`` column (pymongo);
    * a **label-less single-row summary** whose header starts at ``Passed``
      (the C++ gauge) — there is no ``Overall`` row to look for.
    """
    rows = [_cells(line) for line in text.splitlines() if line.strip().startswith("|")]
    header: list[str] | None = None
    data: list[list[str]] = []
    for cells in rows:
        if header is None:
            if "Passed" in cells:
                header = cells
            continue
        if not _is_separator(cells):
            data.append(cells)
    if header is None or not data:
        return None

    if header and header[0] == "Passed":
        overall = data[0]  # label-less summary: the single data row IS the total
    else:
        overall = next((c for c in data if c and c[0].lower() == "overall"), [])
    if not overall:
        return None

    by_name = dict(zip(header, overall, strict=False))
    passed = _to_int(by_name.get("Passed", ""))
    total = _to_int(by_name.get("Total", ""))
    if passed is None or total is None:
        return None
    rate_raw = by_name.get("Pass rate", "").rstrip("%")
    try:
        rate = float(rate_raw)
    except ValueError:
        rate = (passed / total * 100.0) if total else 0.0
    gen = _GENERATED_RE.search(text)
    return GaugeReport(
        passed=passed,
        failed=_to_int(by_name.get("Failed", "")) or 0,
        skipped=_to_int(by_name.get("Skipped", "")) or 0,
        total=total,
        rate=rate,
        errored=_to_int(by_name.get("Errored", "")) or 0,
        generated=gen.group(1) if gen else "",
        path=path,
    )


def load(repo_root: str | Path, gauge_key: str, server: str) -> GaugeReport | None:
    """Read + parse a gauge's report; None when it hasn't been run here."""
    name = report_filename(gauge_key, server)
    path = Path(repo_root) / "docs" / name
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    return parse(text, path=name)


__all__ = ["GaugeReport", "parse", "load", "report_filename"]
