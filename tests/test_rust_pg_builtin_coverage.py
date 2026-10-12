"""The Rust PostgreSQL server's built-in functions, held to PostgreSQL 15's
recorded answers.

`tools/probes/pg_corpora/builtin_coverage.sql` is the corpus for the
functions `tools/probes/pg_builtin_coverage.py` found refused or answered
wrongly: the `bytea` overloads of `substr` / `overlay` / `md5` / `trim`,
`regexp_match`, `sum(interval)`, `bit_xor`, `OVERLAPS`, a built-in called by
the name of the function behind its operator (`int4pl`), and the rest. Its
answers on PostgreSQL 15.19 are recorded beside it, in
`builtin_coverage.expected.json`, so this runs where there is no PostgreSQL
to ask.

To re-record after adding a line (it needs a PostgreSQL 15 on port 5415, or
`SECANTUS_PG_ORACLE_DSN_15`):

    python tools/probes/pg_differential.py \\
        tools/probes/pg_corpora/builtin_coverage.setup.sql \\
        tools/probes/pg_corpora/builtin_coverage.sql --types --rust \\
        --record=tools/probes/pg_corpora/builtin_coverage.expected.json

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
CORPORA = REPO / "tools" / "probes" / "pg_corpora"

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pg)",
)

if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))


@pytest.fixture
def connection(tmp_path: Path) -> Iterator[psycopg.Connection]:
    """A connection to a `secantusd-pg` over a store of its own."""
    proc = subprocess.Popen(
        [str(BINARY), str(tmp_path / "home"), "127.0.0.1:0"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        line = proc.stdout.readline() if proc.stdout else ""
        match = re.search(r"listening on \S+:(\d+)", line)
        assert match, f"secantusd-pg did not start: {line!r}"
        with psycopg.connect(
            f"host=127.0.0.1 port={match.group(1)} dbname=postgres user=probe",
            autocommit=True,
            connect_timeout=10,
        ) as conn:
            yield conn
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def _types(oids: list) -> list:
    """A user type's oid is each server's own; a built-in's is compared."""
    return ["user" if isinstance(o, int) and o >= 16384 else o for o in oids]


def test_the_corpus_and_its_record_are_the_same_statements() -> None:
    """A line added to the corpus and not recorded would be checked against
    nothing, and the test below would still pass."""
    from tools.probes import pg_differential

    recorded = json.loads((CORPORA / "builtin_coverage.expected.json").read_text())
    assert recorded["reference"].startswith("15."), recorded["reference"]
    corpus = pg_differential._read(str(CORPORA / "builtin_coverage.sql"))
    assert [check[0] for check in recorded["checks"]] == corpus
    assert len(corpus) >= 153


def test_builtin_functions_answer_as_postgresql_15_does(connection: psycopg.Connection) -> None:
    from tools.probes import pg_differential

    cur = connection.cursor()
    for statement in pg_differential._read(str(CORPORA / "builtin_coverage.setup.sql")):
        outcome = pg_differential._run(cur, statement, False)
        assert outcome[0] == "ok" or statement.upper().startswith("DROP"), (statement, outcome)

    recorded = json.loads((CORPORA / "builtin_coverage.expected.json").read_text())
    wrong: list[str] = []
    for sql, _label, kind, payload, oids in recorded["checks"]:
        got = pg_differential._run(cur, sql, True)
        # Through JSON, as the record went: tuples become lists.
        got_payload = json.loads(json.dumps(got[1]))
        if (got[0], got_payload, _types(got[3])) != (kind, payload, _types(oids)):
            wrong.append(
                f"{sql}\n   PostgreSQL: {kind} {payload} {oids}\n   secantusd-pg: {got[0]} "
                f"{got_payload} {got[3]}"
            )
    assert not wrong, f"{len(wrong)} of {len(recorded['checks'])} differ:\n" + "\n".join(wrong)
