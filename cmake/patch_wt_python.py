"""Idempotent patch for vendor/wiredtiger/lang/python/CMakeLists.txt.

Two fixes the upstream WT release we vendor (mongodb-7.0.33) doesn't ship:

1. **macOS**: WT links the SWIG `_wiredtiger` extension against
   `${python_libs}` (the absolute path to libpython inside the framework).
   When cibuildwheel's `delocate-wheel` repairs the wheel, it bundles
   that libpython into `secantus/.dylibs/Python` and rewrites the
   extension's load path to point there. At import time the host
   interpreter has already loaded its own libpython, so two distinct
   libpython copies end up in the same process and the extension
   segfaults the moment SWIG calls back into the interpreter.

   The Python-on-Apple convention is to NOT link libpython explicitly
   and to pass `-undefined dynamic_lookup` so unresolved symbols
   resolve at load time against the host interpreter. We rewrite the
   `swig_link_libraries(...)` call to do that on Apple (and to also
   skip libpython on Linux, where extensions are conventionally
   unresolved-symbol too). Windows still needs the import lib (the
   loader has no equivalent of dynamic_lookup), so the original call
   is kept on WIN32.

2. **Windows**: WT only sets the SWIG output suffix on Darwin
   (`SUFFIX ".so"`). On Windows CMake defaults SHARED libraries to
   `.dll`, but Python's import machinery only looks for `.pyd`. We
   add an analogous Windows branch.

Reapplying is a no-op — markers in the file are detected and the
script exits 0.
"""

from __future__ import annotations

import sys
from pathlib import Path

LINK_MARKER = "# secantus-patch: link-libpython conditional"
SUFFIX_MARKER = "# secantus-patch: .pyd suffix on Windows"

ORIGINAL_LINK = (
    "swig_link_libraries(wiredtiger_python ${wiredtiger_target} ${python_libs})"
)
PATCHED_LINK = f"""{LINK_MARKER}
if(WIN32)
    swig_link_libraries(wiredtiger_python ${{wiredtiger_target}} ${{python_libs}})
else()
    swig_link_libraries(wiredtiger_python ${{wiredtiger_target}})
    if(APPLE)
        target_link_options(${{SWIG_MODULE_wiredtiger_python_REAL_NAME}}
            PRIVATE "LINKER:-undefined,dynamic_lookup")
    endif()
endif()"""

ORIGINAL_DARWIN_SUFFIX = """if(WT_DARWIN)
    set_target_properties(${swig_wt_target} PROPERTIES SUFFIX ".so")
endif()"""
PATCHED_DARWIN_SUFFIX = f"""{ORIGINAL_DARWIN_SUFFIX}
{SUFFIX_MARKER}
if(WIN32)
    set_target_properties(${{swig_wt_target}} PROPERTIES SUFFIX ".pyd")
endif()"""


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <path-to-lang/python/CMakeLists.txt>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    text = path.read_text()

    new = text
    actions: list[str] = []

    if LINK_MARKER in new:
        actions.append("link: already patched")
    elif ORIGINAL_LINK in new:
        new = new.replace(ORIGINAL_LINK, PATCHED_LINK)
        actions.append("link: patched")
    else:
        print(f"error: original swig_link_libraries(...) line not found in {path}", file=sys.stderr)
        return 1

    if SUFFIX_MARKER in new:
        actions.append("suffix: already patched")
    elif ORIGINAL_DARWIN_SUFFIX in new:
        new = new.replace(ORIGINAL_DARWIN_SUFFIX, PATCHED_DARWIN_SUFFIX)
        actions.append("suffix: patched")
    else:
        print(f"error: original WT_DARWIN suffix block not found in {path}", file=sys.stderr)
        return 1

    if new != text:
        path.write_text(new)
    print(f"{path}: " + "; ".join(actions))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
