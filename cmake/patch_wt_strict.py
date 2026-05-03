"""Idempotent patch for vendor/wiredtiger/cmake/strict/strict_flags_helpers.cmake.

Comments out the two ``list(APPEND ..._flags "-Werror")`` lines (one for
GCC, one for Clang) so WiredTiger's strict-mode build doesn't fail on
warnings that newer compilers add (and which the WT release we vendor
predates). Run as part of the ExternalProject's PATCH_COMMAND.

Invoke as:
    python3 cmake/patch_wt_strict.py vendor/wiredtiger/cmake/strict/strict_flags_helpers.cmake

Reapplying is a no-op — looks for the original lines, skips if already
patched.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "# patched-out for modern-compiler compat:"
TARGETS = (
    'list(APPEND gnu_flags "-Werror")',
    'list(APPEND clang_flags "-Werror")',
)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-strict_flags_helpers.cmake>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0
    new = text
    replaced = 0
    for line in TARGETS:
        if line in new:
            new = new.replace(line, f"{PATCH_MARKER} {line}")
            replaced += 1
    if replaced == 0:
        print(f"warning: no -Werror lines found in {path}", file=sys.stderr)
        return 1
    path.write_text(new)
    print(f"patched {replaced} -Werror line(s) in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
