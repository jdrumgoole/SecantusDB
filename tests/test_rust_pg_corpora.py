"""The Rust PostgreSQL server against PostgreSQL's recorded answers to every
differential corpus.

`tools/probes/pg_corpora/` holds the corpora `pg_differential.py` runs
against `secantusd-pg` and a live PostgreSQL. Until 2026-10-21 that comparison
ran only on a developer's box: CI ran one corpus (`builtin_coverage`, which
has its own test), so a change that broke what any of the others pinned --
`pg_get_viewdef` text, pgcrypto, FDWs, event triggers, rules, jsonpath,
`to_char` -- passed CI. PostgreSQL's answers are now recorded beside the
corpora, in `pg_corpora/expected/`, and this test holds the server to them
where there is no PostgreSQL to ask.

Each corpus runs on a server of its own, so corpora cannot see each other's
objects (the reason the live sweep must run serially does not apply here).

A check whose answer PostgreSQL itself does not repeat -- the clock, the
process, or a difference between two recordings -- is marked volatile in its
record and compared on status and column types only. Every other check is
compared exactly. A divergence the server has today is listed in
`KNOWN_DIVERGENCES` with the backlog item that owns it, and the test fails
both on a divergence that is not listed and on a listed one that has gone, so
the list cannot go stale in either direction.

To re-record (both PostgreSQL 14 and 15 references; record on FRESH clusters,
twice -- see `tools/probes/pg_corpora_expect.py`)::

    python tools/probes/pg_corpora_sweep.py --record /tmp/rec1
    python tools/probes/pg_corpora_sweep.py --record /tmp/rec2   # new clusters
    python tools/probes/pg_corpora_expect.py /tmp/rec1 /tmp/rec2

Skipped unless `secantusd-pg` has been built; CI's `pg-oracle` lane builds it
and names this file.
"""

from __future__ import annotations

import json
import re
import shutil
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
EXPECTED = CORPORA / "expected"

if str(REPO) not in sys.path:
    sys.path.insert(0, str(REPO))

#: Corpora held to PostgreSQL elsewhere, by name, with where.
PINNED_ELSEWHERE = {
    "builtin_coverage": "tests/test_rust_pg_builtin_coverage.py, its own record",
}

#: {corpus: {"<sql> [<binding mode>]": "<backlog item>: what differs"}}.
#: Every entry is a divergence secantusd-pg has TODAY, owned by an open item in
#: tasks/backlog-rust-postgres.md. Fix one and its entry must go.
KNOWN_DIVERGENCES: dict[str, dict[str, str]] = {}


def _corpora() -> list[str]:
    return sorted(
        p.name[: -len(".sql")]
        for p in CORPORA.glob("*.sql")
        if not p.name.endswith(".setup.sql") and p.name[: -len(".sql")] not in PINNED_ELSEWHERE
    )


def _key(sql: str, label: str) -> str:
    return f"{sql} [{label}]" if label else sql


def _types(oids: list) -> list:
    """A user type's oid is each server's own; a built-in's is compared."""
    return ["user" if isinstance(o, int) and o >= 16384 else o for o in oids]


@pytest.fixture
def connection(tmp_path: Path) -> Iterator[psycopg.Connection]:
    """A connection to a `secantusd-pg` over a store of its own."""
    if not BINARY.exists():
        pytest.skip(f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pg)")
    proc = subprocess.Popen(
        # It connects as `probe`, a role nobody creates, as the probe does.
        [str(BINARY), str(tmp_path / "home"), "127.0.0.1:0", "--trust-unknown-users"],
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
            proc.wait()
    # WiredTiger preallocates a 128 MB log file per store, which Linux really
    # allocates; pytest keeps every tmp_path to the end of the session, so one
    # store per corpus filled a CI runner's disk.
    shutil.rmtree(tmp_path / "home")


def test_every_corpus_has_a_record() -> None:
    """A corpus with no record is checked by nothing here."""
    unrecorded = [name for name in _corpora() if not (EXPECTED / f"{name}.json").exists()]
    assert not unrecorded, "record these corpora (see this file's docstring): " + ", ".join(
        unrecorded
    )
    orphans = sorted({p.stem for p in EXPECTED.glob("*.json")} - set(_corpora()))
    assert not orphans, "records with no corpus: " + ", ".join(orphans)
    assert len(_corpora()) >= 300


def test_known_divergences_name_an_open_backlog_item() -> None:
    backlog = (REPO / "tasks" / "backlog-rust-postgres.md").read_text(encoding="utf-8")
    open_ids = set(re.findall(r"Backlog-ID: (RP-\d+)", backlog))
    for corpus, entries in KNOWN_DIVERGENCES.items():
        assert (CORPORA / f"{corpus}.sql").exists(), f"no corpus {corpus}"
        for key, why in entries.items():
            item = why.split(":", 1)[0]
            assert item in open_ids, f"{corpus}: {key!r} names {item}, not an open RP item"


# Twice the suite's 600 s deadline. `psql_describe` takes 268 s on a debug
# build (2026-10-21), 263 of them in one catalog query that RP-077 owns; a CI
# runner slower than that laptop would otherwise hit the deadline on a known
# cause. When RP-077 is fixed this can go.
@pytest.mark.timeout(1200)
@pytest.mark.parametrize("corpus", _corpora())
def test_corpus_answers_as_postgresql_does(corpus: str, connection: psycopg.Connection) -> None:
    from tools.probes import pg_differential as pd

    record_path = EXPECTED / f"{corpus}.json"
    if not record_path.exists():
        pytest.fail(f"{corpus} has no record; see test_every_corpus_has_a_record")
    record = json.loads(record_path.read_text(encoding="utf-8"))
    volatile = record["volatile"]
    checks = record["checks"]

    cur = connection.cursor()
    for statement in pd._read(str(CORPORA / f"{corpus}.setup.sql")):
        # As the probe does: a setup statement that fails is the server's
        # answer too, and shows up in the checks that depend on it.
        pd._run(cur, statement, False)

    runs: list[tuple[str, str, tuple]] = []
    for line in pd._read(str(CORPORA / f"{corpus}.sql")):
        sql, args = pd._split_params(line)
        if args is None:
            runs.append((sql, "", pd._run(cur, sql, True)))
        else:
            for label, prepare, binary in pd.PARAM_MODES:
                got = pd._run_params(cur, sql, args, True, prepare=prepare, binary=binary)
                runs.append((sql, label, got))
    assert [(s, label) for s, label, _ in runs] == [(c[0], c[1]) for c in checks], (
        f"{corpus}.sql and its record are not the same statements: re-record it"
    )

    known = KNOWN_DIVERGENCES.get(corpus, {})
    wrong: list[str] = []
    seen: set[str] = set()
    for i, ((sql, label, got), (_, _, kind, payload, oids)) in enumerate(
        zip(runs, checks, strict=True)
    ):
        # Through JSON, as the record went: tuples become lists.
        got_payload = json.loads(json.dumps(got[1]))
        if str(i) in volatile:
            # Status, and the error's SQLSTATE or the column types.
            want = (kind, payload.split(":")[0] if kind == "err" else _types(oids))
            have = (got[0], got_payload.split(":")[0] if got[0] == "err" else _types(got[3]))
        else:
            want = (kind, payload, _types(oids))
            have = (got[0], got_payload, _types(got[3]))
        key = _key(sql, label)
        if want == have:
            continue
        if key in known:
            seen.add(key)
            continue
        wrong.append(
            f"{key}\n   PostgreSQL: {kind} {payload} {oids}\n   secantusd-pg: {got[0]} "
            f"{got_payload} {got[3]}"
        )
    gone = sorted(set(known) - seen)
    assert not wrong, f"{len(wrong)} of {len(checks)} differ:\n" + "\n".join(wrong)
    assert not gone, (
        "these KNOWN_DIVERGENCES now match PostgreSQL; remove them (and close or update "
        "their backlog items):\n  " + "\n  ".join(gone)
    )
