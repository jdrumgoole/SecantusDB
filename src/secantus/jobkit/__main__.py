"""``python -m secantus.jobkit <task> [args...]`` — run a tracked invoke job.

This entrypoint imports the ``secantus`` package (heavy), so it is only for a
synced environment. The build-free path used by unsynced worktrees is the
repo-root ``./inv`` wrapper, which loads ``jobkit._core`` by file path instead.
"""

from __future__ import annotations

import sys

from secantus.jobkit._core import run_tracked


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args:
        sys.stderr.write("usage: python -m secantus.jobkit <task> [args...]\n")
        return 2
    return run_tracked(args)


if __name__ == "__main__":
    sys.exit(main())
