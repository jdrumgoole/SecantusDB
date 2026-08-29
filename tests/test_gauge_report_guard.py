"""The gauge report guard: never render a report from a run that didn't happen.

`tasks._run_gauge` is what stops a gauge that bailed (missing toolchain, port
already taken, failed build) from falling through to `generate_report`, which
would re-render the PREVIOUS run's artifact under today's date — a stale
conformance number wearing a fresh timestamp. Observed for real on the C++
gauge, whose tests hard-wire port 27017 and which refuses to start when
something else holds it.

The guard is keyed on the artifact rather than the runner's exit code, because
a gauge whose tests FAIL must still produce a report.
"""

from __future__ import annotations

import sys
import types
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tasks  # noqa: E402


class _FakeContext:
    """Stands in for invoke's Context; records commands and fakes the runner."""

    def __init__(self, on_run=None) -> None:
        self.commands: list[str] = []
        self._on_run = on_run

    def run(self, cmd: str, **_kw: object) -> types.SimpleNamespace:
        self.commands.append(cmd)
        if self._on_run is not None:
            self._on_run()
        return types.SimpleNamespace(exited=0)


def test_report_is_refused_when_the_runner_produced_nothing(tmp_path: Path) -> None:
    """The bug this exists for: a runner that bails writes no artifact, so the
    report must NOT be regenerated — even though the runner is invoked with
    warn=True and its exit code is therefore ignored."""
    raw = tmp_path / "gauge-raw.json"
    c = _FakeContext()  # runs the command but writes no artifact, like a bail

    with pytest.raises(SystemExit) as exc:
        tasks._run_gauge(
            c,
            module="cxx_validation.runner",
            raw=str(raw),
            report="docs/validation-report-cxx.md",
            server="python",
            hint="Port 27017 already being in use is the usual cause.",
        )

    msg = str(exc.value)
    assert "produced no results" in msg
    assert "docs/validation-report-cxx.md" in msg
    assert "Port 27017" in msg, "the gauge-specific hint must reach the reader"


def test_a_stale_artifact_cannot_masquerade_as_this_run(tmp_path: Path) -> None:
    """A leftover artifact from a previous run is cleared BEFORE the runner, so
    it can never be mistaken for fresh output. This is the exact shape of the
    real bug: several runners clear their artifact only after their pre-flight
    checks, and the pre-flight bail is what leaves the stale file."""
    raw = tmp_path / "gauge-raw.json"
    raw.write_text('{"from": "a previous run"}')

    with pytest.raises(SystemExit):
        tasks._run_gauge(
            c := _FakeContext(),
            module="c_validation.runner",
            raw=str(raw),
            report="docs/validation-report-c.md",
        )
    assert c.commands, "the runner should still have been invoked"
    assert not raw.exists(), "the previous run's artifact must be gone, not reused"


def test_report_proceeds_when_the_runner_produced_results(tmp_path: Path) -> None:
    """A gauge that ran gets its report — including one whose tests failed, which
    is why the guard looks at the artifact and not the exit code."""
    raw = tmp_path / "gauge-raw.xml"
    c = _FakeContext(on_run=lambda: raw.write_text("<testsuite failures='3'/>"))

    tasks._run_gauge(
        c,
        module="php_ext_validation.runner",
        raw=str(raw),
        report="docs/validation-report-php-ext.md",
        server="python",
    )
    assert raw.is_file()


def test_directory_artifacts_are_handled(tmp_path: Path) -> None:
    """java / kotlin emit a DIRECTORY of JUnit XML rather than a single file."""
    results = tmp_path / "java-results"

    # Empty directory == no results.
    results.mkdir()
    with pytest.raises(SystemExit):
        tasks._run_gauge(
            _FakeContext(),
            module="java_validation.runner",
            raw=str(results),
            report="docs/validation-report-java.md",
            server="python",
        )

    # A directory with XML in it == a real run.
    def _emit() -> None:
        results.mkdir(exist_ok=True)
        (results / "TEST-Foo.xml").write_text("<testsuite/>")

    tasks._run_gauge(
        _FakeContext(on_run=_emit),
        module="java_validation.runner",
        raw=str(results),
        report="docs/validation-report-java.md",
        server="python",
    )


def test_empty_artifact_counts_as_no_results(tmp_path: Path) -> None:
    """A zero-byte file is what a killed test process leaves behind."""
    raw = tmp_path / "gauge-raw.json"
    with pytest.raises(SystemExit):
        tasks._run_gauge(
            _FakeContext(on_run=lambda: raw.write_text("")),
            module="go_validation.runner",
            raw=str(raw),
            report="docs/validation-report-go.md",
            server="python",
        )


def test_the_server_selector_is_only_set_when_a_server_is_given(tmp_path: Path) -> None:
    """The SQL gauges (psycopg, slt) have no python/rust split."""
    raw = tmp_path / "raw.json"

    c = _FakeContext(on_run=lambda: raw.write_text("{}"))
    tasks._run_gauge(c, module="slt_validation.runner", raw=str(raw), report="r.md")
    assert "SECANTUS_GAUGE_SERVER" not in c.commands[0]

    c = _FakeContext(on_run=lambda: raw.write_text("{}"))
    tasks._run_gauge(c, module="go_validation.runner", raw=str(raw), report="r.md", server="rust")
    assert "SECANTUS_GAUGE_SERVER=rust" in c.commands[0]


def test_every_gauge_guards_the_artifact_its_report_is_built_from() -> None:
    """The guard and the report generator must name the SAME artifact.

    If they drift, the failure is silent and backwards: a gauge that ran fine
    gets refused (guard watches a path nothing writes), or — worse — one that
    bailed still reports. Also asserts no gauge task slips back to calling
    generate_report straight after an unguarded runner, which is the shape the
    original bug had.
    """
    import re

    source = (Path(__file__).resolve().parents[1] / "tasks.py").read_text()
    tasks_seen = 0
    for m in re.finditer(r'@task\(name="(validate-[a-z-]+)"\)(.*?)(?=\n@task\(|\Z)', source, re.S):
        name, body = m.group(1), m.group(2)
        if "generate_report" not in body:
            continue
        if "_validation.runner" not in body and "pymongo_validation.plugin" in body:
            continue  # the pymongo gauges run pytest inline, not a runner module
        if name.endswith("-report"):
            # A merge-only task renders from shard artifacts, not a runner.
            # Its freshness guard is consume-on-merge: the shard raws are
            # deleted after a successful render, so a re-run without fresh
            # artifacts fails on the missing files instead of re-rendering
            # yesterday's results — assert THAT guard is present instead.
            assert ".unlink()" in body, (
                f"{name} merges shard artifacts but never consumes them — "
                "a re-run would re-render stale results under today's date"
            )
            continue
        tasks_seen += 1
        guard = re.search(r"_run_gauge\((.*?)\n    \)", body, re.S)
        assert guard, f"{name} calls generate_report without going through _run_gauge"
        raw = re.search(r'raw=f?"([^"]+)"', guard.group(1))
        assert raw, f"{name}: _run_gauge has no raw= artifact"
        assert raw.group(1) in body.split("generate_report", 1)[1], (
            f"{name}: the guarded artifact {raw.group(1)!r} is not the one "
            "generate_report reads — they have drifted apart"
        )
    assert tasks_seen >= 13, f"expected every driver gauge to be checked, saw {tasks_seen}"


def test_pgtest_run_pattern_is_anchored_per_file() -> None:
    """The pgtest gauge runs one corpus file per daemon, so its ``-run``
    pattern must be anchored: an unanchored ``TestPGTest/copy`` also selects
    ``copy_file_upload`` and would run it against an already-dirtied server.
    """
    import re

    from pgtest_validation.runner import _run_pattern

    pattern = _run_pattern("copy")
    assert pattern == "TestPGTest/^copy$"
    # Emulate go's per-element regexp matching: the second element must match
    # "copy" and NOT "copy_file_upload".
    elem = pattern.split("/", 1)[1]
    assert re.search(elem, "copy")
    assert not re.search(elem, "copy_file_upload")
