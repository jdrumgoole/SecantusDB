"""The published head-to-head table must match the newest benchmark run.

`docs/benchmark.md`'s "Over a real network" section was the last benchmark
surface updated by hand, and it went stale twice — most recently across two
releases, showing `0.5.3-beta.161` at 11,099 ops/s under a header that named a
different mongod. Figures quoted in two release summaries were never actually
published there.

Nothing failed when that happened, which is the whole problem: prose with
numbers in it rots silently. This test is the tripwire — it regenerates the
marked block from `bench/results/do/<newest>/comparison.md` and fails if the
committed page differs.
"""

from __future__ import annotations

import pytest
from bench import head_to_head_chart as hth


def _has_runs() -> bool:
    return hth.RUNS_DIR.is_dir() and any(
        (p / "comparison.md").is_file() for p in hth.RUNS_DIR.glob("*/")
    )


@pytest.mark.skipif(
    not _has_runs(),
    reason="no bench/results/do/<run>/comparison.md checked out (benchmark artifacts are optional)",
)
def test_published_head_to_head_matches_newest_run() -> None:
    """Fails when someone re-runs the benchmark and forgets to refresh the page."""
    exit_code = hth.main(["--check"])
    assert exit_code == 0, (
        "docs/benchmark.md's head-to-head table does not match the newest run in "
        "bench/results/do/. Refresh it with:\n"
        "    uv run python -m bench.head_to_head_chart"
    )


@pytest.mark.skipif(not _has_runs(), reason="no benchmark artifacts checked out")
def test_generator_is_idempotent(tmp_path, monkeypatch) -> None:
    """Running the generator twice must not keep changing the file.

    A renderer that reformats its own output would make the freshness check
    above fail forever after the first run.

    Operates on a COPY: a test that rewrites the real ``docs/benchmark.md``
    would mutate a tracked file mid-run and make the check above depend on
    test ordering.
    """
    scratch = tmp_path / "benchmark.md"
    scratch.write_text(hth.DOCS.read_text(encoding="utf-8"), encoding="utf-8")
    monkeypatch.setattr(hth, "DOCS", scratch)

    assert hth.main([]) == 0
    once = scratch.read_text(encoding="utf-8")
    assert hth.main([]) == 0
    assert scratch.read_text(encoding="utf-8") == once, "generator is not idempotent"
