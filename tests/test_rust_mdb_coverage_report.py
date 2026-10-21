"""The report half of ``scripts/rust_mdb_coverage.py``: what it counts and what it says.

The measurement takes over an hour and needs every gauge toolchain, so it is not
run here; these tests feed the report functions ``llvm-cov export`` JSON shaped
like the real thing.
"""

from __future__ import annotations

import importlib.util
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "rust_mdb_coverage.py"


def _load():
    spec = importlib.util.spec_from_file_location("rust_mdb_coverage", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cov = _load()


def _file(
    root: pathlib.Path, rel: str, lines: tuple[int, int], fns: tuple[int, int] = (1, 1)
) -> dict:
    def s(n, c):
        return {"count": n, "covered": c, "percent": 100.0 * c / n if n else 0}

    return {
        "filename": str(root / rel),
        "summary": {"lines": s(*lines), "regions": s(*lines), "functions": s(*fns)},
    }


def _export(root: pathlib.Path, files: list[dict]) -> dict:
    return {"data": [{"files": files, "totals": {}}], "type": "llvm.coverage.json.export"}


def test_keeps_only_the_server_crates_sources(tmp_path):
    files = [
        _file(tmp_path, "crates/secantus-core/src/group.rs", (100, 80)),
        _file(tmp_path, "crates/secantus-core/tests/parity.rs", (50, 50)),  # a test, not source
        _file(tmp_path, "crates/secantus-pg/src/lib.rs", (70, 10)),  # the other server
        _file(tmp_path, "crates/secantus-core-py/src/lib.rs", (30, 0)),  # the PyO3 binding
        _file(pathlib.Path("/home/u/.cargo/registry"), "bson/src/lib.rs", (9, 9)),  # a dependency
    ]
    got = cov.file_summaries(_export(tmp_path, files), tmp_path)
    assert list(got) == ["secantus-core/src/group.rs"]
    assert got["secantus-core/src/group.rs"]["lines"] == (100, 80)


def test_totals_by_crate(tmp_path):
    files = cov.file_summaries(
        _export(
            tmp_path,
            [
                _file(tmp_path, "crates/secantus-core/src/a.rs", (100, 50)),
                _file(tmp_path, "crates/secantus-core/src/b.rs", (100, 100)),
                _file(tmp_path, "crates/secantus-wire/src/lib.rs", (10, 1)),
            ],
        ),
        tmp_path,
    )
    assert cov.totals(files)["lines"] == (210, 151)
    assert cov.totals(files, "secantus-core")["lines"] == (200, 150)
    assert cov.totals(files, "secantus-auth")["lines"] == (0, 0)


def _sets(tmp_path):
    def fs(spec):
        return cov.file_summaries(
            _export(tmp_path, [_file(tmp_path, f"crates/{p}", ln) for p, ln in spec.items()]),
            tmp_path,
        )

    return {
        "suites": fs(
            {
                "secantus-storage/src/changestreams.rs": (100, 70),
                "secantus-core/src/expressions.rs": (400, 260),
            }
        ),
        "gauges": fs(
            {
                "secantus-storage/src/changestreams.rs": (100, 60),
                "secantus-core/src/expressions.rs": (400, 100),
            }
        ),
        "all": fs(
            {
                "secantus-storage/src/changestreams.rs": (100, 90),
                "secantus-core/src/expressions.rs": (400, 260),
            }
        ),
    }


META = {
    "commit": "abc1234",
    "date": "2026-10-21",
    "rustc": "rustc 1.98.0",
    "mongod": "db version v8.2.11",
    "platform": "Darwin arm64",
}


def test_report_says_what_the_gauges_add(tmp_path):
    md = cov.render(
        _sets(tmp_path), META, [("gauge go", 0, "go_validation: 599 passed, 11 failed")]
    )
    assert "`abc1234`" in md
    # Totals: 330/500 suites, 160/500 gauges, 350/500 both.
    assert "| Test suites | 66.0% |" in md
    assert "| Driver gauges | 32.0% |" in md
    assert "| **Both** | 70.0% |" in md
    # Only the file the gauges reach beyond the suites is listed as added.
    added = md.split("## Where the gauges reach")[1].split("##")[0]
    assert "| `secantus-storage/src/changestreams.rs` | +20 | 70.0% | 90.0% |" in added
    assert "expressions.rs" not in added
    assert "| secantus-storage | 100 | 70.0% | 60.0% | 90.0% | +20 |" in md
    # The run table keeps the summary line (a pipe in it would break the table).
    assert "| gauge go | 0 | go_validation: 599 passed, 11 failed |" in md


def test_report_without_gauges_has_no_gauge_columns(tmp_path):
    sets = _sets(tmp_path)
    sets["gauges"] = {}
    md = cov.render(sets, META, [("tests/mongod_live", 0, "75 passed | in 11s")])
    assert "Driver gauges" not in md
    assert "Where the gauges reach" not in md
    assert "| secantus-core | 400 | 65.0% |" in md
    assert "75 passed / in 11s" in md


def test_most_missed_lines_ranks_by_lines_not_percent(tmp_path):
    md = cov.render(_sets(tmp_path), META, [])
    missed = md.split("## Most lines not executed")[1].split("##")[0]
    rows = [r for r in missed.splitlines() if r.startswith("| `")]
    assert rows[0].startswith("| `secantus-core/src/expressions.rs` | 140 |")


def test_cargo_summary_sums_every_test_binary(tmp_path):
    log = tmp_path / "t.log"
    log.write_text(
        "running 3 tests\n"
        "test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n"
        "test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n"
        "   Doc-tests x\n"
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
    )
    assert cov.cargo_summary(log) == "3 test binaries: 8 passed, 2 failed, 1 ignored"


def test_junit_summary_reads_only_this_runs_xml(tmp_path):
    import os
    import time

    old = tmp_path / "old-raw.xml"
    old.write_text('<testsuites tests="9" failures="9">')
    os.utime(old, (1, 1))
    assert cov.junit_summary(tmp_path, time.time() - 60) is None
    new = tmp_path / "cxx-raw-rust-server.xml"
    new.write_text(
        '<?xml version="1.0"?>\n'
        '<testsuites name="t" tests="143" failures="2" errors="0" skipped="5">'
    )
    assert cov.junit_summary(tmp_path, time.time() - 60) == (
        "cxx-raw-rust-server.xml: 143 tests, 2 failures, 0 errors, 5 skipped"
    )
