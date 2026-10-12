"""The psycopg gauge must not inherit an IGNORED SIGINT.

A shell starts a background job with SIGINT ignored, and an ignored signal
survives fork and exec. Launched that way, every process of the suite was
deaf to Ctrl-C, so psycopg's two `test_ctrl_c` tests -- which Ctrl-C a client
inside `pg_sleep` and expect it to cancel -- failed (the sync one by hanging
for its whole budget) while passing in a foreground run of the same server.
The runner now gives the suite's process the default disposition.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from psycopg_validation import runner  # noqa: E402

_PROBE = "import signal; print(signal.getsignal(signal.SIGINT) is signal.default_int_handler)"


def _child_hears_sigint(preexec_fn) -> bool:
    out = subprocess.run(
        [sys.executable, "-c", _PROBE],
        capture_output=True,
        text=True,
        check=True,
        preexec_fn=preexec_fn,
    )
    return out.stdout.strip() == "True"


@pytest.mark.skipif(
    os.name != "posix",
    reason="POSIX signal inheritance: a background job's ignored SIGINT survives exec; "
    "Windows has no such inheritance and subprocess rejects preexec_fn there",
)
def test_suite_process_gets_the_default_sigint_even_from_a_background_launch():
    previous = signal.signal(signal.SIGINT, signal.SIG_IGN)
    try:
        # The trap itself: an ignored SIGINT is inherited by the child.
        assert not _child_hears_sigint(None)
        # What the runner passes to the suite's process.
        assert _child_hears_sigint(runner._default_sigint)
    finally:
        signal.signal(signal.SIGINT, previous)


def test_the_runners_scratch_directories_are_removed_at_exit(tmp_path: Path) -> None:
    """The gauge's server store and TLS files were left behind by every run
    (about 100 MB each). A directory `_scratch_dir` makes is gone once the
    process that made it has exited, however `main` returned."""
    script = (
        "import sys, tempfile\n"
        f"tempfile.tempdir = {str(tmp_path)!r}\n"
        "from psycopg_validation import runner\n"
        "d = runner._scratch_dir('secantus-psycopg-gauge-')\n"
        "open(d + '/WiredTiger', 'w').write('x')\n"
        "print(d)\n"
        "sys.exit(2)\n"
    )
    done = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        cwd=Path(__file__).resolve().parents[1],
        timeout=60,
    )
    assert done.returncode == 2, done.stderr
    made = Path(done.stdout.strip())
    assert made.parent == tmp_path and made.name.startswith("secantus-psycopg-gauge-")
    assert not made.exists()
    assert list(tmp_path.iterdir()) == []
