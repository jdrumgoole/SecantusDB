"""Regenerate the latency-vs-mongod chart + table from benchmark results.

Counterpart of ``concurrency_chart.py`` for the per-operation latency
comparison. Reads ``bench/results/latency.json`` (written by hand from a
``bench.compare_servers`` run — see ``invoke compare-servers``) and rewrites:

- the markdown table and the inline SVG in ``docs/benchmark.md``
- the inline SVG in ``website/themes/secantus/templates/performance.html``

Both SVG blocks share one geometry (bar width = ratio * PX_PER_X - CAP,
mongod's 1x reference line at ``X0 + PX_PER_X``); they differ only in CSS
class prefixes and tooltip mechanism, which is what ``Style`` captures. The
surrounding prose (the "short version" ranges) is hand-maintained — the
script prints the fresh ranges so the prose can be checked against them.

Usage:
    uv run python -m bench.latency_chart          # rewrite from results JSON
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RESULTS = REPO / "bench" / "results" / "latency.json"
DOCS_MD = REPO / "docs" / "benchmark.md"
PERF_HTML = REPO / "website" / "themes" / "secantus" / "templates" / "performance.html"

# Shared geometry (recovered from the committed charts and kept stable so
# diffs stay reviewable): x-axis scale, bar start, rounded-cap width.
X0 = 200.0  # bar start / label anchor
PX_PER_X = 19.04  # pixels per 1x-of-mongod
CAP = 4.0  # rounded end-cap radius (its width is excluded from the h span)
ROW_H = 54.0
BAR_H = 14.0
TOP = 26.0


@dataclass(frozen=True)
class Style:
    svg_class: str
    grid: str
    tick: str
    ref: str
    reflab: str
    lab: str
    val: str
    x: str
    rust_fill: str
    py_fill: str
    tooltip_attr: bool  # data-tip attribute (site) vs <title> child (docs)
    rust_name: str
    py_name: str


SITE = Style(
    svg_class="viz",
    grid="viz-grid",
    tick="viz-tick",
    ref="viz-ref",
    reflab="viz-reflab",
    lab="viz-lab",
    val="viz-val",
    x="viz-x",
    rust_fill="var(--viz-rust)",
    py_fill="var(--viz-py)",
    tooltip_attr=True,
    rust_name="Rust DB",
    py_name="Python DB",
)
DOCS = Style(
    svg_class="dviz",
    grid="dv-grid",
    tick="dv-tick",
    ref="dv-ref",
    reflab="dv-tick",
    lab="dv-lab",
    val="dv-val",
    x="dv-x",
    rust_fill="var(--dv-rust)",
    py_fill="var(--dv-py)",
    tooltip_attr=False,
    rust_name="Rust server",
    py_name="Python server",
)


def _bar(x_ratio: float, y: float, fill: str, tip: str, st: Style) -> str:
    w = max(x_ratio * PX_PER_X - CAP, 1.0)
    path = (
        f'<path d="M{X0:.0f},{y:.0f} h{w:.1f} a{CAP},{CAP} 0 0 1 {CAP},{CAP} '
        f"v{BAR_H - 2 * CAP:.1f} a{CAP},{CAP} 0 0 1 -{CAP},{CAP} "
        f'h-{w:.1f} z" fill="{fill}"'
    )
    if st.tooltip_attr:
        return f'{path} data-tip="{tip}"></path>'
    return f"{path}><title>{tip}</title></path>"


def _val(x_ratio: float, y: float, ratio_text: str, st: Style) -> str:
    x = X0 + max(x_ratio * PX_PER_X - CAP, 1.0) + CAP + 6.0
    return (
        f'<text x="{x:.1f}" y="{y:.0f}" class="{st.val}">'
        f'{ratio_text}<tspan class="{st.x}">x</tspan></text>'
    )


def render_svg(workloads: list[dict], st: Style, aria: str) -> str:
    height = TOP + len(workloads) * ROW_H - 16.0 + 28.0
    view_h = int(round(height / 2) * 2)
    parts = [
        f'<svg viewBox="0 0 760 {view_h}" role="img" aria-label="{aria}" class="{st.svg_class}">'
    ]
    bottom = TOP + len(workloads) * ROW_H - 26.0
    for mult in (5, 10, 15, 20, 25):
        x = X0 + mult * PX_PER_X
        parts.append(f'<line x1="{x:.1f}" y1="18" x2="{x:.1f}" y2="{bottom:.0f}" class="{st.grid}"/>')
        parts.append(
            f'<text x="{x:.1f}" y="{bottom + 16:.0f}" text-anchor="middle" class="{st.tick}">'
            f'{mult}<tspan class="{st.x}">x</tspan></text>'
        )
    ref_x = X0 + PX_PER_X
    parts.append(f'<line x1="{ref_x:.1f}" y1="18" x2="{ref_x:.1f}" y2="{bottom:.0f}" class="{st.ref}"/>')
    parts.append(
        f'<text x="{ref_x:.1f}" y="12" text-anchor="middle" class="{st.reflab}">'
        f'mongod = 1<tspan class="{st.x}">x</tspan></text>'
    )
    y = TOP
    for w in workloads:
        rust_x = w["rust_ms"] / w["mongod_ms"]
        py_x = w["py_ms"] / w["mongod_ms"]
        parts.append(
            f'<text x="{X0 - 10:.0f}" y="{y + 16:.0f}" text-anchor="end" class="{st.lab}">'
            f'{w["label"]}</text>'
        )
        parts.append(_bar(rust_x, y, st.rust_fill, f"{st.rust_name} — {rust_x:.1f}x mongod", st))
        parts.append(_val(rust_x, y + 11, f"{rust_x:.1f}", st))
        parts.append(_bar(py_x, y + 16, st.py_fill, f"{st.py_name} — {py_x:.1f}x mongod", st))
        parts.append(_val(py_x, y + 27, f"{py_x:.1f}", st))
        y += ROW_H
    parts.append("</svg>")
    return "".join(parts)


def render_table(workloads: list[dict]) -> str:
    lines = [
        "| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for w in workloads:
        rust_x = w["rust_ms"] / w["mongod_ms"]
        py_x = w["py_ms"] / w["mongod_ms"]
        lines.append(
            f"| {w['md_label']} | {w['mongod_ms']:.1f} ms | {w['rust_ms']:.1f} ms | "
            f"{rust_x:.1f}× | {w['py_ms']:.1f} ms | {py_x:.1f}× |"
        )
    return "\n".join(lines)


def splice_svg(path: Path, new_svg: str) -> None:
    src = path.read_text()
    out, n = re.subn(r'<svg viewBox="0 0 760 \d+".*?</svg>', new_svg, src, count=1, flags=re.S)
    if n != 1:
        raise SystemExit(f"{path}: expected exactly one latency SVG block to replace")
    path.write_text(out)


def render_html_tbody(workloads: list[dict]) -> str:
    """The site table's <tbody> rows (same data, HTML markup + &hairsp; ratios)."""
    rows = []
    for w in workloads:
        rust_x = w["rust_ms"] / w["mongod_ms"]
        py_x = w["py_ms"] / w["mongod_ms"]
        label = w["label"] + ("&#42;" if "drain" in w["label"] else "")
        rows.append(
            f"          <tr><th>{label}</th><td>{w['mongod_ms']:.1f} ms</td>"
            f"<td>{w['rust_ms']:.1f} ms</td>"
            f'<td>{rust_x:.1f}<span class="unit-x">x</span></td>'
            f"<td>{w['py_ms']:.1f} ms</td>"
            f'<td>{py_x:.1f}<span class="unit-x">x</span></td></tr>'
        )
    return "        <tbody>\n" + "\n".join(rows) + "\n        </tbody>"


def splice_html_tbody(path: Path, new_tbody: str) -> None:
    src = path.read_text()
    out, n = re.subn(r"        <tbody>\n(?:          <tr>.*?\n)+        </tbody>", new_tbody, src, count=1)
    if n != 1:
        raise SystemExit(f"{path}: expected exactly one latency <tbody> to replace")
    path.write_text(out)


def splice_table(path: Path, new_table: str) -> None:
    src = path.read_text()
    out, n = re.subn(
        r"\| Workload \| mongod \| Rust server \| ×mongod \| Python server \| ×mongod \|\n"
        r"(?:\|[^\n]*\|\n)+",
        new_table + "\n",
        src,
        count=1,
    )
    if n != 1:
        raise SystemExit(f"{path}: expected exactly one latency table to replace")
    path.write_text(out)


def main() -> None:
    data = json.loads(RESULTS.read_text())
    workloads = data["workloads"]
    splice_svg(
        DOCS_MD,
        render_svg(workloads, DOCS, "Per-operation latency as a multiple of mongod"),
    )
    splice_table(DOCS_MD, render_table(workloads))
    splice_svg(
        PERF_HTML,
        render_svg(
            workloads, SITE, "Per-operation latency as a multiple of mongod, per workload"
        ),
    )
    splice_html_tbody(PERF_HTML, render_html_tbody(workloads))
    rust = [w["rust_ms"] / w["mongod_ms"] for w in workloads]
    py = [w["py_ms"] / w["mongod_ms"] for w in workloads]
    ratio = [p / r for r, p in zip(rust, py, strict=True)]
    print(f"rewrote {DOCS_MD.relative_to(REPO)} and {PERF_HTML.relative_to(REPO)}")
    print(
        f"prose check — Rust: {min(rust):.1f}x–{max(rust):.1f}x of mongod; "
        f"Python: {min(py):.1f}x–{max(py):.1f}x; "
        f"Rust vs Python: {min(ratio):.1f}x–{max(ratio):.1f}x"
    )
    print("review the hand-maintained prose around both charts against these ranges")


if __name__ == "__main__":
    main()
