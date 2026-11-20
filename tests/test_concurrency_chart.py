"""Unit tests for bench.concurrency_chart — the concurrency-graph generator.

These never run the benchmark; they pin the render/replace logic and act
as a drift detector: the committed chart blocks in
``website/themes/secantus/templates/performance.html`` and
``docs/concurrency.md`` must be exactly what the committed
``bench/results/concurrency.json`` renders to.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest
from bench.concurrency import assemble_results, parse_mongod_version, parse_rust_version
from bench.concurrency_chart import (
    DEFAULT_RESULTS,
    DOCS_MD,
    PERFORMANCE_HTML,
    TABLE_BEGIN,
    TABLE_END,
    VIZ_BEGIN,
    VIZ_END,
    _nudge_labels,
    load_results,
    refresh_surface,
    render_docs_table,
    render_viz,
    render_website_table,
    replace_block,
    scaling_ratios,
)


@pytest.fixture()
def results() -> dict:
    return {
        "meta": {
            "duration": 30.0,
            "batch": 100,
            "writers": [1, 2, 4, 8],
            "shared_collection": False,
            "runs": 1,
        },
        "servers": {
            "python": {"docs_per_sec": [12100.0, 8900.0, 10400.0, 7600.0]},
            "rust": {"docs_per_sec": [35400.0, 53700.0, 80200.0, 96400.0]},
            "rust-async": {"docs_per_sec": [50400.0, 66000.0, 100300.0, 119200.0]},
            "mongod": {"docs_per_sec": [105700.0, 203300.0, 366100.0, 481000.0]},
        },
    }


def test_scaling_ratios() -> None:
    assert scaling_ratios([100.0, 200.0, 50.0]) == [1.0, 2.0, 0.5]
    with pytest.raises(ValueError):
        scaling_ratios([0.0, 100.0])


def test_render_viz_structure(results: dict) -> None:
    # Neither surface draws the deprecated Python series or the withdrawn
    # async stack (RM-731), even when the results carry them.
    for style, prefix, lines in [("website", "viz", 2), ("docs", "dv", 2)]:
        svg = render_viz(results, style)
        assert svg.count("<path ") == lines
        assert svg.count("<circle ") == 4 * lines
        assert 'stroke-dasharray="6 4"' not in svg
        assert "async" not in svg
        assert svg.count(f'class="{prefix}-val"') == lines
        # Absolute docs/s on a zero-based axis: gridlines at 0, 200k, 400k
        # and 600k for this data, no ratio scale.
        assert svg.count(f'class="{prefix}-grid"') == 4
        assert ">200k<" in svg and ">600k<" in svg and ">docs/s<" in svg
        assert f'class="{prefix}-x"' not in svg
        assert "mongod 481k" in svg and "Rust 96.4k" in svg


def test_render_viz_tooltips(results: dict) -> None:
    website = render_viz(results, "website")
    assert 'data-tip="mongod — 2 writers: 203,300 docs/s"' in website
    assert "Python" not in website
    assert "<title>" not in website
    docs = render_viz(results, "docs")
    assert "<title>Rust server — 8 writers: 96,400 docs/s</title>" in docs
    assert "data-tip" not in docs
    assert "Python" not in docs


def test_render_viz_singular_writer_phrase(results: dict) -> None:
    svg = render_viz(results, "website")
    assert "1 writer:" in svg
    assert "1 writers:" not in svg


def test_render_tables(results: dict) -> None:
    website = render_website_table(results)
    row = "<tr><th>1</th><td>105,700</td><td>35,400</td></tr>"
    assert row in website
    assert "async" not in website
    docs = render_docs_table(results)
    assert "| 8 | 96,400 | 481,000 |" in docs
    assert "async" not in docs
    assert "Python" not in docs


def test_nudge_labels_separates_collisions() -> None:
    pos = _nudge_labels([("a", 100.0), ("b", 104.0), ("c", 200.0)])
    assert pos["b"] - pos["a"] >= 14
    assert pos["c"] == 200.0


def test_replace_block_requires_single_region(tmp_path: Path) -> None:
    f = tmp_path / "t.html"
    f.write_text(f"x\n{VIZ_BEGIN}\nold\n{VIZ_END}\ny\n")
    out = replace_block(f.read_text(), VIZ_BEGIN, VIZ_END, "new", path=f)
    assert "new" in out and "old" not in out
    with pytest.raises(SystemExit):
        replace_block("no markers here", VIZ_BEGIN, VIZ_END, "new", path=f)


def test_load_results_rejects_missing_server(tmp_path: Path, results: dict) -> None:
    del results["servers"]["mongod"]
    p = tmp_path / "r.json"
    p.write_text(json.dumps(results))
    with pytest.raises(SystemExit):
        load_results(p)


def test_refresh_surface_idempotent(tmp_path: Path, results: dict) -> None:
    for src, style, table in [
        (PERFORMANCE_HTML, "website", render_website_table(results)),
        (DOCS_MD, "docs", render_docs_table(results)),
    ]:
        copy = tmp_path / src.name
        shutil.copy(src, copy)
        viz = render_viz(results, style)
        refresh_surface(copy, viz, table)
        assert refresh_surface(copy, viz, table) is False
        text = copy.read_text(encoding="utf-8")
        for marker in (VIZ_BEGIN, VIZ_END, TABLE_BEGIN, TABLE_END):
            assert text.count(marker) == 1


def test_committed_surfaces_match_committed_results(tmp_path: Path) -> None:
    """The committed chart blocks must be what the committed JSON renders to.

    If this fails, someone edited the chart or the results JSON without
    running ``invoke concurrency-refresh --skip-bench``.
    """
    results = load_results(DEFAULT_RESULTS)
    for src, style, table in [
        (PERFORMANCE_HTML, "website", render_website_table(results)),
        (DOCS_MD, "docs", render_docs_table(results)),
    ]:
        copy = tmp_path / src.name
        shutil.copy(src, copy)
        changed = refresh_surface(copy, render_viz(results, style), table)
        assert not changed, (
            f"{src} chart blocks are stale relative to {DEFAULT_RESULTS}; "
            "run `invoke concurrency-refresh --skip-bench`"
        )


def test_assemble_results_medians() -> None:
    payload = assemble_results(
        writers_list=[1, 2],
        duration=5.0,
        batch=100,
        shared_collection=False,
        runs=3,
        runs_rates={"python": [[10.0, 30.0, 20.0], [5.0, 7.0, 6.0]]},
    )
    assert payload["meta"]["writers"] == [1, 2]
    assert payload["servers"]["python"]["docs_per_sec"] == [20.0, 6.0]
    assert payload["servers"]["python"]["runs_docs_per_sec"][0] == [10.0, 30.0, 20.0]


def test_results_name_the_builds_they_measured() -> None:
    """The 2026-11-18 file recorded no version of either server; a run now
    names each build, the host and the date."""
    assert parse_rust_version(["secantusd-mdb 0.5.3-beta.199", "tree: abc123"]) == {
        "version": "0.5.3-beta.199",
        "tree": "abc123",
    }
    assert parse_rust_version([]) == {"version": "unknown"}
    assert parse_mongod_version(["db version v8.0.38", "Build Info: {"]) == {"version": "8.0.38"}
    assert parse_mongod_version(["nonsense"]) == {"version": "unknown"}
    payload = assemble_results(
        writers_list=[1],
        duration=5.0,
        batch=100,
        shared_collection=False,
        runs=1,
        runs_rates={"rust": [[10.0]]},
        versions={"rust": {"version": "0.5.3-beta.199", "tree": "abc123"}},
        host={"machine": "x86_64", "cpus": 8},
        generated="2026-11-20",
    )
    assert payload["meta"]["versions"]["rust"]["tree"] == "abc123"
    assert payload["meta"]["host"]["cpus"] == 8
    assert payload["meta"]["generated"] == "2026-11-20"
