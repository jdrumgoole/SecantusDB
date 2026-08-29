"""A prebuilt WiredTiger home that tests CLONE instead of CREATE.

Creating a ``Storage()`` costs ~235 ms, and ~137 ms of that is WiredTiger
creating the ~12 tables that are not lazily created — measured at ~9.7 ms per
``session.create``. None of that work is per-test: every test starts from the
same empty schema. So build one pristine, cleanly-closed home per worker and
copy it, which measured ~127 ms — a ~1.9x cut of the per-test fixture floor.

See ``tasks/rust-test-harness-investigation.md`` for the full measurements and
for why this (rather than a faster harness language) is the lever that matters:
83 % of the per-test floor is inside WiredTiger's C library, so the only way to
go faster is to ask WiredTiger to do less.

The equivalence this relies on — a cloned home behaving identically to a created
one — is pinned by ``tests/test_wt_template.py``, not assumed.
"""

from __future__ import annotations

import shutil
import subprocess
import sys

__all__ = ["build_template", "clone_template"]


def _clone_flags() -> list[str] | None:
    """Platform flags asking ``cp`` for a copy-on-write clone, if it has any.

    macOS/APFS takes ``-c`` (clonefile); GNU coreutils takes
    ``--reflink=auto``, which silently degrades to a full copy on filesystems
    without reflink support. Both are optimisations only: a plain copy measured
    within 7 ms of the clone, so the fallback path is not a cliff.
    """
    if sys.platform == "darwin":
        return ["-c"]
    if sys.platform.startswith("linux"):
        return ["--reflink=auto"]
    return None


def build_template(dest: str) -> None:
    """Create a pristine ``Storage`` home at *dest* and close it cleanly.

    Built by running the real ``Storage`` constructor rather than by shipping a
    fixture directory, so the template can never drift from the current schema:
    add a table to ``Storage`` and the next template has it.

    The close must be clean — a half-written home would be cloned into every
    test that uses it.
    """
    from secantus.storage import Storage

    store = Storage(dest)
    store.close()


def clone_template(template: str, dest: str) -> None:
    """Copy the home at *template* to *dest* (which must not already exist)."""
    flags = _clone_flags()
    if flags is not None:
        completed = subprocess.run(
            ["cp", *flags, "-R", template, dest],
            capture_output=True,
        )
        if completed.returncode == 0:
            return
        # Fall through: the filesystem refused a reflink/clone (or `cp` lacks
        # the flag). A plain copy is correct, just marginally slower.
    shutil.copytree(template, dest)
