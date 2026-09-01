"""Sweep collated ORDER against mongod, with and without a collated index.

Collation was implemented for MATCHING and only for matching: strings were
folded to one value and then compared by codepoint, so accents sorted after
``z``, tertiary case order was absent, and ``caseFirst`` / ``backwards`` /
``numericOrdering`` were accepted and ignored. This is the sweep that measured
that (2026-08-31) and the one that verifies the three-level key which replaced
it (2026-09-01).

Every case is run TWICE -- once on a bare collection and once with a collated
index on the sorted field -- because the interesting failure is not just "wrong
order" but "different order depending on whether an index exists". An index must
change speed, never results.

What remains open here is the LOCALE. Swedish sorts ``ä`` after ``z`` and
Danish sorts ``å`` last; decomposition cannot discover that, it is CLDR data.
Those cases are expected to diverge until an ICU dependency is taken.
"""

import os
import sys
import tempfile

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")

CASES = [
    ("accents_en", ["a", "á", "ä", "az", "b"], {"locale": "en"}),
    ("accents_mixed", ["resume", "résumé", "resumes", "Resume"], {"locale": "en"}),
    ("case_s3", ["a", "A", "b", "B"], {"locale": "en", "strength": 3}),
    (
        "case_first_upper",
        ["a", "A", "b", "B"],
        {"locale": "en", "strength": 3, "caseFirst": "upper"},
    ),
    (
        "case_first_lower",
        ["a", "A", "b", "B"],
        {"locale": "en", "strength": 3, "caseFirst": "lower"},
    ),
    ("numeric", ["10", "9", "2", "100"], {"locale": "en", "numericOrdering": True}),
    ("numeric_mixed", ["a2", "a10", "a1b3", "a1b20"], {"locale": "en", "numericOrdering": True}),
    ("numeric_off", ["10", "9", "2", "100"], {"locale": "en"}),
    ("backwards_fr", ["cote", "côte", "coté", "côté"], {"locale": "fr", "backwards": True}),
    ("forwards_fr", ["cote", "côte", "coté", "côté"], {"locale": "fr"}),
    ("de_locale", ["ä", "az", "b"], {"locale": "de"}),
    ("strength_1", ["a", "A", "á", "B", "b"], {"locale": "en", "strength": 1}),
    ("strength_2", ["a", "A", "á", "B", "b"], {"locale": "en", "strength": 2}),
    ("case_level_s1", ["a", "A", "b"], {"locale": "en", "strength": 1, "caseLevel": True}),
    ("multi_accent", ["a", "à", "á", "â", "ã", "ä"], {"locale": "en"}),
    ("empty_and_short", ["", "a", "ab", "A"], {"locale": "en"}),
    ("nonlatin", ["ä", "o", "ö", "z"], {"locale": "en"}),
    # KNOWN OPEN: locale-specific ordering needs CLDR data, not decomposition.
    ("sv_locale", ["a", "z", "ä", "ö"], {"locale": "sv"}),
    ("da_locale", ["a", "z", "å"], {"locale": "da"}),
]

#: Cases expected to diverge until an ICU dependency is taken (see the module
#: docstring). Listed so the sweep's headline number means "unexpected".
KNOWN_LOCALE_GAPS = {"sv_locale", "da_locale"}


def main() -> int:
    mon = pymongo.MongoClient(MONGOD)
    srv = None
    if SERVER:
        sec = pymongo.MongoClient(SERVER)
    else:
        from secantus import SecantusDBServer

        srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
        srv.start()
        sec = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}")

    unexpected = 0
    known = 0
    for name, values, spec in CASES:
        dbn = f"probe_coll_{name}"
        mon.drop_database(dbn)
        sec.drop_database(dbn)
        docs = [{"_id": i, "v": v} for i, v in enumerate(values)]
        for db in (mon[dbn], sec[dbn]):
            db.c.insert_many([dict(d) for d in docs])
        expected = [d["v"] for d in mon[dbn].c.find().sort("v", 1).collation(spec)]
        plain = [d["v"] for d in sec[dbn].c.find().sort("v", 1).collation(spec)]
        for db in (mon[dbn], sec[dbn]):
            db.c.create_index("v", collation=spec, name="v_c")
        indexed = [d["v"] for d in sec[dbn].c.find().sort("v", 1).collation(spec)]
        ok = expected == plain == indexed
        if not ok:
            if name in KNOWN_LOCALE_GAPS:
                known += 1
                label = "KNOWN"
            else:
                unexpected += 1
                label = "DIFF"
            print(f"[{label}] {name} {spec}")
            print(f"   mongod      {expected}")
            print(f"   secantus    {plain}")
            print(f"   +index      {indexed}")
    print(
        f"--- collated order: {unexpected} unexpected divergences, "
        f"{known} known locale gaps, of {len(CASES)} cases"
    )
    if srv is not None:
        srv.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
