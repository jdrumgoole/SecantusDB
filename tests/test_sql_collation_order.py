"""``ORDER BY … COLLATE "<name>"``.

**The backlog entry that prompted this was wrong about the reference server.**
It said "PostgreSQL sorts text using the database collation (this box:
`en_US.UTF-8`)" and recorded SecantusDB's byte order as a defect. This box's
PostgreSQL is initialised with **`C`** (`datcollate = 'C'`), and under `C`
PostgreSQL sorts by bytes — byte-identical to SecantusDB. The entry compared
PostgreSQL *with an explicit `COLLATE` clause* against SecantusDB's default and
called the difference a bug.

So the default ordering was never wrong, and is deliberately left alone here:
changing it would BREAK the match with a `C`-collation database. What was
actually broken:

* `ORDER BY … COLLATE "en_US.UTF-8"` was accepted and then **silently ignored**
  — the user asked for a locale order and got bytes.
* an unknown collation was accepted too, where PostgreSQL raises `42704`.
* `SHOW lc_collate` returned `''`, which is not a collation name.
* `pg_collation` was present-but-empty, so a client enumerating collations was
  told there are none.

The locale key is `collation.sort_levels` — a three-level ICU-shaped key
computed **without ICU**. It matches PostgreSQL's `en_US.UTF-8` on 9 of 11
measured corpora; the two misses are documented in
`TestKnownNonIcuLimits` rather than pretended away.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

_WORDS = ["a b", "a-b", "ab", "abc", "aBc", "Abc", "ABC", "zzz", "ZZZ"]


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return [r.rows for r in run_sql(storage, "t", sql, session=session)][0]

    run("CREATE TABLE collt (w text)")
    for w in _WORDS:
        run(f"INSERT INTO collt VALUES ({w!r})")
    try:
        yield run
    finally:
        storage.close()


def _order(db, suffix=""):
    return [r[0] for r in db(f"SELECT w FROM collt ORDER BY w{suffix}")]


class TestDefaultIsByteOrder:
    """SecantusDB is a `C`-collation database and says so."""

    def test_default_matches_a_C_collation_postgres(self, db):
        assert _order(db) == ["ABC", "Abc", "ZZZ", "a b", "a-b", "aBc", "ab", "abc", "zzz"]

    @pytest.mark.parametrize("name", ["C", "POSIX", "ucs_basic", "default"])
    def test_explicit_bytewise_collations_agree(self, db, name):
        assert _order(db, f' COLLATE "{name}"') == _order(db)


class TestLocaleCollation:
    def test_en_us_ignores_case_and_punctuation_at_the_primary_level(self, db):
        assert _order(db, ' COLLATE "en_US.UTF-8"') == [
            "a b",
            "a-b",
            "ab",
            "abc",
            "aBc",
            "Abc",
            "ABC",
            "zzz",
            "ZZZ",
        ]

    def test_descending_reverses_it(self, db):
        asc = _order(db, ' COLLATE "en_US.UTF-8"')
        assert _order(db, ' COLLATE "en_US.UTF-8" DESC') == list(reversed(asc))

    def test_accents_sort_beside_their_base_letter(self, db):
        db("DELETE FROM collt")
        for w in ["a", "á", "ä", "az", "b"]:
            db(f"INSERT INTO collt VALUES ({w!r})")
        # Byte order puts every accented word after `z`; the locale key does not.
        assert _order(db, ' COLLATE "en_US.UTF-8"') == ["a", "á", "ä", "az", "b"]
        assert _order(db) == ["a", "az", "b", "á", "ä"]

    def test_unknown_collation_is_42704(self, db):
        with pytest.raises(SQLError) as ei:
            db('SELECT w FROM collt ORDER BY w COLLATE "nope_locale"')
        assert ei.value.sqlstate == "42704"
        assert str(ei.value) == 'collation "nope_locale" for encoding "UTF8" does not exist'


class TestKnownNonIcuLimits:
    """The two corpora where the non-ICU key differs from PostgreSQL.

    Pinned so the gap is visible and a future ICU-backed implementation has a
    target, rather than left to be rediscovered.
    """

    def test_eszett_is_not_expanded_to_ss(self, db):
        """PostgreSQL expands `ß` to `ss`, so `Straße` sorts AFTER `Strasse`."""
        db("DELETE FROM collt")
        for w in ["Straße", "Strasse", "Strase"]:
            db(f"INSERT INTO collt VALUES ({w!r})")
        assert _order(db, ' COLLATE "en_US.UTF-8"') == ["Strase", "Straße", "Strasse"]
        # PostgreSQL 14.13 answers ["Strase", "Strasse", "Straße"].

    def test_punctuation_weights_differ(self, db):
        """`-` versus `_` take different relative weights under CLDR."""
        db("DELETE FROM collt")
        for w in ["", " ", "-", "a", "_a", "a_"]:
            db(f"INSERT INTO collt VALUES ({w!r})")
        assert _order(db, ' COLLATE "en_US.UTF-8"') == ["", " ", "-", "_a", "a", "a_"]
        # PostgreSQL 14.13 answers ["", " ", "_a", "-", "a", "a_"].


class TestCollationIntrospection:
    def test_lc_collate_reports_C(self, db):
        assert db("SHOW lc_collate") == [("C",)]

    def test_pg_collation_lists_the_builtins(self, db):
        got = {
            r[0]
            for r in db(
                "SELECT collname FROM pg_collation "
                "WHERE collname IN ('C','POSIX','ucs_basic','default')"
            )
        }
        assert got == {"C", "POSIX", "ucs_basic", "default"}

    def test_pg_collation_lists_what_we_can_actually_serve(self, db):
        got = {r[0] for r in db("SELECT collname FROM pg_collation")}
        assert "en_US.UTF-8" in got
