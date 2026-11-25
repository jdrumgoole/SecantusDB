"""The Rust PostgreSQL server's login rules, compared with PostgreSQL 15.

Each test runs one probe from ``tools/probes/``. A probe creates a throwaway
PostgreSQL cluster, puts the same logins to it and to ``secantusd-pg``, and
prints ``N of M the same``. The test requires N == M, and M to be the number
of scenarios the probe is known to hold, so a probe that stopped early cannot
pass on the scenarios it did reach.

These need a PostgreSQL 15 ``bin`` directory (``initdb``, ``pg_ctl``), which a
connection to a reference server cannot stand in for: the probes write their
own ``pg_hba.conf``, certificates and revocation lists. Name it in
``SECANTUS_PG15_BIN``. Without it the tests skip, because each one starts a
PostgreSQL cluster on a fixed port for a few minutes and that is not
something to do to a developer's machine unasked::

    SECANTUS_PG15_BIN=/opt/homebrew/opt/postgresql@15/bin \\
        uv run python -m pytest -n0 tests/test_rust_pg_login_differential.py

CI's ``pg-oracle`` lane installs PostgreSQL 15 and sets the variable, and it
sets ``SECANTUS_REQUIRE_PG15=1`` so that a missing directory there is a
failure and never a skip.

The TLS probe also needs the reference to link an OpenSSL from 2026-08-25 or
later on its line (``OPENSSL_DELTA_FIX``): an older one answers six of its
scenarios differently, and the test refuses it by name rather than report
them as divergences.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

import pytest

pytest.importorskip("psycopg")
pytest.importorskip("trustme")

REPO = Path(__file__).resolve().parent.parent
BINARY = REPO / "crates" / "secantus-pg" / "target" / "debug" / "secantusd-pg"
PROBES = REPO / "tools" / "probes"

#: The probes were measured against this major, and it is the one the server
#: reports itself as. Another major may word an answer differently.
REFERENCE_MAJOR = 15

#: probe -> (arguments after the binary, scenarios it holds)
CASES: dict[str, tuple[tuple[str, ...], int]] = {
    "pg_login_options.py": ((), 234),
    "pg_login_tls_files.py": ((), 547),
    "pg_login_hba.py": ((), 134),
}

#: PostgreSQL leaves revocation checking to the OpenSSL it links, so the TLS
#: probe's answers are that OpenSSL's. Until the 2026-08-25 releases,
#: OpenSSL's ``get_crl_score`` reached its "don't process deltas" check only
#: as the ``else`` arm of the extended-CRL-support test, which PostgreSQL
#: never turns on, so a delta list was scored as a full one. Each line's
#: release of that day hoisted the check, and a delta is now never a
#: certificate's list. ``secantusd-pg`` answers as the fixed OpenSSL does
#: (RP-747), and against an older one the probe's six ``crl scope delta``
#: scenarios differ. Measured on PostgreSQL 15.20: 3.6.3 differs, 3.0.22,
#: 3.5.9, 3.6.5 and 4.1.0 agree; the fix's first release on each line was
#: read from ``crypto/x509/x509_vfy.c`` at each tag (2026-11-25).
#: (major, minor) -> first patch release with the fix.
OPENSSL_DELTA_FIX: dict[tuple[int, int], int] = {
    (3, 0): 22,
    (3, 4): 7,
    (3, 5): 8,
    (3, 6): 4,
    (4, 0): 2,
}
#: The newest line the table knows. A later line was branched after the fix.
OPENSSL_NEWEST_KNOWN = (4, 0)

#: Prints the version of a libcrypto. Run in a child, so that loading it does
#: not mix two OpenSSLs in this process.
_ASK_OPENSSL = (
    "import ctypes, sys\n"
    "lib = ctypes.CDLL(sys.argv[1])\n"
    "lib.OpenSSL_version.restype = ctypes.c_char_p\n"
    "lib.OpenSSL_version.argtypes = [ctypes.c_int]\n"
    "print(lib.OpenSSL_version(0).decode())\n"
)


def _linked_libcrypto(postgres: Path) -> Path | None:
    """The libcrypto ``postgres`` loads, from the dynamic linker's view."""
    if sys.platform == "darwin":
        listing = subprocess.run(
            ["otool", "-L", str(postgres)], capture_output=True, text=True, check=True
        ).stdout
        found = re.search(r"^\s*(\S*libcrypto\S*\.dylib)", listing, re.MULTILINE)
    else:
        listing = subprocess.run(
            ["ldd", str(postgres)], capture_output=True, text=True, check=True
        ).stdout
        found = re.search(r"libcrypto\S*\s+=>\s+(\S+)", listing)
    return Path(found.group(1)) if found else None


def _reference_openssl(postgres: Path) -> str:
    """The version text of the OpenSSL ``postgres`` links, as that library
    reports it (``OpenSSL 3.5.9 29 Sep 2026``)."""
    lib = _linked_libcrypto(postgres)
    assert lib is not None, (
        f"{postgres}: links no libcrypto the dynamic linker names; the TLS "
        "probe's answers are its OpenSSL's, so the reference must link one"
    )
    return subprocess.run(
        [sys.executable, "-c", _ASK_OPENSSL, str(lib)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def openssl_scores_deltas_as_full_lists(version: str) -> bool:
    """Whether the OpenSSL named by ``version`` predates the delta fix (see
    ``OPENSSL_DELTA_FIX``). A version this cannot read counts as old."""
    parsed = re.search(r"OpenSSL (\d+)\.(\d+)\.(\d+)", version)
    if not parsed:
        return True
    major, minor, patch = (int(part) for part in parsed.groups())
    first_fixed = OPENSSL_DELTA_FIX.get((major, minor))
    if first_fixed is not None:
        return patch < first_fixed
    return (major, minor) < OPENSSL_NEWEST_KNOWN


def _pg_bin() -> Path:
    """The PostgreSQL 15 ``bin`` directory, or a skip (a failure where the
    environment says it must be there)."""
    required = os.environ.get("SECANTUS_REQUIRE_PG15") == "1"
    named = os.environ.get("SECANTUS_PG15_BIN")
    if not named:
        if required:
            pytest.fail("SECANTUS_REQUIRE_PG15=1 and SECANTUS_PG15_BIN is not set")
        pytest.skip("set SECANTUS_PG15_BIN to a PostgreSQL 15 bin directory to run this")
    bin_dir = Path(named)
    postgres = bin_dir / "postgres"
    # A directory that was named and is wrong is an error either way: the
    # caller asked for this to run.
    assert postgres.exists(), f"SECANTUS_PG15_BIN={named}: no postgres binary there"
    version = subprocess.run(
        [str(postgres), "--version"], capture_output=True, text=True, check=True
    ).stdout
    major = re.search(r"\b(\d+)\.\d+", version)
    assert major and int(major.group(1)) == REFERENCE_MAJOR, (
        f"SECANTUS_PG15_BIN={named} is {version.strip()!r}, not PostgreSQL {REFERENCE_MAJOR}"
    )
    return bin_dir


@pytest.mark.skipif(sys.platform == "win32", reason="the probes drive pg_ctl and SIGHUP")
# A probe restarts PostgreSQL once per configuration: minutes on a slow runner,
# past the suite's per-test deadline.
@pytest.mark.timeout(1800)
@pytest.mark.parametrize("probe", sorted(CASES))
def test_logins_answer_as_postgresql_does(probe: str) -> None:
    pg_bin = _pg_bin()
    assert BINARY.exists(), f"{BINARY.relative_to(REPO)} is not built"
    if probe == "pg_login_tls_files.py":
        openssl = _reference_openssl(pg_bin / "postgres")
        # Not a skip: the caller asked for this comparison, and a reference
        # that cannot give it is an error to fix, never a pass.
        assert not openssl_scores_deltas_as_full_lists(openssl), (
            f"SECANTUS_PG15_BIN={pg_bin} links {openssl!r}, which predates "
            "OpenSSL's 2026-08-25 fix and reads a delta revocation list as a "
            "full one (RP-747; OPENSSL_DELTA_FIX in this file). Point "
            "SECANTUS_PG15_BIN at a PostgreSQL 15 linking OpenSSL 3.0.22, "
            "3.4.7, 3.5.8, 3.6.4, 4.0.2 or later on its line, as CI's "
            ".github/actions/pg15-current-openssl builds"
        )
    extra, scenarios = CASES[probe]
    done = subprocess.run(
        [sys.executable, str(PROBES / probe), str(pg_bin), str(BINARY), *extra],
        capture_output=True,
        text=True,
        timeout=1500,
        check=False,
    )
    output = done.stdout + done.stderr
    differences = "\n".join(
        line
        for line in output.splitlines()
        if not line.lstrip().startswith("SAME") and line.strip()
    )
    assert done.returncode == 0, f"{probe} did not finish:\n{output[-4000:]}"
    last = output.strip().splitlines()[-1]
    assert last == f"{scenarios} of {scenarios} the same", f"{probe}: {last}\n{differences[-6000:]}"


@pytest.mark.parametrize(
    ("version", "old"),
    [
        # CI's reference until RP-747: Ubuntu 24.04's PGDG postgresql-15.
        ("OpenSSL 3.0.13 30 Jan 2024", True),
        ("OpenSSL 3.0.21 9 Jun 2026", True),
        ("OpenSSL 3.0.22 25 Aug 2026", False),
        ("OpenSSL 3.3.7 7 Apr 2026", True),
        ("OpenSSL 3.5.7 9 Jun 2026", True),
        ("OpenSSL 3.5.9 29 Sep 2026", False),
        # The other Mac's Homebrew openssl@3 on 2026-11-25.
        ("OpenSSL 3.6.3 9 Jun 2026", True),
        ("OpenSSL 3.6.4 25 Aug 2026", False),
        ("OpenSSL 4.0.1 9 Jun 2026", True),
        ("OpenSSL 4.1.0 23 Oct 2026", False),
        ("OpenSSL 1.1.1w  11 Sep 2023", True),
        ("LibreSSL 3.3.6", True),
    ],
)
def test_a_reference_before_the_delta_fix_is_named(version: str, old: bool) -> None:
    """Which reference OpenSSLs read a delta list as a full one (RP-747): the
    TLS probe refuses those by name instead of reporting six divergences."""
    assert openssl_scores_deltas_as_full_lists(version) is old
