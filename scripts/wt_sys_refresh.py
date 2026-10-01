"""Regenerate the WiredTiger source copy inside ``crates/secantus-wiredtiger-sys``.

The crate builds WiredTiger with no Python on the user's machine, so it ships a
copy of ``vendor/wiredtiger`` with the build patches ALREADY applied. This
script is the only thing that writes that copy, and it applies the patches with
the very same ``cmake/patch_wt_*.py`` scripts the wheel's CMake build runs, so
the two WiredTiger builds cannot drift apart by patching differently.

Only the patches a Python-free static build needs are applied:

- ``patch_wt_strict.py`` -- drop ``-Werror`` (newer compilers add warnings);
- ``patch_wt_musl.py`` -- ``off64_t`` -> ``off_t`` for musl.

The three Python-binding patches have nothing to act on: ``lang/`` is not
copied. On top of those, the top-level ``CMakeLists.txt`` stops adding the
bench / example / test / utility subdirectories, which are not copied either
(they are most of the tree, and the crate builds only the library), and
``cmake/configs/base.cmake`` stops REQUIRING the Python development headers.

The digest of the result is written to ``wiredtiger.sha256`` beside the crate,
which is committed. ``tests/test_wt_sys_fresh.py`` regenerates into a temporary
directory and compares digests, so a submodule bump or a patch-script change
that was not followed by a refresh fails a test instead of shipping a crate
built from different WiredTiger source than the wheel.

Usage: ``python scripts/wt_sys_refresh.py [--dest DIR] [--check]``
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
VENDOR = REPO / "vendor" / "wiredtiger"
CRATE = REPO / "crates" / "secantus-wiredtiger-sys"
DEFAULT_DEST = CRATE / "wiredtiger"
STAMP = CRATE / "wiredtiger.sha256"
# Per-file hashes, so a mismatch names the files rather than only failing.
MANIFEST = CRATE / "wiredtiger.manifest"

# Top-level entries of vendor/wiredtiger that the library build does not need.
SKIP_TOP = {"bench", "examples", "lang", "test", "tools", ".git", ".github"}
# From dist/, only the file list the CMake build parses.
DIST_KEEP = {"filelist"}

TRIM_MARKER = "# secantus-trimmed: "
TRIMMED_SUBDIRS = (
    "add_subdirectory(src/utilities)",
    "add_subdirectory(bench/wtperf)",
    "add_subdirectory(bench/tiered)",
    "add_subdirectory(bench/wt2853_perf)",
    "add_subdirectory(examples)",
    "add_subdirectory(test)",
)

# base.cmake probes for Python 3 DEVELOPMENT headers unconditionally, even with
# ENABLE_PYTHON=OFF, and the probe is REQUIRED -- so without this a crate build
# fails on any machine lacking them, and picks up the host's Python include dir
# on one that has them. The crate never builds the Python API.
PYTHON_PROBE = "source_python3_package(python_libs python_version python_executable)\n"

PATCHES = (
    ("patch_wt_strict.py", "cmake/strict/strict_flags_helpers.cmake"),
    ("patch_wt_musl.py", "src/os_posix/os_fs.c"),
)


def _copy(dest: Path) -> None:
    if not (VENDOR / "CMakeLists.txt").exists():
        raise SystemExit(f"{VENDOR} is empty: run `git submodule update --init vendor/wiredtiger`")
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    for entry in sorted(VENDOR.iterdir()):
        if entry.name in SKIP_TOP:
            continue
        if entry.name == "dist":
            (dest / "dist").mkdir()
            for keep in DIST_KEEP:
                shutil.copy2(entry / keep, dest / "dist" / keep)
        elif entry.is_dir():
            shutil.copytree(entry, dest / entry.name, symlinks=True)
        else:
            shutil.copy2(entry, dest / entry.name)


def _trim_cmakelists(dest: Path) -> None:
    path = dest / "CMakeLists.txt"
    lines = path.read_text().splitlines(keepends=True)
    seen = set()
    out = []
    for line in lines:
        stripped = line.strip()
        if stripped in TRIMMED_SUBDIRS and not line.startswith(" "):
            seen.add(stripped)
            out.append(TRIM_MARKER + line)
        else:
            out.append(line)
    missing = set(TRIMMED_SUBDIRS) - seen
    if missing:
        # Fail loudly: an upstream layout change must be looked at, not skipped.
        raise SystemExit(f"CMakeLists.txt no longer has top-level {sorted(missing)}")
    path.write_text("".join(out))

    base = dest / "cmake" / "configs" / "base.cmake"
    text = base.read_text()
    if text.count(PYTHON_PROBE) != 1:
        raise SystemExit("cmake/configs/base.cmake no longer has the Python probe line")
    base.write_text(text.replace(PYTHON_PROBE, TRIM_MARKER + PYTHON_PROBE))


def _patch(dest: Path) -> None:
    for script, target in PATCHES:
        subprocess.run(
            [sys.executable, str(REPO / "cmake" / script), str(dest / target)],
            check=True,
            stdout=subprocess.DEVNULL,
        )


def manifest(root: Path) -> dict[str, str]:
    """Each file's relative path -> sha256 of its bytes."""
    out = {}
    for path in sorted(p for p in root.rglob("*") if p.is_file() or p.is_symlink()):
        data = str(path.readlink()).encode() if path.is_symlink() else path.read_bytes()
        # Line endings are not content: a Windows checkout (core.autocrlf) and
        # the patch scripts' text-mode writes both produce CRLF there, which
        # changes no byte the compiler acts on but would change the digest.
        data = data.replace(b"\r\n", b"\n")
        out[path.relative_to(root).as_posix()] = hashlib.sha256(data).hexdigest()
    return out


def render(files: dict[str, str]) -> str:
    return "".join(f"{h}  {rel}\n" for rel, h in sorted(files.items()))


def parse(text: str) -> dict[str, str]:
    return {rel: h for h, rel in (line.split("  ", 1) for line in text.splitlines() if line)}


def digest(root: Path) -> str:
    """One digest over the whole manifest."""
    return hashlib.sha256(render(manifest(root)).encode()).hexdigest()


def refresh(dest: Path) -> str:
    _copy(dest)
    _trim_cmakelists(dest)
    _patch(dest)
    return digest(dest)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    ap.add_argument(
        "--check", action="store_true", help="compare with the stamp; write nothing to it"
    )
    args = ap.parse_args()
    got = refresh(args.dest)
    files = manifest(args.dest)
    if args.check:
        want = STAMP.read_text().strip()
        if got != want:
            print(f"stale: regenerated {got}, stamp says {want}", file=sys.stderr)
            old = parse(MANIFEST.read_text())
            for rel in sorted(set(old) | set(files)):
                if old.get(rel) != files.get(rel):
                    print(f"  differs: {rel} ({old.get(rel)} -> {files.get(rel)})", file=sys.stderr)
            return 1
        print(f"fresh: {got}")
        return 0
    STAMP.write_text(got + "\n")
    MANIFEST.write_text(render(files))
    print(f"wrote {args.dest} ({got})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
