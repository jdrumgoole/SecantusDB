"""Batch 51 of the Rust PG server (2026-10-23): the regression tests of
RP-169's text-search dictionaries (`CREATE / ALTER / DROP TEXT SEARCH
DICTIONARY` over the `simple`, `snowball` and `synonym` templates, and
`ts_lexize` and the configurations lexing through them).

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch51.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario runs in a
transaction it rolls back (an error inside one rolls back to a savepoint).
An error whose message names PostgreSQL's installation directory (a
dictionary file it cannot open) is compared by its SQLSTATE alone, which a
DO block raises as a notice.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario


def _sqlstate_of(sql: str) -> str:
    """`~` statement: the SQLSTATE `sql` fails with, as a notice."""
    return "~do $$ begin " + sql + "; exception when others then raise notice '%', sqlstate; end $$"


def _guarded(sql: str) -> list[str]:
    """`sql`, which fails, inside a savepoint the scenario goes on after."""
    return ["savepoint e", sql, "rollback to savepoint e"]


SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a simple, a snowball and a synonym dictionary": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_d "
            "(template = simple, StopWords = English, ACCEPT = true)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "alter text search dictionary rp51_d (accept = false, stopwords = french)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "select ts_lexize('rp51_d', 'Le'), ts_lexize('rp51_d', 'Chat'), "
            "ts_lexize('rp51_d', 'The')",
            "alter text search dictionary rp51_d (accept)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "create text search dictionary rp51_sn "
            "(template = snowball, language = english, stopwords = english)",
            "select ts_lexize('rp51_sn', 'Running'), ts_lexize('rp51_sn', 'the')",
            "create text search dictionary rp51_sy (template = synonym, synonyms = synonym_sample)",
            "select ts_lexize('rp51_sy', 'Postgres'), ts_lexize('rp51_sy', 'indices'), "
            "ts_lexize('rp51_sy', 'x')",
            "select dictname, dicttemplate::int, dictinitoption from pg_ts_dict "
            "where dictname like 'rp51_%' order by 1",
            "rollback",
        ],
    ),
    "a configuration lexes through user dictionaries in order": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_sy (template = synonym, synonyms = synonym_sample)",
            "create text search dictionary rp51_st (template = simple, stopwords = english, "
            "accept = false)",
            "create text search dictionary rp51_sn (template = snowball, language = english)",
            "create text search configuration rp51_c (copy = simple)",
            "alter text search configuration rp51_c alter mapping for asciiword "
            "with rp51_sy, rp51_st, rp51_sn",
            "select to_tsvector('rp51_c', 'The cats running Postgres')",
            "select alias, dictionaries::text, dictionary::text, lexemes::text "
            "from ts_debug('rp51_c', 'The Postgres cats') where alias = 'asciiword'",
            "~drop text search dictionary rp51_sn cascade",
            "select count(*) from pg_ts_config where cfgname = 'rp51_c'",
            "rollback",
        ],
    ),
    "a dictionary's errors": (
        "RP-169",
        False,
        [
            "begin",
            *_guarded("create text search dictionary rp51_e (template = snowball)"),
            *_guarded(
                "create text search dictionary rp51_e (template = snowball, language = klingon)"
            ),
            *_guarded("create text search dictionary rp51_e (template = simple, foo = 1)"),
            *_guarded("create text search dictionary rp51_e (template = simple, accept = maybe)"),
            *_guarded("create text search dictionary rp51_e (template = nosuch)"),
            *_guarded("create text search dictionary rp51_e (stopwords = english)"),
            *_guarded(
                "create text search dictionary rp51_e (template = simple, stopwords = 'English')"
            ),
            *_guarded(
                "create text search dictionary rp51_e (template = snowball, language = english, "
                "foo = 1)"
            ),
            _sqlstate_of(
                "create text search dictionary rp51_e (template = simple, stopwords = nosuch)"
            ),
            _sqlstate_of(
                "create text search dictionary rp51_e (template = synonym, synonyms = nosuch)"
            ),
            "create text search dictionary rp51_q (template = simple)",
            *_guarded("alter text search dictionary rp51_q (template = snowball)"),
            *_guarded("create text search dictionary rp51_q (template = simple)"),
            *_guarded("alter text search dictionary nosuch (accept = true)"),
            "~drop text search dictionary if exists nosuch",
            *_guarded("drop text search dictionary simple"),
            "!savepoint e",
            "!drop text search dictionary english_stem",
            "rollback to savepoint e",
            "rollback",
        ],
    ),
    "rename, owner, schema and comment": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_r (template = pg_catalog.simple)",
            "alter text search dictionary rp51_r rename to rp51_r2",
            "alter text search dictionary rp51_r2 owner to postgres",
            "comment on text search dictionary rp51_r2 is 'a dictionary'",
            "select obj_description('rp51_r2'::regdictionary, 'pg_ts_dict')",
            "select 'rp51_r2'::regdictionary::text",
            "create schema rp51_s",
            "alter text search dictionary rp51_r2 set schema rp51_s",
            "select 'rp51_s.rp51_r2'::regdictionary::text, dictnamespace::regnamespace::text "
            "from pg_ts_dict where dictname = 'rp51_r2'",
            "drop text search dictionary rp51_s.rp51_r2",
            "select count(*) from pg_ts_dict where dictname like 'rp51_r%'",
            "rollback",
        ],
    ),
    "a built-in dictionary changed": (
        "RP-169",
        False,
        [
            "begin",
            "alter text search dictionary english_stem (stopwords = french)",
            "select dictinitoption from pg_ts_dict where dictname = 'english_stem'",
            "select ts_lexize('english_stem', 'le'), ts_lexize('english_stem', 'the')",
            "select to_tsvector('english', 'le chat and the cats')",
            "rollback",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a built-in dictionary changed": [
        "BEGIN",
        "ALTER TEXT SEARCH DICTIONARY",
        [("language = 'english', stopwords = 'french'",)],
        [("[]", "['the']")],
        [("'and':3 'cat':5 'chat':2 'the':4",)],
        "ROLLBACK",
    ],
    "a configuration lexes through user dictionaries in order": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'cat':2 'pgsql':4 'run':3",)],
        [
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_st", "{}"),
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_sy", "{pgsql}"),
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_sn", "{cat}"),
        ],
        ("DROP TEXT SEARCH DICTIONARY", ["drop cascades to text search configuration rp51_c"]),
        [("0",)],
        "ROLLBACK",
    ],
    "a dictionary's errors": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "22023", "missing Language parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42704",
            'no Snowball stemmer available for language "klingon" and encoding "UTF8"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized simple dictionary parameter: "foo"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "accept requires a Boolean value", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'text search template "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "text search template is required", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'invalid text search configuration file name "English"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized Snowball parameter: "foo"', None),
        "ROLLBACK",
        ("DO", ["F0000"]),
        ("DO", ["F0000"]),
        "CREATE TEXT SEARCH DICTIONARY",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized simple dictionary parameter: "template"', None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "pg_ts_dict_dictname_index"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'text search dictionary "nosuch" does not exist', None),
        "ROLLBACK",
        (
            "DROP TEXT SEARCH DICTIONARY",
            ['text search dictionary "nosuch" does not exist, skipping'],
        ),
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop text search dictionary simple because it is required by "
            "the database system",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop text search dictionary english_stem because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "text search configuration english depends on text search dictionary "
            "english_stem\n"
            "text search configuration hindi depends on text search dictionary "
            "english_stem\n"
            "text search configuration russian depends on text search dictionary "
            "english_stem",
        ),
        "ROLLBACK",
        "ROLLBACK",
    ],
    "a simple, a snowball and a synonym dictionary": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        [("stopwords = 'english', accept = 'true'",)],
        "ALTER TEXT SEARCH DICTIONARY",
        [("accept = 'false', stopwords = 'french'",)],
        [("[]", "None", "None")],
        "ALTER TEXT SEARCH DICTIONARY",
        [("stopwords = 'french'",)],
        "CREATE TEXT SEARCH DICTIONARY",
        [("['run']", "[]")],
        "CREATE TEXT SEARCH DICTIONARY",
        [("['pgsql']", "['index']", "None")],
        [
            ("rp51_d", "3727", "stopwords = 'french'"),
            ("rp51_sn", "13622", "language = 'english', stopwords = 'english'"),
            ("rp51_sy", "3730", "synonyms = 'synonym_sample'"),
        ],
        "ROLLBACK",
    ],
    "rename, owner, schema and comment": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "ALTER TEXT SEARCH DICTIONARY",
        "ALTER TEXT SEARCH DICTIONARY",
        "COMMENT",
        [("a dictionary",)],
        [("rp51_r2",)],
        "CREATE SCHEMA",
        "ALTER TEXT SEARCH DICTIONARY",
        [("rp51_s.rp51_r2", "rp51_s")],
        "DROP TEXT SEARCH DICTIONARY",
        [("0",)],
        "ROLLBACK",
    ],
}


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
