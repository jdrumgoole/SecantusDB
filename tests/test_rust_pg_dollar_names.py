"""The Rust PostgreSQL server keeps keys and values that look like MongoDB
extended JSON exactly as they were written.

A composite PRIMARY KEY is stored as a sub-document of the row's `_id`, keyed
by its column names, and PostgreSQL allows a column named `"$date"` or
`"$regex"`. Two things went wrong with such a key (measured 2026-10-14
against PostgreSQL 15.19, 60 of 130 scenarios differing):

- rows were decoded through serde, whose BSON visitor reads
  `{"$date": ..}`, `{"$regex": .., "$options": ..}`, `{"$binary": ..}` and the
  rest as the value they stand for in extended JSON, so a SELECT answered NULL
  keys, or every read of the table failed;
- a row was found again by `{_id: <its key>}`, and a key document with `$`
  names in that position is read as query operators, so an UPDATE changed
  nothing (and said it had) or failed.

A foreign key over such columns failed the same way, through the filters its
checks build by column name.

`json` / `jsonb` values are stored as text and were never affected; the last
test pins that.

Skipped unless `secantusd-pg` has been built; CI's `pg-oracle` lane builds it
and names this file.
"""

from __future__ import annotations

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

# (key columns, a key value) -- each pair of names is one extended-JSON form.
KEYS = {
    "k_regex": ('"$regex" text, "$options" text', ("a", "i")),
    "k_binary": ('"$binary" text, "$type" text', ("AA==", "00")),
    "k_ref": ('"$ref" text, "$id" int', ("coll", 5)),
    "k_date": ('"$date" text, "$x" text', ("2020-01-01T00:00:00Z", "y")),
}


@pytest.fixture
def connection(tmp_path: Path) -> Iterator[psycopg.Connection]:
    """A connection to a `secantusd-pg` over a store of its own."""
    proc = subprocess.Popen(
        # It connects as `probe`, a role nobody creates.
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


def _names(cols: str) -> list[str]:
    return [part.strip().split(" ")[0] for part in cols.split(",")]


@pytest.mark.parametrize("table", sorted(KEYS))
def test_a_composite_key_named_like_extended_json(
    connection: psycopg.Connection, table: str
) -> None:
    cols, key = KEYS[table]
    names = ", ".join(_names(cols))
    first = _names(cols)[0]
    c = connection
    c.execute(f"create table {table} ({cols}, n int, primary key ({names}))")
    c.execute(f"insert into {table} values (%s, %s, 0)", key)
    assert c.execute(f"select * from {table}").fetchall() == [(*key, 0)]

    # An UPDATE finds the row again by its key.
    assert c.execute(f"update {table} set n = n + 1").rowcount == 1
    assert c.execute(f"select * from {table} where n = 1").fetchall() == [(*key, 1)]

    # The key is unique, and ON CONFLICT finds the row it conflicts with.
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute(f"insert into {table} values (%s, %s, 9)", key)
    c.execute(
        f"insert into {table} values (%s, %s, 0) on conflict ({names}) "
        f"do update set n = {table}.n + 10",
        key,
    )
    assert c.execute(f"select * from {table}").fetchall() == [(*key, 11)]

    # A foreign key over the same names, cascading a change of key.
    c.execute(
        f"create table {table}_child (k int primary key, {cols}, foreign key ({names}) "
        f"references {table} ({names}) on update cascade on delete cascade)"
    )
    c.execute(f"insert into {table}_child values (1, %s, %s)", key)
    c.execute(f"update {table} set {first} = {first} || 'q'")
    moved = (f"{key[0]}q", key[1])
    assert c.execute(f"select * from {table}").fetchall() == [(*moved, 11)]
    assert c.execute(f"select * from {table}_child").fetchall() == [(1, *moved)]

    # DELETE finds it, and the cascade removes the child.
    assert c.execute(f"delete from {table} where n > 0").rowcount == 1
    assert c.execute(f"select count(*) from {table}").fetchone() == (0,)
    assert c.execute(f"select count(*) from {table}_child").fetchone() == (0,)


EXTENDED_JSON = [
    '{"$numberLong": "1"}',
    '{"$oid": "507f1f77bcf86cd799439011"}',
    '{"$date": {"$numberLong": "0"}}',
    '{"$binary": {"base64": "AA==", "subType": "00"}}',
    '{"$regularExpression": {"pattern": "a", "options": ""}}',
    '{"a": {"$numberLong": "1"}, "b": 2}',
    '[{"$oid": "507f1f77bcf86cd799439011"}, {"$numberLong": "7"}]',
]


def test_json_shaped_like_extended_json_survives_an_update(
    connection: psycopg.Connection,
) -> None:
    c = connection
    c.execute("create table j (id int primary key, j jsonb, js json, n int)")
    for i, v in enumerate(EXTENDED_JSON):
        c.execute("insert into j values (%s, %s::jsonb, %s::json, 0)", (i, v, v))
    c.execute("update j set n = n + 1")
    rows = c.execute("select j::text, js::text, jsonb_typeof(j) from j order by id").fetchall()
    # PostgreSQL 15.19's answers: `json` keeps the text, `jsonb` orders an
    # object's keys (shorter first), which moves only `options`.
    as_jsonb = [
        v.replace('"pattern": "a", "options": ""', '"options": "", "pattern": "a"')
        for v in EXTENDED_JSON
    ]
    assert [r[0] for r in rows] == as_jsonb
    assert [r[1] for r in rows] == EXTENDED_JSON
    assert [r[2] for r in rows] == ["object"] * 6 + ["array"]
