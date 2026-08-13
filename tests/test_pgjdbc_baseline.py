"""Baseline-aware verdict for the pgjdbc gauge lane (`pgjdbc_validation.baseline`).

The lane fails only on regression vs the committed baseline: a new failing
(class, test) or a failure COUNT above baseline (parameterized classes repeat
bare names, so ids alone can't see a partial regression). Sharded runs judge
only the classes present in the run.
"""

from __future__ import annotations

import json
from collections import Counter

from pgjdbc_validation import baseline as bl


def raw(classes):
    return {"classes": classes}


def entry(cls, failed=(), tests=10):
    return {
        "class": cls,
        "tests": tests,
        "failures": len(failed),
        "skipped": 0,
        "seconds": 1.0,
        "failed_tests": list(failed),
    }


BASE = Counter(
    {
        "a.B::t1()": 1,
        "a.B::run()": 3,
        "a.C::t2()": 1,
    }
)


def test_clean_run_no_regression():
    r = raw([entry("a.B"), entry("a.C")])
    regressions, improvements = bl.compare(r, BASE)
    assert regressions == {}
    # Everything in baseline for present classes improved to zero.
    assert improvements == {
        "a.B::t1()": (0, 1),
        "a.B::run()": (0, 3),
        "a.C::t2()": (0, 1),
    }


def test_standing_failures_stay_green():
    r = raw([entry("a.B", ["t1()", "run()", "run()", "run()"]), entry("a.C", ["t2()"])])
    regressions, improvements = bl.compare(r, BASE)
    assert regressions == {}
    assert improvements == {}


def test_new_failure_is_regression():
    r = raw([entry("a.B", ["t1()", "tNEW()"])])
    regressions, _ = bl.compare(r, BASE)
    assert regressions == {"a.B::tNEW()": (1, 0)}


def test_count_increase_is_regression():
    r = raw([entry("a.B", ["run()"] * 5)])
    regressions, _ = bl.compare(r, BASE)
    assert regressions == {"a.B::run()": (5, 3)}


def test_absent_class_says_nothing():
    # A shard without a.C neither regresses nor improves a.C's baseline.
    r = raw([entry("a.B", ["t1()", "run()", "run()", "run()"])])
    regressions, improvements = bl.compare(r, BASE)
    assert regressions == {}
    assert improvements == {}


def test_verdict_exit_codes(tmp_path, monkeypatch):
    base_file = tmp_path / "baseline.json"
    base_file.write_text(json.dumps({"failures": dict(BASE)}))
    monkeypatch.setattr(bl, "BASELINE_PATH", base_file)

    ok = tmp_path / "ok.json"
    ok.write_text(json.dumps(raw([entry("a.B", ["t1()"])])))
    assert bl.verdict(ok) == 0

    bad = tmp_path / "bad.json"
    bad.write_text(json.dumps(raw([entry("a.B", ["boom()"])])))
    assert bl.verdict(bad) == 1

    trunc = tmp_path / "trunc.json"
    trunc.write_text(json.dumps({**raw([]), "truncated": True}))
    assert bl.verdict(trunc) == 124


def test_verdict_missing_baseline_fails_loudly(tmp_path, monkeypatch):
    monkeypatch.setattr(bl, "BASELINE_PATH", tmp_path / "absent.json")
    r = tmp_path / "r.json"
    r.write_text(json.dumps(raw([])))
    assert bl.verdict(r) == 2


def test_update_roundtrip(tmp_path):
    p1 = tmp_path / "s1.json"
    p1.write_text(json.dumps(raw([entry("a.B", ["run()", "run()"])])))
    p2 = tmp_path / "s2.json"
    p2.write_text(json.dumps(raw([entry("a.C", ["t2()"])])))
    out = tmp_path / "baseline.json"
    merged = bl.update_baseline([p1, p2], out=out)
    assert merged == Counter({"a.B::run()": 2, "a.C::t2()": 1})
    data = json.loads(out.read_text())
    assert data["failures"] == {"a.B::run()": 2, "a.C::t2()": 1}
    assert data["total"] == 3


def test_committed_baseline_loads():
    base = bl.load_baseline()
    assert sum(base.values()) > 0  # the committed file is real and parseable
