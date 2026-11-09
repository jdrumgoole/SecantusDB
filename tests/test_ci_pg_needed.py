"""tools/ci_pg_needed.py decides whether a PR needs CI's pg-oracle lanes.

It is an allow-list of MongoDB-only paths: anything it does not name must run
the PostgreSQL lanes, so a forgotten path costs time, never coverage.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _tool():
    spec = importlib.util.spec_from_file_location("ci_pg_needed", ROOT / "tools/ci_pg_needed.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_a_mongodb_only_change_skips_pg_oracle() -> None:
    files = [
        "crates/secantus-commands/src/crud.rs",
        "tests/backlog/test_rm_355_364.py",
        "tests/mongod_live/test_slow_query_lines.py",
        "tasks/backlog-rust-mongodb.md",
        "changelog.d/rm-999-x.md",
        "tests/test_gate_rm.py",
    ]
    assert _tool().pg_needed(files) is False


def test_anything_the_postgres_server_links_runs_pg_oracle() -> None:
    t = _tool()
    for shared in (
        "crates/secantus-core/src/sorter.rs",
        "crates/secantus-storage/src/lib.rs",
        "crates/Cargo.lock",
        "vendor/icu4c-57.1/source/common/uchar.cpp",
        ".github/workflows/test.yml",
        "tests/backlog/conftest.py",
        "some/new/dir/file.rs",
    ):
        assert t.pg_needed(["crates/secantus-commands/src/crud.rs", shared]) is True, shared


def test_an_empty_change_runs_pg_oracle() -> None:
    assert _tool().pg_needed([]) is True


def test_the_allow_list_names_no_crate_the_postgres_server_links() -> None:
    linked = (ROOT / "crates/secantus-pg/Cargo.toml").read_text()
    for path in _tool().MONGO_ONLY:
        if path.startswith("crates/"):
            crate = path.split("/")[1]
            assert f'path = "../{crate}"' not in linked, crate


def test_the_workflow_gates_pg_oracle_on_the_tool() -> None:
    wf = (ROOT / ".github/workflows/test.yml").read_text()
    job = wf[wf.index("\n  pg-oracle:\n") :]
    head = job[: job.index("steps:")]
    assert "needs: changes" in head
    assert "if: needs.changes.outputs.pg == 'true'" in head
    assert "python3 tools/ci_pg_needed.py" in wf
