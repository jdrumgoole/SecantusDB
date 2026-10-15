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
    "pg_login_options.py": (("--refuse-unknown-users",), 234),
    "pg_login_tls_files.py": ((), 513),
    "pg_login_hba.py": ((), 128),
}


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
