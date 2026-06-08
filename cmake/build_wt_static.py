#!/usr/bin/env python3
"""Build the vendored WiredTiger as a **static** library, for the
`secantus-storage` companion wheel.

Unlike the main `secantus` wheel (which builds WiredTiger *with* its SWIG Python
bindings via the root ``CMakeLists.txt``), the Rust storage extension only needs
``libwiredtiger.a`` + the generated ``wiredtiger.h``. So this build is
``ENABLE_PYTHON=OFF`` (no SWIG dependency at all) and static-only — the same
configuration that produced the working static lib the Rust crates link against.

It applies the same idempotent source patches the main build uses
(``patch_wt_strict`` for modern-compiler ``-Werror``; ``patch_wt_musl`` on
musl/Alpine) and forces the Ninja generator everywhere (matching the root build).

Usage::

    python3 cmake/build_wt_static.py <build-dir>

On success it prints the include dir and lib dir (suitable for
``SECANTUS_WT_INCLUDE`` / ``SECANTUS_WT_LIB``) as ``KEY=VALUE`` lines, and writes
them to ``$GITHUB_ENV`` if that is set (so CI steps downstream pick them up).
"""

from __future__ import annotations

import os
import platform
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
WT_SRC = REPO_ROOT / "vendor" / "wiredtiger"


def _is_musl() -> bool:
    # ``platform.libc_ver`` reports "" on musl; the manylinux/musllinux env var
    # is the reliable signal inside cibuildwheel/maturin containers.
    if "musllinux" in os.environ.get("AUDITWHEEL_PLAT", ""):
        return True
    return "musl" in (os.environ.get("SECANTUS_LIBC", ""))


def _run(cmd: list[str]) -> None:
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <build-dir>", file=sys.stderr)
        return 2
    if not (WT_SRC / "CMakeLists.txt").exists():
        print(
            f"WiredTiger source not found at {WT_SRC} — is the submodule checked out?",
            file=sys.stderr,
        )
        return 1

    build_dir = Path(sys.argv[1]).resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    py = sys.executable

    # Idempotent source patches (same scripts the root CMakeLists applies).
    _run([py, str(REPO_ROOT / "cmake" / "patch_wt_strict.py"),
          str(WT_SRC / "cmake" / "strict" / "strict_flags_helpers.cmake")])
    if platform.system() == "Linux" and _is_musl():
        _run([py, str(REPO_ROOT / "cmake" / "patch_wt_musl.py"),
              str(WT_SRC / "src" / "os_posix" / "os_fs.c")])

    _run([
        "cmake", "-S", str(WT_SRC), "-B", str(build_dir), "-G", "Ninja",
        "-DCMAKE_BUILD_TYPE=Release",
        "-DENABLE_STATIC=ON",
        "-DENABLE_SHARED=OFF",
        "-DENABLE_PYTHON=OFF",
        "-DENABLE_CPPSUITE=OFF",
        "-DWITH_PIC=ON",
        "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
    ])
    _run(["cmake", "--build", str(build_dir), "--target", "wiredtiger_static"])

    header = build_dir / "include" / "wiredtiger.h"
    lib_a = build_dir / "libwiredtiger.a"
    if not header.exists() or not lib_a.exists():
        print(f"expected {header} and {lib_a} after build", file=sys.stderr)
        return 1

    inc_dir = str(build_dir / "include")
    lib_dir = str(build_dir)
    print(f"SECANTUS_WT_INCLUDE={inc_dir}")
    print(f"SECANTUS_WT_LIB={lib_dir}")
    gh_env = os.environ.get("GITHUB_ENV")
    if gh_env:
        with open(gh_env, "a", encoding="utf-8") as fh:
            fh.write(f"SECANTUS_WT_INCLUDE={inc_dir}\n")
            fh.write(f"SECANTUS_WT_LIB={lib_dir}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
