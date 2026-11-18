"""The PyPI wheel builder (packaging/pypi/build_wheels.py) and its launcher.

The wheel repackages the Rust servers' release archives; publish-pypi.yml
smoke-tests the real wheels on Linux and macOS. These tests pin the parts a
smoke run cannot see going wrong quietly: the PEP 440 version, the platform
tag read from the binaries, the RECORD hashes, the executable bits, the
refusal of an unverified archive, and the launcher's startup handling.
"""

from __future__ import annotations

import base64
import hashlib
import importlib.util
import io
import struct
import sys
import tarfile
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
PYPI = ROOT / "packaging" / "pypi"


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


build_wheels = _load("secantus_pypi_build_wheels", PYPI / "build_wheels.py")
sys.path.insert(0, str(PYPI))
try:
    from secantusdb import _server as launcher
finally:
    sys.path.remove(str(PYPI))


@pytest.mark.parametrize(
    ("semver", "pep440"),
    [
        ("0.5.3-beta.196", "0.5.3b196"),
        ("1.2.3-alpha.4", "1.2.3a4"),
        ("1.2.3-rc.1", "1.2.3rc1"),
        ("1.0.0", "1.0.0"),
    ],
)
def test_pep440(semver: str, pep440: str) -> None:
    assert build_wheels.pep440(semver) == pep440


@pytest.mark.parametrize("bad", ["0.5.3-beta", "0.5.3-dev.1", "v0.5.3", "0.5"])
def test_pep440_refuses_an_unknown_shape(bad: str) -> None:
    with pytest.raises(ValueError):
        build_wheels.pep440(bad)


def _elf(*glibc: str) -> bytes:
    return b"\x7fELF" + b"\x00" * 60 + b"".join(f"GLIBC_{v}\x00".encode() for v in glibc)


def _macho(major: int, minor: int, *, legacy: bool = False) -> bytes:
    version = (major << 16) | (minor << 8)
    if legacy:
        command = struct.pack("<4I", 0x24, 16, version, version)
    else:
        command = struct.pack("<6I", 0x32, 24, 1, version, version, 0)
    unrelated = struct.pack("<2I", 0x19, 8)  # LC_SEGMENT_64, skipped
    header = struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 2, 2, len(unrelated + command), 0, 0)
    return header + unrelated + command


def test_glibc_floor_is_the_newest_symbol_version() -> None:
    assert build_wheels.glibc_floor(_elf("2.2.5", "2.17", "2.34", "2.9")) == (2, 34)


def test_glibc_floor_refuses_a_binary_without_glibc() -> None:
    with pytest.raises(ValueError):
        build_wheels.glibc_floor(_elf())
    with pytest.raises(ValueError):
        build_wheels.glibc_floor(b"MZ not elf")


def test_macos_floor_reads_build_version_and_the_legacy_command() -> None:
    assert build_wheels.macos_floor(_macho(11, 0)) == (11, 0)
    assert build_wheels.macos_floor(_macho(10, 15, legacy=True)) == (10, 15)


def test_platform_tags() -> None:
    linux = "x86_64-unknown-linux-gnu"
    mac = "aarch64-apple-darwin"
    assert build_wheels.platform_tag(linux, [_elf("2.17"), _elf("2.28")]) == "manylinux_2_28_x86_64"
    # pip matches macOS 11+ wheels only with a 0 minor.
    assert build_wheels.platform_tag(mac, [_macho(14, 2), _macho(11, 0)]) == "macosx_14_0_arm64"


def _archive(assets: Path, stem: str, name: str, payload: bytes, *, sha: str | None = None) -> None:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        info = tarfile.TarInfo(f"{stem}/{name}")
        info.size = len(payload)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(payload))
    data = buf.getvalue()
    (assets / f"{stem}.tar.gz").write_bytes(data)
    digest = sha if sha is not None else hashlib.sha256(data).hexdigest()
    (assets / f"{stem}.tar.gz.sha256").write_text(f"{digest}  {stem}.tar.gz\n")


TARGET = "x86_64-unknown-linux-gnu"


@pytest.fixture
def assets(tmp_path: Path) -> Path:
    d = tmp_path / "assets"
    d.mkdir()
    _archive(d, f"secantusdb-0.5.3-beta.7-{TARGET}", "secantusd-mdb", _elf("2.17", "2.31"))
    _archive(d, f"secantusd-pg-0.1.0-beta.2-{TARGET}", "secantusd-pg", _elf("2.35"))
    return d


def _build(assets: Path, out: Path) -> Path:
    return build_wheels.build_wheel(TARGET, assets, out, "0.5.3-beta.7", "0.1.0-beta.2")


def test_wheel_contents_tag_and_record(assets: Path, tmp_path: Path) -> None:
    wheel = _build(assets, tmp_path / "dist")
    # The tag is the newest glibc EITHER binary needs.
    assert wheel.name == "secantusdb-0.5.3b7-py3-none-manylinux_2_35_x86_64.whl"
    with zipfile.ZipFile(wheel) as zf:
        names = set(zf.namelist())
        dist_info = "secantusdb-0.5.3b7.dist-info"
        assert {
            "secantusdb/__init__.py",
            "secantusdb/_server.py",
            "secantusdb/_cli.py",
            "secantusdb/bin/secantusd-mdb",
            "secantusdb/bin/secantusd-pg",
            f"{dist_info}/METADATA",
            f"{dist_info}/WHEEL",
            f"{dist_info}/RECORD",
            f"{dist_info}/entry_points.txt",
            f"{dist_info}/licenses/LICENSE",
            f"{dist_info}/licenses/LICENSE-ICU",
        } <= names
        # No deprecated Python server goes in the wheel.
        assert not any(n.startswith("secantus/") for n in names)
        for binary in ("secantusd-mdb", "secantusd-pg"):
            mode = zf.getinfo(f"secantusdb/bin/{binary}").external_attr >> 16
            assert mode & 0o111, f"{binary} is not executable"
        metadata = zf.read(f"{dist_info}/METADATA").decode()
        assert "Name: SecantusDB\nVersion: 0.5.3b7\n" in metadata
        assert "| 0.5.3-beta.7 |" in metadata and "| 0.1.0-beta.2 |" in metadata
        assert "Tag: py3-none-manylinux_2_35_x86_64" in zf.read(f"{dist_info}/WHEEL").decode()
        entry_points = zf.read(f"{dist_info}/entry_points.txt").decode()
        assert "secantusd-mdb = secantusdb._cli:mongodb" in entry_points
        assert "secantusd-pg = secantusdb._cli:postgres" in entry_points
        record = zf.read(f"{dist_info}/RECORD").decode().splitlines()
        listed = {}
        for line in record:
            path, digest, size = line.rsplit(",", 2)
            listed[path] = (digest, size)
        assert set(listed) == names
        for path, (digest, size) in listed.items():
            if path.endswith("/RECORD"):
                assert digest == size == ""
                continue
            data = zf.read(path)
            want = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            assert digest == f"sha256={want}" and int(size) == len(data), path


def test_an_archive_with_the_old_binary_name_still_builds(assets: Path, tmp_path: Path) -> None:
    # Releases up to 0.5.3-beta.196 shipped the MongoDB binary as secantusd-rs.
    _archive(assets, f"secantusdb-0.5.3-beta.7-{TARGET}", "secantusd-rs", _elf("2.17"))
    with zipfile.ZipFile(_build(assets, tmp_path / "dist")) as zf:
        assert "secantusdb/bin/secantusd-mdb" in zf.namelist()


def test_a_checksum_mismatch_is_refused(assets: Path, tmp_path: Path) -> None:
    _archive(
        assets, f"secantusd-pg-0.1.0-beta.2-{TARGET}", "secantusd-pg", _elf("2.35"), sha="0" * 64
    )
    with pytest.raises(ValueError, match="sha256"):
        _build(assets, tmp_path / "dist")


def test_an_archive_without_a_checksum_is_refused(assets: Path, tmp_path: Path) -> None:
    (assets / f"secantusdb-0.5.3-beta.7-{TARGET}.tar.gz.sha256").unlink()
    with pytest.raises(FileNotFoundError, match="unverified"):
        _build(assets, tmp_path / "dist")


def _fake_binary(bin_dir: Path, name: str, body: str) -> None:
    bin_dir.mkdir(exist_ok=True)
    path = bin_dir / name
    path.write_text(f"#!{sys.executable}\nimport sys, time\n{body}\n")
    path.chmod(0o755)


@pytest.mark.skipif(sys.platform == "win32", reason="the wheel ships no Windows binaries")
def test_launcher_reads_the_bound_port_and_cleans_up(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    bin_dir = tmp_path / "bin"
    # Prints a line of noise, the banner, then floods output: the launcher must
    # keep draining the pipe after the banner or the child blocks.
    _fake_binary(
        bin_dir,
        "secantusd-mdb",
        'print("starting", flush=True)\n'
        'print("secantusd-mdb listening on 127.0.0.1:41234", flush=True)\n'
        'print("x" * 200000, flush=True)\n'
        "time.sleep(60)",
    )
    monkeypatch.setattr(launcher, "_BIN_DIR", bin_dir)
    server = launcher.MongoServer(startup_timeout=20).start()
    try:
        assert (server.host, server.port) == ("127.0.0.1", 41234)
        assert server.uri == "mongodb://127.0.0.1:41234/"
        storage = server.storage_path
        assert storage is not None and storage.is_dir()
    finally:
        server.stop()
    assert not storage.exists()


@pytest.mark.skipif(sys.platform == "win32", reason="the wheel ships no Windows binaries")
def test_launcher_reports_a_server_that_exits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    bin_dir = tmp_path / "bin"
    _fake_binary(bin_dir, "secantusd-pg", 'print("cannot open storage", flush=True)\nsys.exit(3)')
    monkeypatch.setattr(launcher, "_BIN_DIR", bin_dir)
    kept = tmp_path / "data"
    with pytest.raises(launcher.ServerStartError, match="cannot open storage"):
        launcher.PostgresServer(kept, startup_timeout=20).start()
    # A storage path the caller named is theirs: never removed.
    assert kept.is_dir()


def test_binary_path_refuses_an_unknown_name() -> None:
    with pytest.raises(ValueError):
        launcher.binary_path("mongod")
