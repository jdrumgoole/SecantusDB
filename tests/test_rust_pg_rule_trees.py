"""The Rust PostgreSQL server's rule trees (`pg_rewrite.ev_qual` /
`ev_action`, RP-203), held to PostgreSQL 15.19's.

`tools/probes/pg_rule_trees.py` compares the trees with a live PostgreSQL:
the 142 system rules `initdb` leaves byte for byte, and the rules its
scenarios create (views, `CREATE RULE`) after replacing user-object oids
with names. Its `--record` run writes the sha256 of each tree PostgreSQL
holds to `tools/probes/pg_rule_trees.expected.json`, so this runs where
there is no PostgreSQL to ask.

To re-record after changing a scenario (it needs a PRIVATE PostgreSQL 15.19
cluster, since the scenarios create objects inside a rolled-back
transaction):

    python tools/probes/pg_rule_trees.py --record \\
        --reference "host=127.0.0.1 port=<port> user=postgres dbname=postgres"

Skipped unless `secantusd-pg` has been built; CI's `pg-oracle` lane builds it
and names this file.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")

REPO = Path(__file__).resolve().parents[1]
BINARY = (
    REPO
    / "crates"
    / "secantus-pg"
    / "target"
    / "debug"
    / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")
)

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pg)",
)

if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))


@pytest.fixture
def dsn(tmp_path: Path) -> Iterator[str]:
    """The DSN of a `secantusd-pg` over a store of its own."""
    proc = subprocess.Popen(
        [str(BINARY), str(tmp_path / "home"), "127.0.0.1:0", "--trust-unknown-users"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        line = proc.stdout.readline() if proc.stdout else ""
        match = re.search(r"listening on \S+:(\d+)", line)
        assert match, f"secantusd-pg did not start: {line!r}"
        yield f"host=127.0.0.1 port={match.group(1)} dbname=postgres user=postgres"
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def _expected() -> dict:
    from tools.probes import pg_rule_trees

    return json.loads(pg_rule_trees.EXPECTED.read_text())


def test_the_record_is_postgresql_15_and_covers_every_scenario() -> None:
    """A scenario added to the probe and not recorded would be checked
    against nothing, and the test below would still pass."""
    from tools.probes import pg_rule_trees

    recorded = _expected()
    assert recorded["postgresql"].startswith("15.19"), recorded["postgresql"]
    assert len(recorded["system"]) == 142
    assert all(row[3] is not None or row[4] is not None for row in recorded["system"])
    assert sorted(recorded["scenarios"]) == sorted(pg_rule_trees.SCENARIOS)


def test_system_rules_are_postgresql_15s_byte_for_byte(dsn: str) -> None:
    from tools.probes import pg_rule_trees

    with psycopg.connect(dsn, autocommit=True, connect_timeout=10) as conn:
        rows = conn.execute(pg_rule_trees.SYSTEM_SQL).fetchall()
    got = pg_rule_trees.system_digests(rows)
    want = _expected()["system"]
    wrong = [w[:3] for w in want if w not in got]
    assert not wrong, f"{len(wrong)} of {len(want)} system rules differ: {wrong[:10]}"
    assert len(got) == len(want)


def test_user_rules_are_postgresql_15s(dsn: str) -> None:
    from tools.probes import pg_rule_trees

    want = _expected()["scenarios"]
    wrong = {}
    for name, stmts in pg_rule_trees.SCENARIOS.items():
        rows, err = pg_rule_trees.run_scenario(dsn, stmts)
        assert err is None, (name, err)
        got = pg_rule_trees.scenario_digests(rows)
        if got != want[name]:
            wrong[name] = got
    assert not wrong, f"{len(wrong)} of {len(want)} scenarios differ: {sorted(wrong)}"
