"""Idempotent patch for vendor/wiredtiger/cmake/helpers.cmake.

WT's `source_python3_package` macro calls
`find_package(Python3 ... COMPONENTS Interpreter Development REQUIRED)`.
The `Development` component requires libpython itself (.so/.a/.lib),
which manylinux container images deliberately don't ship — Python C
extensions on Linux are conventionally unresolved-symbol against
libpython and resolve at import time. As a result, configuring WT
inside cibuildwheel's manylinux container fails with
``Could NOT find Python3 (missing: Python3_LIBRARIES Development)``.

`Development.Module` is the CMake-canonical component for building
Python C extensions: it brings in the headers + ABI metadata + the
``Python::Module`` link target (which is empty on POSIX and the
import-lib on Windows), but does NOT require libpython itself. That's
exactly what WT's lang/python build needs.

Reapplying is a no-op — the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "# secantus-patch: Development -> Development.Module (manylinux compat)"

ORIGINAL = (
    "find_package(Python3 ${required_version} COMPONENTS Interpreter Development REQUIRED)"
)
PATCHED = (
    f"{PATCH_MARKER}\n"
    "        find_package(Python3 ${required_version} "
    "COMPONENTS Interpreter Development.Module REQUIRED)"
)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-helpers.cmake>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0
    if ORIGINAL not in text:
        print(f"error: target find_package(Python3 ... Development) not in {path}", file=sys.stderr)
        return 1
    path.write_text(text.replace(ORIGINAL, PATCHED))
    print(f"patched Development -> Development.Module in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
