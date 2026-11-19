"""Idempotent patch for vendor/wiredtiger/cmake/strict/strict_flags_helpers.cmake.

Newer compilers add warnings that the WiredTiger release we vendor predates,
and WiredTiger's strict mode turns every warning into an error. Two edits:

* GCC and Clang: comment out the two ``list(APPEND ..._flags "-Werror")``
  lines.
* MSVC: keep ``/WX``, and disable only the one diagnostic MSVC 19.52 added
  that fires on this release, C4815 ("flexible array member in object will
  have no elements"). It fires on ``__log_record_verify`` (``src/log/log.c``),
  which copies a ``WT_LOG_RECORD`` header to the stack on purpose and never
  reads the ``record[]`` array through that copy. Narrower than dropping
  ``/WX``: any other new MSVC warning still fails the build. Found when the
  GitHub Windows image moved from MSVC 19.51 to 19.52 (image 20261111) and
  every Windows WiredTiger build broke.

Run as part of the ExternalProject's PATCH_COMMAND.

Invoke as:
    python3 cmake/patch_wt_strict.py vendor/wiredtiger/cmake/strict/strict_flags_helpers.cmake

Reapplying is a no-op: each edit is applied only where its original line is
still present, so a copy patched by an older version of this script gets the
newer edits and nothing twice.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "# patched-out for modern-compiler compat:"
MSVC_WX = 'list(APPEND cl_flags "/WX")'
MSVC_PATCHED = (
    f'{MSVC_WX}\n    list(APPEND cl_flags "/wd4815") '
    "# patched in: MSVC 19.52's C4815 (see cmake/patch_wt_strict.py)"
)

# (original line, its replacement, and a string present only once patched)
EDITS = (
    (
        'list(APPEND gnu_flags "-Werror")',
        f'{PATCH_MARKER} list(APPEND gnu_flags "-Werror")',
        f'{PATCH_MARKER} list(APPEND gnu_flags "-Werror")',
    ),
    (
        'list(APPEND clang_flags "-Werror")',
        f'{PATCH_MARKER} list(APPEND clang_flags "-Werror")',
        f'{PATCH_MARKER} list(APPEND clang_flags "-Werror")',
    ),
    (MSVC_WX, MSVC_PATCHED, '"/wd4815"'),
)


def patch(text: str) -> tuple[str, int, int]:
    """The patched text, the edits applied now, and those already present."""
    applied = already = 0
    for original, replacement, done in EDITS:
        if done in text:
            already += 1
        elif original in text:
            # Each original occurs once in the file; replace only that one.
            text = text.replace(original, replacement, 1)
            applied += 1
    return text, applied, already


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-strict_flags_helpers.cmake>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    new, applied, already = patch(path.read_text())
    if applied + already != len(EDITS):
        print(
            f"error: {path}: only {applied + already} of {len(EDITS)} strict-flag lines "
            "found; the WiredTiger source changed under this patch",
            file=sys.stderr,
        )
        return 1
    if applied == 0:
        print(f"already patched: {path}")
        return 0
    path.write_text(new)
    print(f"patched {applied} strict-flag line(s) in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
