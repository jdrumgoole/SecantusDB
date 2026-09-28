"""The published driver grid must not mix measurements taken weeks apart.

The grid goes on secantusdb.com as ONE snapshot with one implied date. Until
2026-09-28 each collector only ever looked at its own artifact and nothing
compared them, so a render could publish a **10 August** pymongo-async rate
beside twelve numbers measured that morning with no staleness marker anywhere.
That was not hypothetical — it is what the first Rust-server render actually
produced.

Spread, not absolute age, is the test: a deliberately old but CONSISTENT sweep
is honest, and it is the mixing that misleads.
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from validation_summary import driver_panels, generate  # noqa: E402


def _touch(path: Path, days_old: float) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text("{}")
    when = time.time() - days_old * 86400
    os.utime(path, (when, when))


def _lay_out(tmp: Path, suffix: str, ages: dict[str, float]) -> None:
    """Create every gauge artifact, `ages` overriding the default of 'today'."""
    for name, base in generate.GAUGE_ARTIFACTS.items():
        _touch(generate._artifact(tmp, base, suffix), ages.get(name, 0.0))


def test_the_map_covers_every_panel():
    """Pins the map against the collector registry.

    The freshness check reaches the artifacts through `GAUGE_ARTIFACTS` while
    the collectors reach them through their own literals. If a gauge is added
    to one and not the other, the new panel is silently exempt from the check —
    which is precisely the failure this whole guard exists to prevent.
    """
    assert set(generate.GAUGE_ARTIFACTS) == set(driver_panels._COLLECTORS)


@pytest.mark.parametrize("suffix", ["", "-rust-server"])
def test_a_consistent_sweep_passes(tmp_path, suffix):
    _lay_out(tmp_path, suffix, {})
    driver_panels._refuse_mixed_age(tmp_path, suffix, "python", 7.0)


@pytest.mark.parametrize("suffix", ["", "-rust-server"])
def test_a_uniformly_old_sweep_passes(tmp_path, suffix):
    """Old but consistent is honest; only the mixing is the problem."""
    _lay_out(tmp_path, suffix, dict.fromkeys(generate.GAUGE_ARTIFACTS, 90.0))
    driver_panels._refuse_mixed_age(tmp_path, suffix, "python", 7.0)


def test_one_stale_artifact_is_refused_and_named(tmp_path):
    """The real 2026-09-28 shape: twelve fresh, one from seven weeks earlier."""
    _lay_out(tmp_path, "-rust-server", {"pymongo (async)": 49.0})
    with pytest.raises(SystemExit) as exc:
        driver_panels._refuse_mixed_age(tmp_path, "-rust-server", "rust", 7.0)
    msg = str(exc.value)
    assert "pymongo (async)" in msg, "the offender must be named, not just counted"
    assert "49 days older" in msg
    assert "--server rust" in msg, "the message must say how to fix it"


def test_within_the_threshold_is_allowed(tmp_path):
    """A sweep that took a couple of days to finish is not a staleness bug."""
    _lay_out(tmp_path, "", {"mongo-c-driver": 3.0, "mongo-cxx-driver": 5.0})
    driver_panels._refuse_mixed_age(tmp_path, "", "python", 7.0)


def test_missing_artifacts_are_not_treated_as_stale(tmp_path):
    """An absent file is the collectors' error to report, with a better message.

    It is also the normal state mid-run, because the gauges now delete their raw
    before starting.
    """
    _lay_out(tmp_path, "", {})
    generate._artifact(tmp_path, generate.GAUGE_ARTIFACTS["pymongo"], "").unlink()
    driver_panels._refuse_mixed_age(tmp_path, "", "python", 7.0)


def test_render_rejects_an_unknown_server(tmp_path):
    with pytest.raises(SystemExit) as exc:
        driver_panels.render(tmp_path, "postgres")
    assert "'python' or 'rust'" in str(exc.value)


def test_an_empty_file_artifact_is_refused(tmp_path):
    """Fresh mtime, no bytes — the age check cannot see this."""
    _lay_out(tmp_path, "", {})
    generate._artifact(tmp_path, generate.GAUGE_ARTIFACTS["pymongo"], "").write_text("")
    with pytest.raises(SystemExit) as exc:
        driver_panels._refuse_mixed_age(tmp_path, "", "python", 7.0)
    assert "contain no results" in str(exc.value)
    assert "pymongo" in str(exc.value)


def test_an_empty_directory_artifact_is_refused(tmp_path):
    """The real 2026-09-28 case: a Java re-run cleared
    `java-results-rust-server/` and never refilled it, while the report on disk
    still described the data that had gone. The directory's mtime was the
    NEWEST of the set, so age alone rendered a blank panel beside twelve real
    ones."""
    _lay_out(tmp_path, "-rust-server", {})
    d = generate._artifact(tmp_path, generate.GAUGE_ARTIFACTS["mongo-java-driver"], "-rust-server")
    if d.is_file():
        d.unlink()
    d.mkdir(parents=True, exist_ok=True)
    with pytest.raises(SystemExit) as exc:
        driver_panels._refuse_mixed_age(tmp_path, "-rust-server", "rust", 7.0)
    assert "mongo-java-driver" in str(exc.value)


def test_a_directory_with_results_is_accepted(tmp_path):
    _lay_out(tmp_path, "-rust-server", {})
    d = generate._artifact(tmp_path, generate.GAUGE_ARTIFACTS["mongo-java-driver"], "-rust-server")
    if d.is_file():
        d.unlink()
    d.mkdir(parents=True, exist_ok=True)
    (d / "TEST-x.xml").write_text("<testsuite/>")
    driver_panels._refuse_mixed_age(tmp_path, "-rust-server", "rust", 7.0)


def test_a_truncated_run_shows_no_rate():
    """`passed / ran` over the part that finished looks BETTER the more tests
    went missing — which is how the Go panel read 100.0% while its run was cut
    short at 476 of 481 by a DNS hang."""
    kw = dict(
        name="mongo-go-driver",
        language="Go",
        driver_version="abc",
        failed=0,
        skipped=37,
        failure_descriptions=[],
    )
    whole = generate.GaugeStats(passed=439, truncated=False, **kw)
    cut = generate.GaugeStats(passed=439, truncated=True, **kw)
    assert driver_panels._format_rate(whole) == "100.0%"
    assert driver_panels._format_rate(cut) == "&mdash;"
