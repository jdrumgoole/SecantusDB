"""A pgjdbc gauge run that gradle never finished must not read as a result.

`RESULTS` is wiped at startup and `_aggregate` only ran after gradle returned,
so a run that hit the wall-clock budget reported **zero tests** — which is
indistinguishable at a glance from a clean sweep, and was in fact read that way
once. The run now aggregates whatever completed, records that it was cut short,
and the report generator refuses to publish a rate measured over part of the
suite.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from pgjdbc_validation import generate_report, runner  # noqa: E402

_SUITE = """<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="org.postgresql.test.jdbc2.ArrayTest" tests="4" failures="1" errors="0" skipped="0">
 <testcase name="ok" classname="org.postgresql.test.jdbc2.ArrayTest"/>
 <testcase name="boom" classname="org.postgresql.test.jdbc2.ArrayTest">
  <failure message="x"/>
 </testcase>
</testsuite>
"""


@pytest.fixture
def results(tmp_path, monkeypatch):
    """Point the runner's XML input and JSON output at a scratch tree."""
    res = tmp_path / "test-results"
    res.mkdir()
    (res / "TEST-ArrayTest.xml").write_text(_SUITE)
    raw = tmp_path / "pgjdbc-raw.json"
    monkeypatch.setattr(runner, "RESULTS", res)
    monkeypatch.setattr(runner, "RAW_OUT", raw)
    return raw


def test_a_completed_run_is_not_flagged(results):
    runner._aggregate()
    data = json.loads(results.read_text())
    assert "truncated" not in data
    assert data["classes"][0]["tests"] == 4


def test_a_truncated_run_keeps_partial_results_and_says_so(results):
    runner._aggregate(truncated=True)
    data = json.loads(results.read_text())
    assert data["truncated"] is True
    # The partial results are kept — they are what tells you how far it got.
    assert data["classes"][0]["class"].endswith("ArrayTest")


def test_timeout_aggregates_instead_of_reporting_nothing(results, monkeypatch, capsys):
    """The regression: the TimeoutExpired escaped, `_aggregate` never ran, and
    the summary showed zero tests from a wiped results directory."""

    def _boom(*_a, **_kw):
        raise subprocess.TimeoutExpired(cmd="gradlew", timeout=runner.GRADLE_TIMEOUT_SECONDS)

    monkeypatch.setattr(runner.subprocess, "run", _boom)
    monkeypatch.setattr(runner, "_verify_secantus_identity", lambda *a, **k: None)
    monkeypatch.setattr(runner, "_wait_for_listener", lambda *a, **k: None)
    monkeypatch.setattr(runner, "_find_jdk21", lambda: "/fake/jdk")
    monkeypatch.setattr(runner, "_test_classes", lambda: ["Some.Test"])
    monkeypatch.setattr(runner.shutil, "rmtree", lambda *a, **k: None)
    monkeypatch.setattr(runner.subprocess, "Popen", lambda *a, **k: _FakeDaemon())
    # A worktree need not have the pgjdbc submodule checked out; satisfy the
    # pre-flight so the test exercises the timeout path, not the bail.
    vendor = results.parent / "vendor"
    vendor.mkdir()
    (vendor / "gradlew").write_text("#!/bin/sh\n")
    monkeypatch.setattr(runner, "VENDOR", vendor)

    rc = runner.main()

    assert rc == 124, "a truncated run must not report success"
    assert json.loads(results.read_text())["truncated"] is True
    assert "TRUNCATED" in capsys.readouterr().err


class _FakeDaemon:
    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


def test_report_refuses_a_truncated_run(tmp_path, monkeypatch):
    raw = tmp_path / "raw.json"
    raw.write_text(json.dumps({"truncated": True, "classes": []}))
    out = tmp_path / "report.md"
    monkeypatch.setattr(sys, "argv", ["generate_report", str(raw), str(out)])

    with pytest.raises(SystemExit) as exc:
        generate_report.main()

    assert "truncated" in str(exc.value)
    assert not out.exists(), "a partial denominator must never be published"


def test_report_renders_a_complete_run(tmp_path, monkeypatch):
    raw = tmp_path / "raw.json"
    raw.write_text(
        json.dumps(
            {
                "classes": [
                    {
                        "class": "org.postgresql.test.jdbc2.ArrayTest",
                        "tests": 4,
                        "failures": 1,
                        "skipped": 0,
                        "failed_tests": ["boom"],
                    }
                ]
            }
        )
    )
    out = tmp_path / "report.md"
    monkeypatch.setattr(sys, "argv", ["generate_report", str(raw), str(out)])

    generate_report.main()

    assert "ArrayTest" in out.read_text()


def test_timeout_budget_is_overridable(monkeypatch):
    """CI hardware is several times slower than a dev machine; the budget has to
    be raisable without editing the runner."""
    monkeypatch.setenv("SECANTUS_PGJDBC_TIMEOUT", "123")
    import importlib

    reloaded = importlib.reload(runner)
    try:
        assert reloaded.GRADLE_TIMEOUT_SECONDS == 123.0
    finally:
        monkeypatch.delenv("SECANTUS_PGJDBC_TIMEOUT", raising=False)
        importlib.reload(runner)


# -- sharding: split the class list, merge only a COMPLETE shard set -------- #


def _shard_raw(tmp_path, k, n, cls="A", truncated=False):
    payload = {
        "classes": [
            {
                "class": f"org.postgresql.test.jdbc2.{cls}",
                "tests": 2,
                "failures": 1,
                "skipped": 0,
                "failed_tests": ["x"],
            }
        ],
        "shard": {"index": k, "of": n},
    }
    if truncated:
        payload["truncated"] = True
    p = tmp_path / f"pgjdbc-raw-shard-{k}.json"
    p.write_text(json.dumps(payload))
    return p


def test_shard_spec_parses_and_rejects(monkeypatch):
    monkeypatch.setenv("SECANTUS_PGJDBC_SHARD", "2/4")
    assert runner._shard_spec() == (2, 4)
    monkeypatch.setenv("SECANTUS_PGJDBC_SHARD", "")
    assert runner._shard_spec() is None
    for bad in ("0/4", "5/4", "x/4", "4"):
        monkeypatch.setenv("SECANTUS_PGJDBC_SHARD", bad)
        with pytest.raises(SystemExit):
            runner._shard_spec()


def test_weighted_shards_partition_exactly_and_split_the_whales(monkeypatch):
    classes = [f"org.postgresql.test.jdbc2.C{i}" for i in range(20)] + [
        "org.postgresql.test.jdbc2.CopyLargeFileTest",
        "org.postgresql.test.jdbc2.AutoRollbackTest",
        "org.postgresql.test.jdbc2.BatchFailureTest",
    ]
    parts = runner._shard_classes(classes, 4)
    assert sorted(sum(parts, [])) == sorted(classes)  # exact partition
    # The heavy classes must land in distinct shards — the whole point of the
    # weighted assignment (a plain round-robin once put all three in one
    # shard, a 44-minute straggler beside three ~15-minute siblings).
    homes = {
        whale: next(i for i, s in enumerate(parts) if any(c.endswith(whale) for c in s))
        for whale in ("CopyLargeFileTest", "AutoRollbackTest", "BatchFailureTest")
    }
    assert len(set(homes.values())) == 3, homes
    assert runner._shard_classes(classes, 4) == parts  # deterministic


def test_merge_accepts_a_complete_shard_set(tmp_path):
    paths = [str(_shard_raw(tmp_path, k, 3, cls=f"T{k}")) for k in (2, 1, 3)]
    merged = generate_report._merge_raw(paths)
    assert sorted(c["class"] for c in merged["classes"]) == [
        "org.postgresql.test.jdbc2.T1",
        "org.postgresql.test.jdbc2.T2",
        "org.postgresql.test.jdbc2.T3",
    ]


def test_merge_refuses_a_missing_shard(tmp_path):
    paths = [str(_shard_raw(tmp_path, k, 3)) for k in (1, 3)]
    with pytest.raises(SystemExit, match="incomplete shard set"):
        generate_report._merge_raw(paths)


def test_merge_refuses_a_truncated_shard(tmp_path):
    paths = [str(_shard_raw(tmp_path, 1, 2)), str(_shard_raw(tmp_path, 2, 2, truncated=True))]
    with pytest.raises(SystemExit, match="truncated"):
        generate_report._merge_raw(paths)


def test_merge_refuses_mixed_sharded_and_unsharded(tmp_path):
    unsharded = tmp_path / "pgjdbc-raw.json"
    unsharded.write_text(json.dumps({"classes": []}))
    paths = [str(_shard_raw(tmp_path, 1, 2)), str(unsharded)]
    with pytest.raises(SystemExit, match="mix of sharded and unsharded"):
        generate_report._merge_raw(paths)


def test_merge_passes_a_single_unsharded_raw_through(tmp_path):
    unsharded = tmp_path / "pgjdbc-raw.json"
    unsharded.write_text(
        json.dumps(
            {
                "classes": [
                    {"class": "c", "tests": 1, "failures": 0, "skipped": 0, "failed_tests": []}
                ]
            }
        )
    )
    merged = generate_report._merge_raw([str(unsharded)])
    assert len(merged["classes"]) == 1
