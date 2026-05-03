"""Idempotent patch for vendor/wiredtiger/src/os_posix/os_fs.c (musl compat).

WT's `sync_file_range` call casts offsets to `off64_t`. That type is a
glibc extension — musl libc (Alpine, used by musllinux wheels) doesn't
provide it, and WT's CMake unconditionally defines `HAVE_SYNC_FILE_RANGE`
on any Linux where the function exists (it does on musl too, just with
a different signature). Result: WT compiles cleanly on glibc, fails on
musl with `'off64_t' undeclared (first use in this function)`.

The fix is a one-line cast change: `(off64_t)0` -> `(off_t)0`. Both
`sync_file_range` overloads accept 0 in their offset/nbytes parameters
(it means "sync the entire file"), so the constant value is unchanged.
On glibc, `(off_t)0` implicitly converts to `off64_t`. On musl,
`(off_t)0` matches the function signature directly. Either way the
syscall is invoked the same way at runtime.

Reapplying is a no-op — the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "/* secantus-patch: off64_t -> off_t for musl compat */"

ORIGINAL = (
    "    WT_SYSCALL(sync_file_range(pfh->fd, (off64_t)0, (off64_t)0, "
    "SYNC_FILE_RANGE_WRITE), ret);"
)
PATCHED = (
    f"    {PATCH_MARKER}\n"
    "    WT_SYSCALL(sync_file_range(pfh->fd, (off_t)0, (off_t)0, "
    "SYNC_FILE_RANGE_WRITE), ret);"
)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-os_fs.c>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0
    if ORIGINAL not in text:
        print(f"error: target sync_file_range line not found in {path}", file=sys.stderr)
        return 1
    path.write_text(text.replace(ORIGINAL, PATCHED))
    print(f"patched off64_t -> off_t in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
