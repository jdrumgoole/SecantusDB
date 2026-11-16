"""Build the SecantusDB PyPI wheels from the Rust servers' release archives.

The wheel compiles nothing. It repackages the binaries the two release
workflows already built, smoke-tested and attached to their GitHub Releases:
``secantusd-mdb`` from a ``secantusdb-v*`` release and ``secantusd-pg`` from a
``secantusd-pg-v*`` release. One wheel per platform, tagged ``py3-none``
(the launcher is pure Python), with the platform tag read from the binaries
themselves: the newest glibc symbol version they need (Linux), or the
minimum macOS version in their Mach-O header.

The wheel's version is the MongoDB server's, converted to PEP 440
(``0.5.3-beta.196`` -> ``0.5.3b196``).

    python packaging/pypi/build_wheels.py --assets DIR --out dist \\
        --mongodb-version 0.5.3-beta.196 --postgres-version 0.1.0-beta.17
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import re
import struct
import sys
import tarfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
PACKAGE = HERE / "secantusdb"

# The Rust target triples the wheel ships, and the wheel-tag architecture.
TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux", "x86_64"),
    "aarch64-apple-darwin": ("macos", "arm64"),
}

_SEMVER_PRE = re.compile(r"^(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)\.(\d+))?$")
_PEP440_PRE = {"alpha": "a", "beta": "b", "rc": "rc"}


def pep440(version: str) -> str:
    """``0.5.3-beta.196`` -> ``0.5.3b196``; a release version is unchanged."""
    match = _SEMVER_PRE.match(version)
    if not match:
        raise ValueError(f"not a SemVer release or alpha/beta/rc pre-release: {version!r}")
    base, kind, number = match.groups()
    return base if kind is None else f"{base}{_PEP440_PRE[kind]}{number}"


def glibc_floor(binary: bytes) -> tuple[int, int]:
    """The newest ``GLIBC_2.N`` symbol version an ELF binary references."""
    if binary[:4] != b"\x7fELF":
        raise ValueError("not an ELF binary")
    versions = {
        (int(major), int(minor))
        for major, minor in re.findall(rb"GLIBC_(\d+)\.(\d+)(?:\.\d+)?\x00", binary)
    }
    if not versions:
        raise ValueError("no GLIBC_ symbol versions: not a glibc-linked binary")
    return max(versions)


_LC_VERSION_MIN_MACOSX = 0x24
_LC_BUILD_VERSION = 0x32
_PLATFORM_MACOS = 1


def macos_floor(binary: bytes) -> tuple[int, int]:
    """The minimum macOS version a 64-bit little-endian Mach-O binary declares."""
    magic, _cpu, _sub, _filetype, ncmds, _size, _flags, _res = struct.unpack_from("<8I", binary)
    if magic != 0xFEEDFACF:
        raise ValueError("not a 64-bit little-endian Mach-O binary")
    offset = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<2I", binary, offset)
        if cmd == _LC_BUILD_VERSION:
            platform, minos = struct.unpack_from("<2I", binary, offset + 8)
            if platform == _PLATFORM_MACOS:
                return minos >> 16, (minos >> 8) & 0xFF
        elif cmd == _LC_VERSION_MIN_MACOSX:
            (minos,) = struct.unpack_from("<I", binary, offset + 8)
            return minos >> 16, (minos >> 8) & 0xFF
        offset += cmdsize
    raise ValueError("no macOS minimum-version load command")


def platform_tag(target: str, binaries: list[bytes]) -> str:
    os_name, arch = TARGETS[target]
    if os_name == "linux":
        major, minor = max(glibc_floor(b) for b in binaries)
        return f"manylinux_{major}_{minor}_{arch}"
    major, minor = max(macos_floor(b) for b in binaries)
    # From macOS 11 on, pip only matches a wheel tagged with minor 0.
    return f"macosx_{major}_{minor if major < 11 else 0}_{arch}"


def read_archive(path: Path, binary_name: str, legacy_name: str | None = None) -> bytes:
    """The named binary from one release archive, after checking its sha256."""
    expected_sha = path.with_name(path.name + ".sha256")
    data = path.read_bytes()
    if expected_sha.exists():
        want = expected_sha.read_text().split()[0]
        got = hashlib.sha256(data).hexdigest()
        if want != got:
            raise ValueError(f"{path.name}: sha256 {got} does not match {want}")
    else:
        raise FileNotFoundError(f"{expected_sha.name} is missing: refusing an unverified archive")
    binary = None
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            if member.name.rsplit("/", 1)[-1] in (binary_name, legacy_name):
                extracted = tar.extractfile(member)
                assert extracted is not None
                binary = extracted.read()
    if binary is None:
        raise ValueError(f"{path.name} holds no {binary_name}")
    return binary


def _record_line(arcname: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{arcname},sha256={digest},{len(data)}"


def _metadata(version: str, mongodb_version: str, postgres_version: str) -> bytes:
    readme = (HERE / "README.md").read_text()
    lines = [
        "Metadata-Version: 2.4",
        "Name: SecantusDB",
        f"Version: {version}",
        "Summary: Single-node MongoDB- and PostgreSQL-wire servers over WiredTiger, "
        "for tests and CI",
        "Author-email: Joe Drumgoole <joe@joedrumgoole.com>",
        "License-Expression: GPL-2.0-only",
        "License-File: LICENSE",
        "License-File: LICENSE-ICU",
        "Project-URL: Homepage, https://secantusdb.com",
        "Project-URL: Source, https://github.com/jdrumgoole/SecantusDB",
        "Requires-Python: >=3.10",
        "Classifier: Programming Language :: Python :: 3",
        "Classifier: Topic :: Database :: Database Engines/Servers",
        "Classifier: Topic :: Software Development :: Testing",
        "Description-Content-Type: text/markdown",
        "",
        readme.replace("{mongodb_version}", mongodb_version).replace(
            "{postgres_version}", postgres_version
        ),
    ]
    return "\n".join(lines).encode()


def _zip_add(zf: zipfile.ZipFile, arcname: str, data: bytes, executable: bool = False) -> None:
    info = zipfile.ZipInfo(arcname, date_time=(2020, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (0o100755 if executable else 0o100644) << 16
    zf.writestr(info, data)


def build_wheel(
    target: str,
    assets: Path,
    out: Path,
    mongodb_version: str,
    postgres_version: str,
) -> Path:
    # Archives before 0.5.3-beta.197 named the MongoDB binary `secantusd-rs`;
    # the wheel installs it as `secantusd-mdb` either way.
    mdb = read_archive(
        assets / f"secantusdb-{mongodb_version}-{target}.tar.gz",
        "secantusd-mdb",
        legacy_name="secantusd-rs",
    )
    pg = read_archive(assets / f"secantusd-pg-{postgres_version}-{target}.tar.gz", "secantusd-pg")
    version = pep440(mongodb_version)
    tag = f"py3-none-{platform_tag(target, [mdb, pg])}"
    dist_info = f"secantusdb-{version}.dist-info"

    files: list[tuple[str, bytes, bool]] = []
    for src in sorted(PACKAGE.iterdir()):
        if src.is_file() and src.suffix in (".py", ".typed"):
            files.append((f"secantusdb/{src.name}", src.read_bytes(), False))
    files.append(("secantusdb/bin/secantusd-mdb", mdb, True))
    files.append(("secantusdb/bin/secantusd-pg", pg, True))
    # The licences come from the checkout the workflow runs from: archives
    # before 2026-10 carry no LICENSE-ICU.
    repo = HERE.parent.parent
    for name, src in (("LICENSE", "LICENSE"), ("LICENSE-ICU", "vendor/icu4c-57.1/LICENSE")):
        files.append((f"{dist_info}/licenses/{name}", (repo / src).read_bytes(), False))
    files.append(
        (f"{dist_info}/METADATA", _metadata(version, mongodb_version, postgres_version), False)
    )
    wheel_file = (
        "Wheel-Version: 1.0\n"
        "Generator: SecantusDB build_wheels.py\n"
        "Root-Is-Purelib: false\n"
        f"Tag: {tag}\n"
    )
    files.append((f"{dist_info}/WHEEL", wheel_file.encode(), False))
    entry_points = (
        "[console_scripts]\n"
        "secantusd-mdb = secantusdb._cli:mongodb\n"
        "secantusd-pg = secantusdb._cli:postgres\n"
    )
    files.append((f"{dist_info}/entry_points.txt", entry_points.encode(), False))

    out.mkdir(parents=True, exist_ok=True)
    wheel_path = out / f"secantusdb-{version}-{tag}.whl"
    record = [_record_line(name, data) for name, data, _ in files]
    record.append(f"{dist_info}/RECORD,,")
    with zipfile.ZipFile(wheel_path, "w") as zf:
        for name, data, executable in files:
            _zip_add(zf, name, data, executable)
        _zip_add(zf, f"{dist_info}/RECORD", ("\n".join(record) + "\n").encode())
    return wheel_path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--assets", type=Path, required=True, help="the release archives")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--mongodb-version", required=True, help="e.g. 0.5.3-beta.196")
    parser.add_argument("--postgres-version", required=True, help="e.g. 0.1.0-beta.17")
    parser.add_argument("--target", action="append", choices=sorted(TARGETS))
    args = parser.parse_args(argv)
    for target in args.target or sorted(TARGETS):
        wheel = build_wheel(
            target, args.assets, args.out, args.mongodb_version, args.postgres_version
        )
        print(wheel)
    return 0


if __name__ == "__main__":
    sys.exit(main())
