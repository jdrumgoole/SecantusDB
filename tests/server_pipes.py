"""Keep reading a server subprocess's pipes once its banner has been read.

A test that starts a server with ``stdout=PIPE`` (or ``stderr=PIPE``) and reads
only the listening banner leaves the rest of the server's output in a pipe
nobody reads. The pipe holds 64 KiB on Linux and macOS and about 4 KiB on
Windows; once it fills, every server thread that writes a line blocks on the
write, so commands hang and so does shutdown. On Windows that failed
``test_rust_binary_smoke.py`` (2026-11-11: pymongo's monitor cancelled the
operations of a server stuck writing its log).

Call :func:`drain` right after the banner is read. A test that needs the
output itself reads it its own way (``test_rust_binary_smoke.py``'s
``_Drain``, ``test_crash_recovery.py``'s ``_StderrTail``).
"""

from __future__ import annotations

import subprocess
import threading


def drain(proc: subprocess.Popen) -> None:
    """Discard everything ``proc`` writes to its piped stdout and stderr from
    now on, each on a daemon thread that ends at EOF."""
    for pipe in (proc.stdout, proc.stderr):
        if pipe is not None:
            threading.Thread(target=_discard, args=(pipe,), daemon=True).start()


def _discard(pipe: object) -> None:
    for _ in pipe:  # type: ignore[attr-defined]
        pass
