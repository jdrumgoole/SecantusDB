"""The batch scripts keep the Mac awake with `caffeinate -w $$ &`, a child
that lives until the script itself exits. A bare `wait` in the same script
waits for it too, so the script never ends: fix_many.sh finished batch 9's
items and then hung (2026-11-09). Every `wait` there must name its pids.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ("scripts/fix_many.sh", "scripts/land_batch.sh")


@pytest.mark.parametrize("script", SCRIPTS)
def test_no_bare_wait_beside_caffeinate(script: str) -> None:
    text = (ROOT / script).read_text()
    if "caffeinate" not in text:
        return
    code = [ln.split("#", 1)[0] for ln in text.splitlines()]
    bare = [ln for ln in code if re.fullmatch(r"\s*wait\s*(;.*)?", ln)]
    assert not bare, f"{script}: a bare `wait` also waits for caffeinate and never returns"


def test_fix_many_exits_when_its_lanes_finish(tmp_path: Path) -> None:
    """Run fix_many's lane loop with a stub lane under a real `caffeinate`
    stand-in that outlives the script unless the script exits first."""
    text = (ROOT / "scripts/fix_many.sh").read_text()
    start = text.index("lanes=()")
    loop = text[start : text.index('wait "${lanes[@]}"', start) + len('wait "${lanes[@]}"')]
    sleeper = shutil.which("sleep")
    assert sleeper
    script = tmp_path / "loop.sh"
    script.write_text(
        "set -u\njobs=2\n"
        f"{sleeper} 120 >/dev/null 2>&1 &\n"  # stands in for `caffeinate -w $$`
        "keeper=$!\n"
        "lane() { :; }\n"
        f"{loop}\necho done\nkill $keeper\n"
    )
    t0 = time.monotonic()
    proc = subprocess.run(
        ["bash", str(script)], capture_output=True, text=True, timeout=60, env=os.environ
    )
    assert proc.stdout.strip() == "done"
    assert time.monotonic() - t0 < 30


def _item_is_open_fn() -> str:
    text = (ROOT / "scripts/fix_many.sh").read_text()
    start = text.index("item_is_open() {")
    return text[start : text.index("\n}\n", start) + 3]


def test_a_listed_item_already_closed_is_skipped_not_run(tmp_path: Path) -> None:
    """fix_many skips a listed ID whose `Backlog-ID:` line is gone from the
    branch the fixes land on: a session for it lands nothing, which stopped
    the batch (RM-397, closed by RM-403's fix, 2026-11-10)."""
    origin = tmp_path / "origin"
    work = tmp_path / "work"
    git = ["git", "-c", "user.email=t@t", "-c", "user.name=t"]
    subprocess.run([*git, "init", "-q", "-b", "rm-batch", str(origin)], check=True)
    (origin / "tasks").mkdir()
    (origin / "tasks/backlog-rust-mongodb.md").write_text(
        "- [ ] **open**\n  Backlog-ID: RM-001\n- [ ] **also open**\n  Backlog-ID: RM-0011\n"
    )
    subprocess.run([*git, "-C", str(origin), "add", "-A"], check=True)
    subprocess.run([*git, "-C", str(origin), "commit", "-qm", "x"], check=True)
    subprocess.run(["git", "clone", "-q", str(origin), str(work)], check=True)
    script = tmp_path / "check.sh"
    script.write_text(
        f"backlog=mongodb\n{_item_is_open_fn()}"
        "for id in RM-001 RM-002 RM-0011 RM-01; do\n"
        '  item_is_open "$id" rm-batch && echo "$id open" || echo "$id closed"\n'
        "done\n"
    )
    proc = subprocess.run(
        ["bash", str(script)], cwd=work, capture_output=True, text=True, timeout=60
    )
    assert proc.stdout.split("\n")[:4] == [
        "RM-001 open",
        "RM-002 closed",
        "RM-0011 open",
        "RM-01 closed",
    ], proc.stdout + proc.stderr


def test_fix_many_skips_a_closed_item_before_its_session() -> None:
    text = (ROOT / "scripts/fix_many.sh").read_text()
    check = text.index('! item_is_open "$id"')
    assert check < text.index("scripts/fix_one.sh -p", check)
