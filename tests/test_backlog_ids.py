"""`tools/backlog.py ids` reserves each new ID on the remote before writing it.

Two checkouts that allocate at once must never write the same ID. Until
2026-11-08 the next ID was "one past the highest in this checkout's file",
and RP-571 and RP-606 were each handed out twice that day: once by the
pg-batch lanes, once on main by another session, neither able to see the
other's file. These tests run against a local bare repository standing in
for `origin`, with real git and no network.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent

RP_FILE = "backlog-rust-postgres.md"
RM_FILE = "backlog-rust-mongodb.md"

BACKLOG = """# Rust PostgreSQL backlog

- [ ] **An item that has an ID.** Text.
  Backlog-ID: RP-001
  Verify: none -- a fixture
- [ ] **A new item.** Text.
  Verify: none -- a fixture
"""


def _backlog():
    spec = importlib.util.spec_from_file_location("backlog_tool_ids", ROOT / "tools/backlog.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


@pytest.fixture
def world(tmp_path, monkeypatch):
    """A bare `origin` holding a backlog on main, and two clones of it."""
    gitconfig = tmp_path / "gitconfig"
    gitconfig.write_text("")
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(gitconfig))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    for var in ("GIT_AUTHOR", "GIT_COMMITTER"):
        monkeypatch.setenv(f"{var}_NAME", "backlog test")
        monkeypatch.setenv(f"{var}_EMAIL", "backlog-test@example.invalid")
    origin = tmp_path / "origin.git"
    _git(tmp_path, "init", "--quiet", "--bare", "-b", "main", str(origin))
    seed = tmp_path / "seed"
    _git(tmp_path, "clone", "--quiet", str(origin), str(seed))
    (seed / "tasks").mkdir()
    (seed / "tasks" / RP_FILE).write_text(BACKLOG)
    (seed / "tasks" / RM_FILE).write_text("# Rust MongoDB backlog\n")
    _git(seed, "add", "-A")
    _git(seed, "commit", "--quiet", "-m", "backlog")
    _git(seed, "push", "--quiet", "origin", "HEAD:main")
    clones = {}
    for name in ("a", "b"):
        clones[name] = tmp_path / name
        _git(tmp_path, "clone", "--quiet", str(origin), str(clones[name]))
    return {"origin": origin, "seed": seed, **clones}


def _assign(mod, clone: Path) -> str:
    """Run `ids` in `clone`; return the ID it wrote on the new item."""
    mod.TASKS = clone / "tasks"
    assert mod.assign_ids(repo=clone) == 1
    ids = [it.id for it in mod.parse(RP_FILE)]
    assert ids[0] == "RP-001"
    return ids[1]


def _reserved(origin: Path) -> list[str]:
    out = _git(origin, "for-each-ref", "--format=%(refname)", "refs/backlog-ids/")
    return sorted(r.rsplit("/", 1)[1] for r in out.split())


def test_a_reserved_id_cannot_be_reserved_again(world):
    mod = _backlog()
    assert mod.reserve("RP-002", repo=world["a"]) is True
    assert mod.reserve("RP-002", repo=world["b"]) is False
    assert _reserved(world["origin"]) == ["RP-002"]


def test_two_checkouts_never_write_the_same_id(world):
    mod = _backlog()
    assert _assign(mod, world["a"]) == "RP-002"
    # b's file still tops out at RP-001; the reservation is what it sees.
    assert _assign(mod, world["b"]) == "RP-003"
    assert _reserved(world["origin"]) == ["RP-002", "RP-003"]


def test_a_number_taken_between_the_count_and_the_push_is_skipped(world, monkeypatch):
    # The race the listing cannot close: b reserves RP-002 after a has
    # counted. a's push of RP-002 is rejected, so a moves on to RP-003.
    mod = _backlog()
    monkeypatch.setattr(mod, "pushed_ids", lambda *a, **k: [])
    assert mod.reserve("RP-002", repo=world["b"]) is True
    assert _assign(mod, world["a"]) == "RP-003"


def test_ids_on_another_branch_are_counted(world):
    # An ID filed by a checkout that never reserved it (an older copy of the
    # tool) is still seen, from that branch's copy of the file.
    mod = _backlog()
    seed = world["seed"]
    path = seed / "tasks" / RP_FILE
    path.write_text(
        path.read_text().replace(
            "- [ ] **A new item.**",
            "- [ ] **Filed elsewhere.** Text.\n"
            "  Backlog-ID: RP-040\n"
            "  Verify: none -- a fixture\n"
            "- [ ] **A new item.**",
        )
    )
    _git(seed, "commit", "--quiet", "-am", "an unreserved ID on another branch")
    _git(seed, "push", "--quiet", "origin", "HEAD:refs/heads/other-batch")
    assert _assign(mod, world["a"]) == "RP-041"


def test_an_unreachable_remote_writes_nothing(world):
    mod = _backlog()
    clone = world["a"]
    _git(clone, "remote", "set-url", "origin", str(clone.parent / "gone.git"))
    before = (clone / "tasks" / RP_FILE).read_text()
    mod.TASKS = clone / "tasks"
    with pytest.raises(mod.ReservationError):
        mod.assign_ids(repo=clone)
    assert (clone / "tasks" / RP_FILE).read_text() == before


def test_nothing_to_assign_needs_no_remote(world):
    mod = _backlog()
    clone = world["a"]
    path = clone / "tasks" / RP_FILE
    path.write_text(
        BACKLOG.replace("- [ ] **A new item.** Text.\n  Verify: none -- a fixture\n", "")
    )
    _git(clone, "remote", "set-url", "origin", str(clone.parent / "gone.git"))
    mod.TASKS = clone / "tasks"
    assert mod.assign_ids(repo=clone) == 0
