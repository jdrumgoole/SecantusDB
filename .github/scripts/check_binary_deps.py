#!/usr/bin/env python3
"""Fail the release if a binary we are about to publish links something the
machine that downloads it may not have.

This exists because a published archive can be broken in a way NOTHING else in
the pipeline notices. Both macOS binaries of 2026-09-19 --- secantusd-rs
0.5.3-beta.164 and secantusd-pg 0.1.0-beta.1 --- linked liblz4 by its absolute
Homebrew path, `/opt/homebrew/opt/lz4/lib/liblz4.1.dylib`, and would not launch
on a Mac without Homebrew lz4 installed there. The build succeeded, the smoke
test passed (the runner HAS Homebrew), and the archive shipped. `otool -L` on
the artifact was the only thing that ever reported it, and nobody was running
`otool -L`.

So: run it here, on the artifact, before it is packaged, against a per-platform
allowlist of things every target machine genuinely has.
"""

from __future__ import annotations

import re
import struct
import subprocess
import sys
from pathlib import Path

# Linux: the glibc family only. Anything else -- a compressor, a TLS library --
# is a dependency the downloader has to install first, which is exactly what a
# "standalone binary" promises they will not have to do.
LINUX_ALLOWED = re.compile(
    r"^(libc|libm|libdl|librt|libpthread|libgcc_s|ld-linux[^.]*)\.so(\.\d+)?$"
)
# macOS: the SDK's own dylibs and system frameworks. NOT /opt/homebrew, NOT
# /usr/local -- those are the builder's machine, not the user's.
MACOS_ALLOWED = re.compile(r"^(/usr/lib/|/System/Library/)")
# Windows: OS DLLs. Notably absent are VCRUNTIME140/MSVCP140, whose presence
# would mean the archive needs the Visual C++ redistributable.
WINDOWS_ALLOWED = {
    "advapi32.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "crypt32.dll",
    "kernel32.dll",
    "ntdll.dll",
    "secur32.dll",
    "user32.dll",
    "ws2_32.dll",
    "userenv.dll",
    "shell32.dll",
    "ole32.dll",
    "oleaut32.dll",
}
WINDOWS_ALLOWED_PREFIXES = ("api-ms-win-", "ext-ms-win-")


def macos_deps(path: Path) -> list[str]:
    out = subprocess.run(
        ["otool", "-L", str(path)], capture_output=True, text=True, check=True
    ).stdout
    # First line is the binary's own name; the rest are "\t<path> (compat ...)".
    return [ln.strip().split(" ", 1)[0] for ln in out.splitlines()[1:] if ln.strip()]


def linux_deps(path: Path) -> list[str]:
    data = path.read_bytes()
    if data[:4] != b"\x7fELF":
        raise SystemExit(f"{path}: not an ELF binary")
    e_shoff = struct.unpack_from("<Q", data, 0x28)[0]
    e_shentsize, e_shnum, _ = struct.unpack_from("<HHH", data, 0x3A)
    sections = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        _, sh_type = struct.unpack_from("<II", data, off)
        sh_off, sh_size = struct.unpack_from("<QQ", data, off + 0x18)
        sh_link = struct.unpack_from("<I", data, off + 0x28)[0]
        sections.append((sh_type, sh_off, sh_size, sh_link))
    needed: list[str] = []
    for sh_type, off, size, link in sections:
        if sh_type != 6:  # SHT_DYNAMIC
            continue
        stroff = sections[link][1]
        for j in range(size // 16):
            tag, val = struct.unpack_from("<qQ", data, off + j * 16)
            if tag == 0:
                break
            if tag == 1:  # DT_NEEDED
                end = data.index(b"\0", stroff + val)
                needed.append(data[stroff + val : end].decode())
    return needed


def windows_deps(path: Path) -> list[str]:
    data = path.read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise SystemExit(f"{path}: not a PE binary")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    dd = opt + (112 if magic == 0x20B else 96)
    imp_rva = struct.unpack_from("<I", data, dd + 8)[0]
    sections = []
    for i in range(nsec):
        off = opt + opt_size + i * 40
        _vsz, va, _rawsz, raw = struct.unpack_from("<IIII", data, off + 8)
        vsz = struct.unpack_from("<I", data, off + 8)[0]
        sections.append((va, vsz, raw))

    def to_off(rva: int) -> int | None:
        for va, vsz, raw in sections:
            if va <= rva < va + max(vsz, 1):
                return raw + (rva - va)
        return None

    names: list[str] = []
    off = to_off(imp_rva)
    if off is None:
        return names
    while True:
        entry = data[off : off + 20]
        if len(entry) < 20 or entry == b"\0" * 20:
            break
        name_rva = struct.unpack_from("<I", entry, 12)[0]
        if name_rva == 0:
            break
        no = to_off(name_rva)
        if no is None:
            break
        names.append(data[no : data.index(b"\0", no)].decode())
        off += 20
    return names


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: check_binary_deps.py <linux|macos|windows> <binary>")
    platform, target = sys.argv[1], Path(sys.argv[2])
    if not target.is_file():
        raise SystemExit(f"{target}: no such file")

    if platform == "macos":
        deps = macos_deps(target)
        bad = [d for d in deps if not MACOS_ALLOWED.match(d)]
    elif platform == "linux":
        deps = linux_deps(target)
        bad = [d for d in deps if not LINUX_ALLOWED.match(d)]
    elif platform == "windows":
        deps = windows_deps(target)
        bad = [
            d
            for d in deps
            if d.lower() not in WINDOWS_ALLOWED
            and not d.lower().startswith(WINDOWS_ALLOWED_PREFIXES)
        ]
    else:
        raise SystemExit(f"unknown platform {platform!r}")

    print(f"{target.name} links {len(deps)} shared librar{'y' if len(deps) == 1 else 'ies'}:")
    for d in deps:
        print(f"  {'FAIL' if d in bad else 'ok  '}  {d}")

    if bad:
        print(
            f"\n::error::{target.name} links {len(bad)} librar"
            f"{'y' if len(bad) == 1 else 'ies'} a user's machine may not have: "
            + ", ".join(bad)
            + ". A standalone binary must not depend on the builder's "
            "environment -- link it statically, or add it to the allowlist in "
            ".github/scripts/check_binary_deps.py with a reason."
        )
        return 1
    print("\nAll dependencies are on the allowlist for this platform.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
