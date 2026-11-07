"""`tools/backlog.py verify` reads a platform skip as ELSEWHERE, not a pass.

Each platform's mongod is the reference (Joe, 2026-11-07): a gap that exists
only where mongod's answer differs by platform is checked there, and on the
others its test skips itself as `platform: ...`.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _backlog():
    spec = importlib.util.spec_from_file_location("backlog_tool", ROOT / "tools/backlog.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    # Its dataclasses look their module up in sys.modules.
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


def _rec(outcome: str, message: str = "", fixed: bool = False) -> dict:
    return {
        "nodeid": "tests/backlog/test_x.py::test_y",
        "outcome": outcome,
        "assertion": outcome == "failed",
        "message": message,
        "ids": [] if fixed else ["RM-001"],
        "fixed_ids": ["RM-001"] if fixed else [],
    }


def test_a_platform_skip_is_elsewhere() -> None:
    b = _backlog()
    assert b.elsewhere(_rec("skipped", "platform: x86_64 only (here: arm64)"))
    assert not b.elsewhere(_rec("skipped", "needs a mongocryptd"))
    assert not b.elsewhere(_rec("passed"))


def test_a_regression_skipped_for_another_platform_is_not_regressed() -> None:
    b = _backlog()
    other = _rec("skipped", "platform: arm64 only (here: x86_64)", fixed=True)
    plain_skip = _rec("skipped", "no mongod", fixed=True)
    failed = _rec("failed", fixed=True)
    assert b.regressions_failed([other, _rec("passed", fixed=True)]) == []
    assert b.regressions_failed([plain_skip, failed]) == [plain_skip, failed]


def test_only_on_skips_with_the_prefix_verify_reads() -> None:
    import pytest

    spec = importlib.util.spec_from_file_location(
        "backlog_conftest", ROOT / "tests/backlog/conftest.py"
    )
    assert spec and spec.loader
    conf = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(conf)
    elsewhere = next(p for p in ("arm64", "x86_64") if p != conf.PLATFORM)
    with pytest.raises(pytest.skip.Exception) as e:
        conf.only_on(elsewhere, why="a test")
    assert str(e.value).startswith(_backlog().PLATFORM_SKIP)
    conf.only_on(conf.PLATFORM, why="a test")  # here: does not skip
