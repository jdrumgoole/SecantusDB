"""The cap on any one file the suite, or anything it starts, writes.

`tests/conftest.py` lowers the soft `RLIMIT_FSIZE` in every pytest process
(`_cap_file_size`) after a reference mongod's log filled this machine's disk
with one 427 GB file (2026-11-06).
"""

from __future__ import annotations

import ast
import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

resource = pytest.importorskip("resource")  # not on Windows


def _expected_cap() -> int:
    # Read from the source: `import conftest` may name tests/backlog's.
    tree = ast.parse((Path(__file__).parent / "conftest.py").read_text())
    (default,) = [
        ast.literal_eval(node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "_MAX_FILE_BYTES" for t in node.targets)
    ]
    return int(os.environ.get("SECANTUS_TEST_MAX_FILE_BYTES", default))


def test_this_process_is_capped() -> None:
    if _expected_cap() <= 0:
        pytest.skip("SECANTUS_TEST_MAX_FILE_BYTES=0 turns the cap off")
    soft, _ = resource.getrlimit(resource.RLIMIT_FSIZE)
    assert soft != resource.RLIM_INFINITY
    assert soft <= _expected_cap()


def test_a_child_process_inherits_the_cap_and_dies_past_it(tmp_path) -> None:
    """A server the suite starts is held to it too: a child writing past
    the limit is killed by SIGXFSZ, so a runaway log is a dead server and a
    failing test, never a full disk. The child is `sh` and then `dd`, not
    Python (which ignores SIGXFSZ at startup), as mongod and the Rust
    servers are not. It lowers its own limit to 1 MiB (it may only lower
    it), so the test writes 4 MiB at most, not 4 GiB."""
    if _expected_cap() <= 0:
        pytest.skip("SECANTUS_TEST_MAX_FILE_BYTES=0 turns the cap off")
    probe = "import resource; print(resource.getrlimit(resource.RLIMIT_FSIZE)[0])"
    seen = subprocess.run([sys.executable, "-c", probe], capture_output=True, text=True)
    assert int(seen.stdout) == resource.getrlimit(resource.RLIMIT_FSIZE)[0]

    out = tmp_path / "big"
    # `ulimit -f 2048` is 1 MiB or 2 MiB, as `sh` counts 512- or 1024-byte
    # blocks; `dd` asks for 4 MiB either way.
    script = f'ulimit -f 2048; exec dd if=/dev/zero of="{out}" bs=65536 count=64'
    proc = subprocess.run(["/bin/sh", "-c", script], capture_output=True, text=True)
    assert proc.returncode == -signal.SIGXFSZ, (proc.returncode, proc.stderr)
    assert out.stat().st_size <= 2 << 20
