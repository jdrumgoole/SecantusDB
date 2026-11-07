"""Found while landing RM-305 (2026-11-07): the WiredTiger the standalone
binary links."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import rust_tasks


class _Recorder:
    def __init__(self) -> None:
        self.envs: list[dict[str, str]] = []

    def run(self, command: str, **kwargs: Any) -> None:
        self.envs.append(kwargs.get("env") or {})


def _complete_wt(d: Path) -> Path:
    (d / "include").mkdir(parents=True)
    (d / "include" / "wiredtiger.h").write_text("")
    (d / "libwiredtiger.a").write_bytes(b"")
    return d


@pytest.mark.backlog("RM-388")
def test_the_binary_links_the_wiredtiger_its_build_refreshed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`rust-binary-build` refreshes `build/rust-wt/wt-build` (rebuilt when a
    `cmake/patch_wt_*.py` patcher changes) and must link that one. It links
    whatever `_find_wt_build` finds first instead, and `build/cp312-*/` sorts
    ahead of `build/rust-wt/`: a checkout that has run `rust-server-build`
    once links the wheel's WiredTiger, which is re-patched only when the
    wheel is rebuilt (measured 2026-11-07: after RM-364's patcher landed, the
    binary lacked `txnNumUpdates` until `rust-server-build` ran)."""
    refreshed = _complete_wt(tmp_path / "build" / "rust-wt" / "wt-build")
    _complete_wt(tmp_path / "build" / "cp312-cp312-macosx_26_0_arm64" / "wt-build")
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("SECANTUS_WT_INCLUDE", raising=False)
    monkeypatch.delenv("SECANTUS_WT_LIB", raising=False)
    monkeypatch.setattr(rust_tasks, "_build_wiredtiger", lambda **_: refreshed)
    c = _Recorder()
    rust_tasks.rust_binary_build.body(c)
    assert [e.get("SECANTUS_WT_LIB") for e in c.envs] == [str(refreshed)]
