"""Regenerate the concurrency scaling graphs from benchmark results.

Reads the JSON written by ``bench.concurrency --json`` (all four
servers: python, rust, rust-async, mongod) and rewrites the
marker-delimited chart + data-table blocks in the two surfaces that
show the N-writer scaling story:

- ``website/themes/secantus/templates/performance.html`` (the marketing
  site's "Throughput under concurrent writers" section)
- ``docs/concurrency.md`` (the Sphinx concurrency deep-dive)

Each surface carries two marked regions::

    <!-- concurrency-viz:begin -->   ... legend + inline SVG ...   <!-- concurrency-viz:end -->
    <!-- concurrency-table:begin --> ... absolute docs/s table ... <!-- concurrency-table:end -->

Only the marked regions are rewritten; the surrounding prose is
hand-maintained and cites headline numbers, so the CLI prints the new
headlines and reminds you to re-read the prose after a refresh.

Invoked by ``invoke concurrency-refresh`` (which runs the benchmark
first); run directly to re-render from an existing results file.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_RESULTS = REPO_ROOT / "bench" / "results" / "concurrency.json"
PERFORMANCE_HTML = REPO_ROOT / "website" / "themes" / "secantus" / "templates" / "performance.html"
DOCS_MD = REPO_ROOT / "docs" / "concurrency.md"

VIZ_BEGIN = "<!-- concurrency-viz:begin -->"
VIZ_END = "<!-- concurrency-viz:end -->"
TABLE_BEGIN = "<!-- concurrency-table:begin -->"
TABLE_END = "<!-- concurrency-table:end -->"

# Draw order matches the hand-authored original: solid lines first,
# the dashed async variant on top.
DRAW_ORDER = ["mongod", "rust", "python", "rust-async"]
REQUIRED_SERVERS = frozenset(DRAW_ORDER)

# Chart geometry (identical to the original hand-authored SVG).
X0, X1 = 56.0, 668.0
Y_REF = 214.0  # the 1x reference line
Y_TOP = 21.0  # highest allowed data point
Y_BOTTOM = 250.0  # lowest allowed data point (x-axis ticks sit at 290)
LABEL_X = 674
LABEL_MIN_GAP = 14.0

STYLES = {
    "website": {
        "prefix": "viz",
        "wrap_class": "viz-wrap",
        "legend_class": "viz-legend",
        "svg_class": "viz",
        "dot_class": ' class="viz-dot"',
        "var": {
            "mongod": "--viz-mongo",
            "rust": "--viz-rust",
            "rust-async": "--viz-rust",
            "python": "--viz-py",
        },
        "names": {
            "mongod": "mongod",
            "rust": "Rust DB",
            "rust-async": "Rust DB (async + non-logged oplog)",
            "python": "Python DB",
        },
        "legend_names": {
            "mongod": "mongod",
            "rust": "Rust DB",
            "rust-async": "Rust DB &mdash; async stack",
            "python": "Python DB",
        },
        "label_names": {
            "mongod": "mongod",
            "rust": "Rust",
            "rust-async": "async",
            "python": "Python",
        },
        "rate_phrase": "its 1-writer rate",
        "tooltip": "data-tip",
        "aria": "Throughput scaling relative to each server’s own single-writer rate",
    },
    "docs": {
        "prefix": "dv",
        "wrap_class": "dviz-wrap",
        "legend_class": "dv-legend",
        "svg_class": "dviz",
        "dot_class": "",
        "var": {
            "mongod": "--dv-mongo",
            "rust": "--dv-rust",
            "rust-async": "--dv-rust",
            "python": "--dv-py",
        },
        "names": {
            "mongod": "mongod",
            "rust": "Rust server",
            "rust-async": "Rust server (async + non-logged oplog)",
            "python": "Python server",
        },
        "legend_names": {
            "mongod": "mongod",
            "rust": "Rust server",
            "rust-async": "Rust server — async stack",
            "python": "Python server",
        },
        "label_names": {
            "mongod": "mongod",
            "rust": "Rust",
            "rust-async": "async",
            "python": "Python",
        },
        "rate_phrase": "its single-writer rate",
        "tooltip": "title",
        "aria": "Throughput scaling relative to each server single-writer rate",
    },
}


def scaling_ratios(rates: list[float]) -> list[float]:
    base = rates[0]
    if base <= 0:
        raise ValueError(f"single-writer rate must be positive, got {base}")
    return [r / base for r in rates]


def _y_scale(all_ratios: list[float]) -> float:
    """Pixels per 1.0x of scaling, fitted so every point stays on-chart."""
    max_r, min_r = max(all_ratios), min(all_ratios)
    scale = 120.0
    if max_r > 1:
        scale = min(scale, (Y_REF - Y_TOP) / (max_r - 1))
    if min_r < 1:
        scale = min(scale, (Y_BOTTOM - Y_REF) / (1 - min_r))
    return scale


def _nudge_labels(entries: list[tuple[str, float]]) -> dict[str, float]:
    """Push end-of-line label baselines apart to at least LABEL_MIN_GAP."""
    ordered = sorted(entries, key=lambda e: e[1])
    ys = [y for _, y in ordered]
    for i in range(1, len(ys)):
        ys[i] = max(ys[i], ys[i - 1] + LABEL_MIN_GAP)
    overflow = ys[-1] - (Y_BOTTOM + 40)
    if overflow > 0:
        ys = [y - overflow for y in ys]
        for i in range(len(ys) - 2, -1, -1):
            ys[i] = min(ys[i], ys[i + 1] - LABEL_MIN_GAP)
    return {key: y for (key, _), y in zip(ordered, ys, strict=True)}


def render_viz(results: dict, style_name: str) -> str:
    st = STYLES[style_name]
    p = st["prefix"]
    writers: list[int] = results["meta"]["writers"]
    rates = {s: results["servers"][s]["docs_per_sec"] for s in DRAW_ORDER}
    ratios = {s: scaling_ratios(rates[s]) for s in DRAW_ORDER}

    span = max(writers) - min(writers)
    if span <= 0:
        raise ValueError("need at least two writer counts to draw a scaling chart")
    xs = [X0 + (n - min(writers)) / span * (X1 - X0) for n in writers]
    scale = _y_scale([r for rs in ratios.values() for r in rs])

    def y(ratio: float) -> float:
        return Y_REF - (ratio - 1) * scale

    parts: list[str] = []
    legend = "".join(
        f'<span><span class="chip" style="background:var({st["var"][s]})"></span>'
        f"{st['legend_names'][s]}</span>"
        if s != "rust-async"
        else f'<span><span class="chip" style="background:transparent;'
        f'border:2px dashed var({st["var"][s]});box-sizing:border-box"></span>'
        f"{st['legend_names'][s]}</span>"
        for s in ["mongod", "rust", "python", "rust-async"]
    )
    parts.append(f'<div class="{st["wrap_class"]}"><div class="{st["legend_class"]}">{legend}</div>')  # noqa: E501
    parts.append(
        f'<svg viewBox="0 0 790 320" role="img" aria-label="{st["aria"]}" class="{st["svg_class"]}">'  # noqa: E501
    )

    parts.append(f'<line x1="{X0:.0f}" y1="{Y_REF:.0f}" x2="{X1:.0f}" y2="{Y_REF:.0f}" class="{p}-ref"/>')  # noqa: E501
    parts.append(
        f'<text x="48" y="{Y_REF + 4:.0f}" text-anchor="end" class="{p}-tick">1<tspan class="{p}-x">x</tspan></text>'  # noqa: E501
    )
    k = 2
    while y(k) >= 35:
        gy = y(k)
        parts.append(f'<line x1="{X0:.0f}" y1="{gy:.0f}" x2="{X1:.0f}" y2="{gy:.0f}" class="{p}-grid"/>')  # noqa: E501
        parts.append(
            f'<text x="48" y="{gy + 4:.0f}" text-anchor="end" class="{p}-tick">{k}<tspan class="{p}-x">x</tspan></text>'  # noqa: E501
        )
        k += 1

    for n, x in zip(writers, xs, strict=True):
        parts.append(f'<text x="{x:.0f}" y="290" text-anchor="middle" class="{p}-tick">{n}</text>')
    parts.append(
        f'<text x="{(X0 + X1) / 2:.0f}" y="310" text-anchor="middle" class="{p}-lab">concurrent writers</text>'  # noqa: E501
    )

    for s in DRAW_ORDER:
        var = f"var({st['var'][s]})"
        pts = [(x, y(r)) for x, r in zip(xs, ratios[s], strict=True)]
        d = "M" + " L".join(f"{x:.1f},{py:.1f}" for x, py in pts)
        dash = ' stroke-dasharray="6 4"' if s == "rust-async" else ""
        parts.append(f'<path d="{d}" fill="none" stroke="{var}" stroke-width="2"{dash}/>')
        for (x, py), n, r in zip(pts, writers, ratios[s], strict=True):
            tip = (
                f"{st['names'][s]} — {n} writer{'s' if n != 1 else ''}: "
                f"{r:.2f}x {st['rate_phrase']}"
            )
            if st["tooltip"] == "data-tip":
                parts.append(
                    f'<circle cx="{x:.1f}" cy="{py:.1f}" r="4.5" fill="{var}"'
                    f'{st["dot_class"]} data-tip="{tip}"/>'
                )
            else:
                parts.append(
                    f'<circle cx="{x:.1f}" cy="{py:.1f}" r="4.5" fill="{var}"'
                    f"{st['dot_class']}><title>{tip}</title></circle>"
                )

    label_pos = _nudge_labels([(s, y(ratios[s][-1]) + 4) for s in DRAW_ORDER])
    for s in DRAW_ORDER:
        parts.append(
            f'<text x="{LABEL_X}" y="{label_pos[s]:.0f}" class="{p}-val" fill="var({st["var"][s]})">'  # noqa: E501
            f"{st['label_names'][s]} {ratios[s][-1]:.1f}<tspan class=\"{p}-x\">x</tspan></text>"
        )

    parts.append("</svg></div>")
    return "".join(parts)


def _fmt_rate(rate: float) -> str:
    return f"{round(rate / 100) * 100:,d}"


def render_website_table(results: dict) -> str:
    writers: list[int] = results["meta"]["writers"]
    order = ["mongod", "rust", "rust-async", "python"]
    rows = []
    for i, n in enumerate(writers):
        cells = "".join(
            f"<td>{_fmt_rate(results['servers'][s]['docs_per_sec'][i])}</td>" for s in order
        )
        rows.append(f"          <tr><th>{n}</th>{cells}</tr>")
    body = "\n".join(rows)
    return (
        '<details><summary style="cursor:pointer;color:var(--slate-500);margin:10px 0;">'
        "Data table (absolute docs/second)</summary>\n"
        '    <div class="compare-wrap">\n'
        '      <table class="compare-table">\n'
        "        <thead>\n"
        "          <tr><th>Writers</th><th>mongod</th><th>Rust DB</th>"
        "<th>Rust DB &mdash; async stack</th><th>Python DB</th></tr>\n"
        "        </thead>\n"
        "        <tbody>\n"
        f"{body}\n"
        "        </tbody>\n"
        "      </table>\n"
        "    </div>\n"
        "    </details>"
    )


def render_docs_table(results: dict) -> str:
    writers: list[int] = results["meta"]["writers"]
    order = ["python", "rust", "rust-async", "mongod"]
    lines = [
        "| N writers | Python server (docs/s) | Rust server (docs/s) | Rust — async stack (docs/s) | mongod (docs/s) |",  # noqa: E501
        "|---|---:|---:|---:|---:|",
    ]
    for i, n in enumerate(writers):
        cells = " | ".join(
            _fmt_rate(results["servers"][s]["docs_per_sec"][i]) for s in order
        )
        lines.append(f"| {n} | {cells} |")
    return "\n".join(lines)


def replace_block(text: str, begin: str, end: str, content: str, *, path: Path) -> str:
    pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.DOTALL)
    hits = pattern.findall(text)
    if len(hits) != 1:
        raise SystemExit(
            f"{path}: expected exactly one {begin} ... {end} region, found {len(hits)}"
        )
    return pattern.sub(lambda _m: f"{begin}\n{content}\n{end}", text)


def refresh_surface(path: Path, viz: str, table: str) -> bool:
    """Rewrite both marked regions in ``path``; return True if it changed."""
    original = path.read_text(encoding="utf-8")
    updated = replace_block(original, VIZ_BEGIN, VIZ_END, viz, path=path)
    updated = replace_block(updated, TABLE_BEGIN, TABLE_END, table, path=path)
    if updated != original:
        path.write_text(updated, encoding="utf-8", newline="\n")
        return True
    return False


def load_results(path: Path) -> dict:
    results = json.loads(path.read_text(encoding="utf-8"))
    missing = REQUIRED_SERVERS - results.get("servers", {}).keys()
    if missing:
        raise SystemExit(
            f"{path}: results are missing server(s) {sorted(missing)} — run "
            f"`uv run python -m bench.concurrency --server all --json {path}`"
        )
    writers = results["meta"]["writers"]
    for s, data in results["servers"].items():
        if len(data["docs_per_sec"]) != len(writers):
            raise SystemExit(f"{path}: server {s!r} has {len(data['docs_per_sec'])} rates for {len(writers)} writer counts")  # noqa: E501
    return results


def print_headlines(results: dict) -> None:
    writers = results["meta"]["writers"]
    print(f"\nheadlines (writers {writers[0]} → {writers[-1]}):")
    for s in DRAW_ORDER:
        rates = results["servers"][s]["docs_per_sec"]
        ratio = scaling_ratios(rates)[-1]
        print(
            f"  {s:<11} {rates[0]:>10,.0f} docs/s single-writer, "
            f"{rates[-1]:>10,.0f} docs/s at {writers[-1]} ({ratio:.2f}x)"
        )
    print(
        "\nThe prose around both charts cites these numbers — re-read the\n"
        "surrounding paragraphs in performance.html and docs/concurrency.md\n"
        "and update any that drifted.\n"
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="concurrency_chart",
        description="Refresh the concurrency graphs from bench.concurrency --json results.",
    )
    ap.add_argument("--results", type=Path, default=DEFAULT_RESULTS,
                    help=f"Results JSON from bench.concurrency (default: {DEFAULT_RESULTS}).")
    ap.add_argument("--performance-html", type=Path, default=PERFORMANCE_HTML,
                    help="Path to the website performance template.")
    ap.add_argument("--docs-md", type=Path, default=DOCS_MD,
                    help="Path to docs/concurrency.md.")
    args = ap.parse_args(argv)

    results = load_results(args.results)
    surfaces = [
        (args.performance_html, render_viz(results, "website"), render_website_table(results)),
        (args.docs_md, render_viz(results, "docs"), render_docs_table(results)),
    ]
    for path, viz, table in surfaces:
        changed = refresh_surface(path, viz, table)
        print(f"{'rewrote' if changed else 'unchanged'} {path}")
    print_headlines(results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
