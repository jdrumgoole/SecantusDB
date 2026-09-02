"""Idempotent patch for vendor/wiredtiger/lang/python/wiredtiger.i (Python 3 C API).

WiredTiger 7.0's hand-written SWIG typemaps call three functions that were
REMOVED in Python 3: `PyInt_AsLong`, `PyInt_FromLong` and
`PyString_InternFromString`. SWIG copies typemap bodies into the generated
wrapper verbatim, so they land in `wiredtigerPYTHON_wrap.c` unchanged.

This is not only a build problem. Under Python 3 those symbols do not exist,
so the code paths that use them — the `key_format` / `value_format`
accessors and the `WT_MODIFY` offset/size lists — cannot work; a compiler
that merely warns produces an extension with undefined symbols in it.

It surfaced as a build FAILURE rather than a latent one because implicit
function declarations became an error in C99 and modern Clang now enforces
that: the file compiles on an older toolchain with warnings and fails hard on
a newer one. That made it look intermittent in CI, since it is only compiled
when the WiredTiger build is not restored from cache, so whichever job had a
cold cache failed while its siblings passed.

The replacements are the documented Python 3 equivalents, unchanged in
behaviour for these uses:

    PyInt_AsLong              -> PyLong_AsLong
    PyInt_FromLong            -> PyLong_FromLong
    PyString_InternFromString -> PyUnicode_InternFromString

Reapplying is a no-op — the marker comment is detected.
"""

from __future__ import annotations

import sys
from pathlib import Path

PATCH_MARKER = "/* secantus-patch: Python 2 C API -> Python 3 */"

# Longest name first so no replacement is a prefix of another.
REPLACEMENTS = (
    ("PyString_InternFromString", "PyUnicode_InternFromString"),
    ("PyInt_FromLong", "PyLong_FromLong"),
    ("PyInt_AsLong", "PyLong_AsLong"),
)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-wiredtiger.i>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    text = path.read_text()
    if PATCH_MARKER in text:
        print(f"already patched: {path}")
        return 0

    replaced = 0
    for old, new in REPLACEMENTS:
        count = text.count(old)
        if count:
            text = text.replace(old, new)
            replaced += count

    if not replaced:
        # Not an error: a future WiredTiger may ship Python 3 typemaps
        # already. Say so, and leave the marker off so a later vendor bump
        # that reintroduces them is patched again.
        print(f"no Python 2 C API calls found in {path}")
        return 0

    path.write_text(f"{PATCH_MARKER}\n{text}")
    print(f"patched {replaced} Python 2 C API call(s) in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
