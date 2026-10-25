"""Batch 73 of the Rust PG server (2026-10-25): the `ispell` and
`thesaurus` text search templates (RP-169), and what they need of the rest
of text search -- several lexemes at one position, a multi-word dictionary
in a configuration (`LexizeExec`), `ts_headline` through both. Most
statements are PostgreSQL's own `tsdicts` regression test.

Found and fixed on the way: `dictinitoption` quoted an option written as a
number (RP-250), `ALTER ... MAPPING` refused a token type named twice
(RP-251), and a synonym's prefix output (`index*`) lost its `:*` in a
tsquery (RP-252).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch73 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario rolls back what it
made. A dictionary file that is not installed is left out: PostgreSQL's
message names the installation's share directory, which differs between
any two builds.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_WORDS = [
    "skies",
    "bookings",
    "booking",
    "foot",
    "foots",
    "rebookings",
    "rebooking",
    "rebook",
    "unbookings",
    "unbooking",
    "unbook",
    "booked",
    "sk",
    "footklubber",
    "footballklubber",
    "ballyklubber",
    "ballsklubber",
    "footballyklubber",
    "ex-machina",
]


def _lexize(dict_name: str) -> str:
    return "select " + ", ".join(f"ts_lexize('{dict_name}', '{w}')" for w in _WORDS)


_DICTS = {
    "ispell": ("ispell_sample", "ispell_sample"),
    "hunspell": ("ispell_sample", "hunspell_sample"),
    "hunspell_long": ("hunspell_sample_long", "hunspell_sample_long"),
    "hunspell_num": ("hunspell_sample_num", "hunspell_sample_num"),
}


def _create(name: str) -> str:
    dict_file, aff_file = _DICTS[name]
    return (
        f"create text search dictionary {name} "
        f"(template = ispell, dictfile = {dict_file}, afffile = {aff_file})"
    )


def _failing(statement: str) -> list[str]:
    return ["savepoint s", statement, "rollback to s"]


_DOC = "Booking the skies after rebookings for footballklubber from a foot"
_THESAURUS = (
    "create text search dictionary thesaurus "
    "(template = thesaurus, dictfile = thesaurus_sample, dictionary = english_stem)"
)
_SYNONYM = "create text search dictionary synonym (template = synonym, synonyms = synonym_sample)"
_SYNONYM_TST = [
    _SYNONYM,
    "create text search configuration synonym_tst (copy = english)",
    "alter text search configuration synonym_tst alter mapping for "
    "asciiword, hword_asciipart, asciihword with synonym, english_stem",
]

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "ispell dictionaries in ts_lexize": (
        "RP-169",
        False,
        [
            "begin",
            *[s for name in _DICTS for s in (_create(name), _lexize(name))],
            "rollback",
        ],
    ),
    "unsuitable ispell affix and dictionary files": (
        "RP-169",
        False,
        [
            "begin",
            *_failing(
                "create text search dictionary e1 "
                "(template = ispell, dictfile = ispell_sample, afffile = hunspell_sample_long)"
            ),
            *_failing(
                "create text search dictionary e2 "
                "(template = ispell, dictfile = ispell_sample, afffile = hunspell_sample_num)"
            ),
            "create text search dictionary i1 "
            "(template = ispell, dictfile = hunspell_sample_long, afffile = ispell_sample)",
            "create text search dictionary i2 "
            "(template = ispell, dictfile = hunspell_sample_long, afffile = hunspell_sample_num)",
            "create text search dictionary i3 "
            "(template = ispell, dictfile = hunspell_sample_num, afffile = ispell_sample)",
            *_failing(
                "create text search dictionary e3 "
                "(template = ispell, dictfile = hunspell_sample_num,"
                " afffile = hunspell_sample_long)"
            ),
            "select ts_lexize('i1', 'bookings'), ts_lexize('i2', 'skies'),"
            " ts_lexize('i3', 'foots')",
            "rollback",
        ],
    ),
    "ispell and thesaurus parameters": (
        "RP-169",
        False,
        [
            "begin",
            *_failing(
                "create text search dictionary p1 (template = ispell, dictfile = ispell_sample)"
            ),
            *_failing(
                "create text search dictionary p2 (template = ispell, afffile = ispell_sample)"
            ),
            *_failing(
                "create text search dictionary p3 (template = ispell, dictfile = ispell_sample,"
                " dictfile = ispell_sample, afffile = ispell_sample)"
            ),
            *_failing(
                "create text search dictionary p4 "
                "(template = ispell, dictfile = 'Bad-Name', afffile = ispell_sample)"
            ),
            *_failing(
                'create text search dictionary p5 (template = ispell, "DictFile" = ispell_sample,'
                ' "AffFile" = ispell_sample)'
            ),
            *_failing(
                "create text search dictionary p6"
                " (template = thesaurus, dictfile = thesaurus_sample)"
            ),
            *_failing(
                "create text search dictionary p7 (template = thesaurus, dictionary = english_stem)"
            ),
            *_failing(
                "create text search dictionary p8 "
                "(template = thesaurus, dictfile = thesaurus_sample, dictionary = nosuchdict)"
            ),
            *_failing(
                "create text search dictionary p9 "
                "(template = thesaurus, dictfile = thesaurus_sample, dictionary = simple)"
            ),
            *_failing(
                "create text search dictionary p10 (template = thesaurus,"
                " dictfile = thesaurus_sample, dictionary = english_stem, foo = 1)"
            ),
            _THESAURUS,
            *_failing(
                "create text search dictionary p11 "
                "(template = thesaurus, dictfile = thesaurus_sample, dictionary = thesaurus)"
            ),
            "create text search dictionary isp (template = ispell, dictfile = ispell_sample,"
            " afffile = ispell_sample, stopwords = english)",
            "select ts_lexize('isp', 'foot'), ts_lexize('isp', 'a'), ts_lexize('isp', ''),"
            " ts_lexize('isp', 'FOOTS'), ts_lexize('isp', 'zzz')",
            *_failing(
                "create text search dictionary p12 "
                "(template = thesaurus, dictfile = thesaurus_sample, dictionary = isp)"
            ),
            "select dictinitoption from pg_ts_dict where dictname in ('isp', 'thesaurus')"
            " order by dictname",
            "rollback",
        ],
    ),
    "a thesaurus in ts_lexize": (
        "RP-169",
        False,
        [
            "begin",
            _THESAURUS,
            "select ts_lexize('thesaurus', 'one'), ts_lexize('thesaurus', 'supernovae'),"
            " ts_lexize('thesaurus', 'booking'), ts_lexize('thesaurus', 'two'),"
            " ts_lexize('thesaurus', 'zzz')",
            "rollback",
        ],
    ),
    "ispell dictionaries in a configuration": (
        "RP-169",
        False,
        [
            "begin",
            *[_create(name) for name in _DICTS],
            "create text search configuration ispell_tst (copy = english)",
            "alter text search configuration ispell_tst alter mapping for word, numword, "
            "asciiword, hword, numhword, asciihword, hword_part, hword_numpart, hword_asciipart "
            "with ispell, english_stem",
            f"select to_tsvector('ispell_tst', '{_DOC}')",
            "select to_tsquery('ispell_tst', 'footballklubber'),"
            " to_tsquery('ispell_tst', 'footballyklubber:b & rebookings:A & sky')",
            "create text search configuration hunspell_tst (copy = ispell_tst)",
            "alter text search configuration hunspell_tst alter mapping"
            " replace ispell with hunspell",
            f"select to_tsvector('hunspell_tst', '{_DOC}')",
            "select to_tsquery('hunspell_tst', 'footballklubber'),"
            " to_tsquery('hunspell_tst', 'footballyklubber:b & rebookings:A & sky'),"
            " to_tsquery('hunspell_tst', 'footballyklubber:b <-> sky'),"
            " phraseto_tsquery('hunspell_tst', 'footballyklubber sky')",
            "alter text search configuration hunspell_tst alter mapping "
            "replace hunspell with hunspell_long",
            f"select to_tsvector('hunspell_tst', '{_DOC}')",
            "select to_tsquery('hunspell_tst', 'footballklubber'),"
            " to_tsquery('hunspell_tst', 'footballyklubber:b & rebookings:A & sky')",
            "alter text search configuration hunspell_tst alter mapping "
            "replace hunspell_long with hunspell_num",
            f"select to_tsvector('hunspell_tst', '{_DOC}')",
            "select to_tsquery('hunspell_tst', 'footballklubber'),"
            " to_tsquery('hunspell_tst', 'footballyklubber:b & rebookings:A & sky')",
            "select to_tsquery('ispell_tst', 'rebookings:*'),"
            " plainto_tsquery('ispell_tst', 'rebookings the footballklubber'),"
            " websearch_to_tsquery('ispell_tst', 'bookings or -footballklubber')",
            "select ts_headline('ispell_tst', 'The bookings for footballklubber are rebookings',"
            " to_tsquery('ispell_tst', 'book & klubber'))",
            "select alias, token, dictionary::text, lexemes"
            " from ts_debug('ispell_tst', 'bookings footballklubber the zzz')",
            "rollback",
        ],
    ),
    "a thesaurus in a configuration": (
        "RP-169",
        False,
        [
            "begin",
            *_SYNONYM_TST,
            _THESAURUS,
            "create text search configuration thesaurus_tst (copy = synonym_tst)",
            "alter text search configuration thesaurus_tst alter mapping for "
            "asciiword, hword_asciipart, asciihword with synonym, thesaurus, english_stem",
            "select to_tsvector('thesaurus_tst', 'one postgres one two one two three one')",
            "select to_tsvector('thesaurus_tst', 'Supernovae star is very new star and usually"
            " called supernovae (abbreviation SN)')",
            "select to_tsvector('thesaurus_tst', 'Booking tickets is looking like a booking a"
            " tickets')",
            "select to_tsquery('thesaurus_tst', 'supernovae & stars'),"
            " plainto_tsquery('thesaurus_tst', 'one two three'),"
            " phraseto_tsquery('thesaurus_tst', 'booking tickets now'),"
            " websearch_to_tsquery('thesaurus_tst', '\"one two\" or supernovae')",
            "select to_tsvector('thesaurus_tst', 'one two three four one two supernovae stars"
            " booking the tickets two')",
            "select ts_headline('thesaurus_tst', 'We like supernovae stars and one two three"
            " things', to_tsquery('thesaurus_tst', 'sn'))",
            "select ts_headline('thesaurus_tst', 'booking tickets is fun',"
            " to_tsquery('thesaurus_tst', 'order & card'))",
            "select alias, token, dictionary::text, lexemes"
            " from ts_debug('thesaurus_tst', 'one two supernovae stars')",
            "rollback",
        ],
    ),
    "a synonym's prefix output in a tsquery": (
        "RP-252",
        False,
        [
            "begin",
            *_SYNONYM_TST,
            "select to_tsvector('synonym_tst', 'Postgresql is often called as postgres or pgsql"
            " and pronounced as postgre')",
            "select to_tsvector('synonym_tst', 'Indexes or indices - Which is right plural form"
            " of index?')",
            "select to_tsquery('synonym_tst', 'Index & indices'),"
            " plainto_tsquery('synonym_tst', 'indices'),"
            " phraseto_tsquery('synonym_tst', 'big indices')",
            "rollback",
        ],
    ),
    "dictinitoption writes a number unquoted": (
        "RP-250",
        False,
        [
            "begin",
            _SYNONYM,
            "alter text search dictionary synonym (CaseSensitive = 1)",
            "select dictinitoption from pg_ts_dict where dictname = 'synonym'",
            *_failing("alter text search dictionary synonym (CaseSensitive = 2)"),
            "alter text search dictionary synonym (CaseSensitive = off)",
            "select dictinitoption from pg_ts_dict where dictname = 'synonym'",
            "create text search dictionary d2 (template = simple, accept = 0, stopwords = english)",
            "select dictinitoption from pg_ts_dict where dictname = 'd2'",
            "alter text search dictionary d2 (accept = 'false')",
            "select dictinitoption from pg_ts_dict where dictname = 'd2'",
            "rollback",
        ],
    ),
    "a token type named twice in ALTER MAPPING": (
        "RP-251",
        False,
        [
            "begin",
            "create text search configuration dummy_tst (copy = english)",
            "alter text search configuration dummy_tst alter mapping for word, word with simple",
            *_failing(
                "alter text search configuration dummy_tst"
                " drop mapping for not_a_token, not_a_token"
            ),
            *_failing(
                "alter text search configuration dummy_tst"
                " drop mapping if exists for not_a_token, not_a_token"
            ),
            "alter text search configuration dummy_tst drop mapping for word, word",
            *_failing("alter text search configuration dummy_tst drop mapping for word"),
            "~alter text search configuration dummy_tst drop mapping if exists for word, word",
            "alter text search configuration dummy_tst add mapping for word, word with simple",
            "select maptokentype, mapseqno, mapdict::regdictionary::text from pg_ts_config_map"
            " where mapcfg = 'dummy_tst'::regconfig and maptokentype = 2",
            "rollback",
        ],
    ),
}


def _scenario(name: str, server: Any) -> list[Any]:
    return run_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


EXPECTED: dict[str, Any] = {
    "ispell dictionaries in ts_lexize": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        [
            (
                "['sky']",
                "['booking', 'book']",
                "['booking', 'book']",
                "['foot']",
                "['foot']",
                "['booking', 'book']",
                "['booking', 'book']",
                "None",
                "['book']",
                "['book']",
                "['book']",
                "None",
                "None",
                "['foot', 'klubber']",
                "['footballklubber', 'foot', 'ball', 'klubber', 'football', 'klubber']",
                "['ball', 'klubber']",
                "None",
                "['foot', 'ball', 'klubber']",
                "None",
            )
        ],
        "CREATE TEXT SEARCH DICTIONARY",
        [
            (
                "['sky']",
                "['booking', 'book']",
                "['booking', 'book']",
                "['foot']",
                "['foot']",
                "['booking', 'book']",
                "['booking', 'book']",
                "None",
                "['book']",
                "['book']",
                "['book']",
                "None",
                "None",
                "['foot', 'klubber']",
                "['footballklubber', 'foot', 'ball', 'klubber', 'football', 'klubber']",
                "['ball', 'klubber']",
                "None",
                "['foot', 'ball', 'klubber']",
                "None",
            )
        ],
        "CREATE TEXT SEARCH DICTIONARY",
        [
            (
                "['sky']",
                "['booking', 'book']",
                "['booking', 'book']",
                "['foot']",
                "['foot']",
                "['booking', 'book']",
                "['booking', 'book']",
                "None",
                "['book']",
                "['book']",
                "['book']",
                "['book']",
                "None",
                "['foot', 'klubber']",
                "['footballklubber', 'foot', 'ball', 'klubber', 'football', 'klubber']",
                "['ball', 'klubber']",
                "['ball', 'klubber']",
                "['foot', 'ball', 'klubber']",
                "['ex-', 'machina']",
            )
        ],
        "CREATE TEXT SEARCH DICTIONARY",
        [
            (
                "['sky']",
                "['booking', 'book']",
                "['booking', 'book']",
                "['foot']",
                "['foot']",
                "['booking', 'book']",
                "['booking', 'book']",
                "None",
                "['book']",
                "['book']",
                "['book']",
                "['book']",
                "['sky']",
                "['foot', 'klubber']",
                "['footballklubber', 'foot', 'ball', 'klubber', 'football', 'klubber']",
                "['ball', 'klubber']",
                "None",
                "['foot', 'ball', 'klubber']",
                "None",
            )
        ],
        "ROLLBACK",
    ],
    "unsuitable ispell affix and dictionary files": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "F0000", 'invalid affix alias "GJUS"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "F0000", 'invalid affix flag "SZ\\"', None),
        "ROLLBACK",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "SAVEPOINT",
        ("ERR", "F0000", 'invalid affix alias "302,301,202,303"', None),
        "ROLLBACK",
        [("None", "None", "None")],
        "ROLLBACK",
    ],
    "ispell and thesaurus parameters": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "22023", "missing AffFile parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", "missing DictFile parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", "multiple DictFile parameters", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'invalid text search configuration file name "Bad-Name"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized Ispell parameter: "DictFile"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", "missing Dictionary parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", "missing DictFile parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'text search dictionary "nosuchdict" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        "CREATE TEXT SEARCH DICTIONARY",
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized Thesaurus parameter: "foo"', None),
        "ROLLBACK",
        "CREATE TEXT SEARCH DICTIONARY",
        "SAVEPOINT",
        ("ERR", "XX000", "forbidden call of thesaurus or nested call", None),
        "ROLLBACK",
        "CREATE TEXT SEARCH DICTIONARY",
        [("['foot']", "None", "None", "['foot']", "None")],
        "SAVEPOINT",
        (
            "ERR",
            "F0000",
            'thesaurus sample word "one" isn\'t recognized by subdictionary (rule 1)',
            None,
        ),
        "ROLLBACK",
        [
            ("dictfile = 'ispell_sample', afffile = 'ispell_sample', stopwords = 'english'",),
            ("dictfile = 'thesaurus_sample', dictionary = 'english_stem'",),
        ],
        "ROLLBACK",
    ],
    "a thesaurus in ts_lexize": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        [("['1']", "['sn']", "None", "['2']", "None")],
        "ROLLBACK",
    ],
    "ispell dictionaries in a configuration": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            (
                "'ball':7 'book':1,5 'booking':1,5 'foot':7,10 "
                "'football':7 'footballklubber':7 'klubber':7 "
                "'sky':3",
            )
        ],
        [
            (
                "'footballklubber' | 'foot' & 'ball' & 'klubber' | 'football' & 'klubber'",
                "'foot':B & 'ball':B & 'klubber':B & ( 'booking':A | 'book':A ) & 'sky'",
            )
        ],
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            (
                "'ball':7 'book':1,5 'booking':1,5 'foot':7,10 "
                "'football':7 'footballklubber':7 'klubber':7 "
                "'sky':3",
            )
        ],
        [
            (
                "'footballklubber' | 'foot' & 'ball' & 'klubber' | 'football' & 'klubber'",
                "'foot':B & 'ball':B & 'klubber':B & ( 'booking':A | 'book':A ) & 'sky'",
                "( 'foot':B & 'ball':B & 'klubber':B ) <-> 'sky'",
                "( 'foot' & 'ball' & 'klubber' ) <-> 'sky'",
            )
        ],
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            (
                "'ball':7 'book':1,5 'booking':1,5 'foot':7,10 "
                "'football':7 'footballklubber':7 'klubber':7 "
                "'sky':3",
            )
        ],
        [
            (
                "'footballklubber' | 'foot' & 'ball' & 'klubber' | 'football' & 'klubber'",
                "'foot':B & 'ball':B & 'klubber':B & ( 'booking':A | 'book':A ) & 'sky'",
            )
        ],
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            (
                "'ball':7 'book':1,5 'booking':1,5 'foot':7,10 "
                "'football':7 'footballklubber':7 'klubber':7 "
                "'sky':3",
            )
        ],
        [
            (
                "'footballklubber' | 'foot' & 'ball' & 'klubber' | 'football' & 'klubber'",
                "'foot':B & 'ball':B & 'klubber':B & ( 'booking':A | 'book':A ) & 'sky'",
            )
        ],
        [
            (
                "'booking':* | 'book':*",
                "( 'booking' | 'book' ) & ( 'footballklubber' | "
                "'foot' & 'ball' & 'klubber' | 'football' & "
                "'klubber' )",
                "'booking' | 'book' | !( 'footballklubber' | 'foot' "
                "& 'ball' & 'klubber' | 'football' & 'klubber' )",
            )
        ],
        [("The <b>bookings</b> for <b>footballklubber</b> are <b>rebookings</b>",)],
        [
            ("asciiword", "bookings", "ispell", "['booking', 'book']"),
            ("blank", " ", "None", "None"),
            (
                "asciiword",
                "footballklubber",
                "ispell",
                "['footballklubber', 'foot', 'ball', 'klubber', 'football', 'klubber']",
            ),
            ("blank", " ", "None", "None"),
            ("asciiword", "the", "english_stem", "[]"),
            ("blank", " ", "None", "None"),
            ("asciiword", "zzz", "english_stem", "['zzz']"),
        ],
        "ROLLBACK",
    ],
    "a thesaurus in a configuration": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'1':1,5 '12':3 '123':4 'pgsql':2",)],
        [("'abbrevi':10 'call':8 'new':4 'sn':1,9,11 'star':5 'usual':7",)],
        [("'card':3,10 'invit':2,9 'like':6 'look':5 'order':1,8",)],
        [("'sn' & 'star'", "'123'", "'order' <-> 'invit' <-> 'card'", "'12' | 'sn'")],
        [("'12':3 '123':1 '2':8 'card':7 'four':2 'invit':6 'order':5 'sn':4",)],
        [("We like <b>supernovae</b><b> </b><b>stars</b> and one two three things",)],
        [("<b>booking</b><b> </b><b>tickets</b> is fun",)],
        [
            ("asciiword", "one", "thesaurus", "['1']"),
            ("blank", " ", "None", "None"),
            ("asciiword", "two", "thesaurus", "['2']"),
            ("blank", " ", "None", "None"),
            ("asciiword", "supernovae", "thesaurus", "['sn']"),
            ("blank", " ", "None", "None"),
            ("asciiword", "stars", "english_stem", "['star']"),
        ],
        "ROLLBACK",
    ],
    "a synonym's prefix output in a tsquery": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'call':4 'often':3 'pgsql':1,6,8,12 'pronounc':10",)],
        [("'form':8 'index':1,3,10 'plural':7 'right':6",)],
        [("'index' & 'index':*", "'index':*", "'big' <-> 'index':*")],
        "ROLLBACK",
    ],
    "dictinitoption writes a number unquoted": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "ALTER TEXT SEARCH DICTIONARY",
        [("synonyms = 'synonym_sample', casesensitive = 1",)],
        "SAVEPOINT",
        ("ERR", "42601", "casesensitive requires a Boolean value", None),
        "ROLLBACK",
        "ALTER TEXT SEARCH DICTIONARY",
        [("synonyms = 'synonym_sample', casesensitive = 'off'",)],
        "CREATE TEXT SEARCH DICTIONARY",
        [("accept = 0, stopwords = 'english'",)],
        "ALTER TEXT SEARCH DICTIONARY",
        [("stopwords = 'english', accept = 'false'",)],
        "ROLLBACK",
    ],
    "a token type named twice in ALTER MAPPING": [
        "BEGIN",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        "SAVEPOINT",
        ("ERR", "22023", 'token type "not_a_token" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'token type "not_a_token" does not exist', None),
        "ROLLBACK",
        "ALTER TEXT SEARCH CONFIGURATION",
        "SAVEPOINT",
        ("ERR", "42704", 'mapping for token type "word" does not exist', None),
        "ROLLBACK",
        (
            "ALTER TEXT SEARCH CONFIGURATION",
            ['mapping for token type "word" does not exist, skipping'],
        ),
        "ALTER TEXT SEARCH CONFIGURATION",
        [("2", "1", "simple")],
        "ROLLBACK",
    ],
}


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1] + " dbname=postgres"
    pprint.pprint(
        {name: run_scenario(dsn, s[2]) for name, s in SCENARIOS.items()},
        width=100,
        sort_dicts=False,
    )
