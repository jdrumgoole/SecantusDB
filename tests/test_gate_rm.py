"""scripts/gate_rm.py runs the storage-engine job's named files, read from test.yml.

If a step is renamed or its list moves, the gate must fail loudly rather than
run fewer files than CI does.
"""

from __future__ import annotations

import importlib.util
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


def _gate_rm():
    spec = importlib.util.spec_from_file_location("gate_rm", ROOT / "scripts/gate_rm.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_reads_every_named_storage_engine_file(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(ROOT)
    files = _gate_rm().storage_engine_files(ROOT / ".github/workflows/test.yml")
    assert "tests/test_rust_server_smoke.py" in files
    assert "tests/test_rust_binary_crash_durability.py" in files
    assert len(files) == len(set(files))
    assert all((ROOT / f).is_file() for f in files)


def test_a_renamed_step_fails_loudly(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(ROOT)
    wf = tmp_path / "test.yml"
    wf.write_text("jobs:\n  storage-engine:\n    steps:\n      - name: Something else\n")
    with pytest.raises(SystemExit, match="not found"):
        _gate_rm().storage_engine_files(wf)


def test_skips_what_ci_deliberately_does_not_run() -> None:
    excluded = _gate_rm().not_in_ci(ROOT)
    assert "test_rust_sortkey_parity.py" in excluded
    assert all(why.strip() for why in excluded.values())


def test_rebuilds_the_pg_binary_provenance_checks() -> None:
    """The provenance hook refuses a run while any artifact it knows is stale;
    the gate rebuilds `secantusd-pg` at the path that hook checks."""
    spec = importlib.util.spec_from_file_location("provenance", ROOT / "tools/provenance.py")
    assert spec and spec.loader
    prov = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(prov)
    assert f'"{prov.PGSERVER_REL}"' in (ROOT / "scripts/gate_rm.py").read_text()


def test_runs_ci_async_oplog_storage_step_with_its_environment() -> None:
    """The gate's async-oplog storage step uses the environment CI's does."""
    wf = (ROOT / ".github/workflows/test.yml").read_text()
    start = wf.index("- name: Cargo test (secantus-storage, async-oplog lane)")
    block = wf[start : wf.index("- name: ", start + 1)]
    for key, value in _gate_rm().ASYNC_OPLOG_ENV.items():
        assert f'{key}: "{value}"' in block
    assert "working-directory: crates/secantus-storage" in block


def test_a_panic_is_read_from_test_logs_not_build_logs(tmp_path: Path) -> None:
    """A build that recompiles WiredTiger quotes its source (`WT_PANIC`) in
    compiler warnings; only a step that ran something can record a panic."""
    (tmp_path / "binary-build.log").write_text(
        '  165 |                 WT_IGNORE_RET(__wt_panic(session, WT_PANIC, "self-deadlock"));\n'
    )
    (tmp_path / "storage-test.log").write_text("ok\n")
    (tmp_path / "ci-selected.log").write_text("[WT_PANIC: WiredTiger library panic]\n")
    assert _gate_rm().panicked_logs(tmp_path) == ["ci-selected.log"]


def test_rebuilds_the_core_extension_before_the_suites() -> None:
    """`_secantus_core` is rebuilt from HEAD (`./inv sync`) with the binaries,
    so provenance does not abort the suites after an amend or a rebase."""
    src = (ROOT / "scripts/gate_rm.py").read_text()
    assert 'g.step("core-sync", ["./inv", "sync"])' in src
    assert src.index('"core-sync"') < src.index('"ci-selected"')


def test_storage_tests_run_only_when_the_storage_crates_change() -> None:
    g = _gate_rm()
    assert not g.touches_storage(
        ["crates/secantus-commands/src/crud.rs", "tasks/backlog-rust-mongodb.md"]
    )
    assert not g.touches_storage(["crates/secantus-storage-adapter/tests/profiler_wt.rs"])
    assert g.touches_storage(["crates/secantus-storage/src/lib.rs"])
    assert g.touches_storage(["cmake/patch_wt_dsync.py"])
    assert g.touches_storage(["vendor/wiredtiger"])


def test_only_runs_the_named_steps_and_skip_is_recorded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A step first waits for the load average to fall under 10, up to 30
    # minutes: under the gate's own load this test blocked past the per-test
    # timeout and took its xdist worker down (2026-11-09). It checks which
    # steps run, not the wait.
    monkeypatch.setenv("SECANTUS_LOAD_MAX_WAIT", "0")
    g = _gate_rm()
    gate = g.Gate(tmp_path, only={"a", "c"})
    gate.step("a", ["true"])
    gate.step("b", ["true"])
    gate.skip("c", "nothing changed")
    gate.skip("d", "not asked for")
    assert (tmp_path / "a.rc").read_text().strip() == "0"
    assert not (tmp_path / "b.rc").exists()
    assert (tmp_path / "c.rc").read_text().strip() == "skipped"
    assert not (tmp_path / "d.rc").exists()


def test_a_step_line_gives_its_time_and_the_load_beside_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("SECANTUS_LOAD_MAX_WAIT", "0")
    g = _gate_rm()
    g.Gate(tmp_path).step("a", ["true"])
    assert re.search(r"^    a=0 \(\d+s\) load \d+\.\d->\d+\.\d$", capsys.readouterr().out, re.M)


def test_ci_selected_logs_its_slowest_tests() -> None:
    text = (ROOT / "scripts/gate_rm.py").read_text()
    assert '"--durations=25"' in text


def test_postgres_server_tests_run_only_when_its_answers_can_change() -> None:
    g = _gate_rm()
    assert g.is_pg_server_test("tests/test_rust_pg_corpora.py")
    assert g.is_pg_server_test("tests/test_rust_pgserver_differential.py")
    assert g.is_pg_server_test("tests/test_pgserver_embedded.py")
    assert not g.is_pg_server_test("tests/test_crash_recovery.py")
    assert not g.pg_needed(["crates/secantus-commands/src/crud.rs", "changelog.d/x.md"])
    assert g.pg_needed(["crates/secantus-core/src/lib.rs"])


def test_the_gate_checks_the_backlog_ids_it_adds() -> None:
    text = (ROOT / "scripts/gate_rm.py").read_text()
    assert '"check-ids", "--file", "backlog-rust-mongodb.md"' in text
