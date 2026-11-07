"""scripts/gate_rm.py runs the storage-engine job's named files, read from test.yml.

If a step is renamed or its list moves, the gate must fail loudly rather than
run fewer files than CI does.
"""

from __future__ import annotations

import importlib.util
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
