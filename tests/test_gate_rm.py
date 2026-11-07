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
