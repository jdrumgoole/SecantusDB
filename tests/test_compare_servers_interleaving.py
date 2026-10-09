"""The per-operation benchmark measures its servers interleaved.

Each published figure is a ratio to mongod. Measuring all of one server's reps
and then all of the next server's put minutes between numerator and
denominator, so a change in the machine mid-run moved one column only: three
runs of one build put multi-stage aggregation at 2.8x, 3.6x and 4.1x.
"""

from __future__ import annotations

import contextlib

import pytest
from bench import compare_servers as cs


def _makers(order: list[str], names: tuple[str, ...]):
    def maker(name: str):
        @contextlib.contextmanager
        def open_client():
            order.append(name)
            yield name

        return open_client

    return {name: maker(name) for name in names}


@pytest.fixture
def fake_workloads(monkeypatch):
    """Each server "takes" a fixed time, scaled by the pass it ran in."""
    cost = {"mongod": 1.0, "rust": 2.0, "python": 10.0}
    passes: dict[str, int] = {}

    def run(client, n):
        passes[client] = passes.get(client, 0) + 1
        return {"insert": cost[client] * passes[client]}

    monkeypatch.setattr(cs, "_run_workloads", run)
    monkeypatch.setattr(cs, "_capture_mongod_version", lambda client: None)


def test_every_pass_runs_every_server_before_the_next_pass(fake_workloads) -> None:
    order: list[str] = []
    cs._interleaved_runs(_makers(order, ("mongod", "rust", "python")), n=1, reps=4)

    passes = [order[i : i + 3] for i in range(0, len(order), 3)]
    assert len(passes) == 4
    assert all(sorted(p) == ["mongod", "python", "rust"] for p in passes)
    # The order rotates, so no server is always first after a cold start.
    assert [p[0] for p in passes] == ["mongod", "rust", "python", "mongod"]


def test_samples_stay_paired_by_pass(fake_workloads) -> None:
    samples = cs._interleaved_runs(_makers([], ("mongod", "rust")), n=1, reps=3)

    assert samples["mongod"]["insert"] == [1.0, 2.0, 3.0]
    assert samples["rust"]["insert"] == [2.0, 4.0, 6.0]
    # The machine tripled in cost across the run and the ratio did not move.
    assert cs.paired_ratio_range(samples["rust"]["insert"], samples["mongod"]["insert"]) == (
        2.0,
        2.0,
    )


def test_paired_ratio_range_skips_unusable_passes() -> None:
    nan = float("nan")
    assert cs.paired_ratio_range([2.0, 9.0, 3.0], [1.0, nan, 1.0]) == (2.0, 3.0)
    assert cs.paired_ratio_range([nan], [1.0]) is None
    assert cs.paired_ratio_range([], []) is None


def test_results_file_records_the_per_pass_spread(tmp_path, fake_workloads) -> None:
    import argparse
    import json

    samples = {
        name: {k: [v, v * 2] for k in cs.WORKLOADS}
        for name, v in (("mongod", 0.010), ("rust", 0.020), ("python", 0.100))
    }
    samples["mongod"].pop("change_stream_drain")
    med = {
        name: cs._medians({k: s.get(k, [1.0]) for k in cs.WORKLOADS}) for name, s in samples.items()
    }
    out = tmp_path / "latency.json"
    cs._write_json(
        out, med["mongod"], med["rust"], med["python"], argparse.Namespace(n=1, reps=2), samples
    )

    data = json.loads(out.read_text())
    assert data["order"] == "interleaved"
    rows = {w["key"]: w for w in data["workloads"]}
    assert rows["insert"]["rust_x_range"] == [2.0, 2.0]
    assert rows["insert"]["py_x_range"] == [10.0, 10.0]
    # mongod's change-stream figure comes from a separate run: nothing to pair.
    assert "rust_x_range" not in rows["change_stream_drain"]
