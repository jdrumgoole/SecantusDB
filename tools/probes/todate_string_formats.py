"""Which strings does `$toDate` accept, and what does each parse to?

mongod's `$toDate` runs **timelib's** parser, not an ISO-8601 one. Both servers
implemented a small ISO subset and rejected the rest, so `{$toDate:
"12/31/2020"}` — an ordinary call — answered `241` where mongod returns a date.
15 of 19 shapes diverged when this was first run (2026-09-09).

Three rules worth keeping in view, all measured rather than assumed:

* **The slash form is US-first by RULE, not by ambiguity-resolution.**
  `31/12/2020` is refused outright, so `MM/DD/YYYY` wins and `DD/MM/YYYY` is not
  a fallback.
* **A trailing letter is a MILITARY TIMEZONE, not the ISO separator.**
  `2020-01-01T` is `07:00:00`, because `T` is UTC-7 — deterministic, and NOT
  host-local: a `TZ=UTC` mongod answers the same. `J` is invalid there and here.
* **An out-of-range component is a parse FAILURE, not a rollover.**
  `13/01/2020` and `12/32/2020` are both refused.

The REJECTIONS are as load-bearing as the acceptances, which is why they are in
the corpus: a parser that accepts too much is as wrong as one that accepts too
little, and only the rejections catch it.

Not covered: mongod's per-position timelib diagnostic for a string it got
partway through (`'abc'` names the offending character and where its scanner
stopped). That needs timelib's own lexer and abbreviation tables; inventing a
position would look authoritative and be wrong. Both servers give the same code
and a general message.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/todate_string_formats.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pymongo

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

STRINGS = [
    # Plain ISO, and the forms `fromisoformat` happens to cover.
    "2020-01-01",
    "2020-01-01T00:00:00Z",
    "2020-01-01T10:00:00.5",
    "2020-01-01T00",
    "20200101",
    "20200101T120000",
    "2020-W01-1",
    # Surrounding whitespace is tolerated...
    "2020-01-01 ",
    "  2020-01-01",
    "2020-01-01\t",
    # ...but a whitespace-only or empty string is not.
    "",
    "   ",
    # timelib's other date forms.
    "12/31/2020",
    "1/2/2020",
    "12/31/2020 10:30",
    "2020/12/31",
    "2020/1/2",
    "2020-1-1",
    "2020-1-1 10:30",
    "Dec 31 2020",
    "dec 31 2020",
    "December 31 2020",
    "31 Dec 2020",
    "31 December 2020",
    "Dec 31, 2020",
    "@1577836800",
    "@0",
    "@-1",
    "@1577836800.5",
    # Military timezone letters -- A..I, K..M, N..Y, Z; J is invalid.
    "2020-01-01T",
    "2020-01-01t",
    "2020-01-01A",
    "2020-01-01M",
    "2020-01-01N",
    "2020-01-01Y",
    "2020-01-01Z",
    "2020-01-01J",
    "2020-01-01 T",
    # Rejections: the wrong slash order, and out-of-range components.
    "31/12/2020",
    "13/01/2020",
    "12/32/2020",
    "2020-02-30",
    "not a date",
    "abc",
    "2020",
]


def _run(client, index):
    db = client["todateformats"]
    try:
        rows = list(
            db["c"].aggregate([{"$match": {"_id": index}}, {"$project": {"v": {"$toDate": "$s"}}}])
        )
        return ("OK", str(rows[0].get("v")) if rows else None)
    except pymongo.errors.OperationFailure as exc:
        # The per-position diagnostic is deliberately out of scope, so compare
        # the CODE for a failure rather than mongod's scanner text.
        return ("ERR", exc.code)


def _seed(client):
    db = client["todateformats"]
    db.drop_collection("c")
    db["c"].insert_many([{"_id": i, "s": s} for i, s in enumerate(STRINGS)])
    return client


def main() -> int:
    with probe_targets() as (mongod, targets):
        _seed(mongod)
        for _, client in targets:
            _seed(client)
        divergent = {label: 0 for label, _ in targets}
        for i, text in enumerate(STRINGS):
            want = _run(mongod, i)
            for label, client in targets:
                got = _run(client, i)
                if got != want:
                    divergent[label] += 1
                    print(f"<<< [{label}] {text!r}")
                    print(f"    mongod {want}")
                    print(f"    ours   {got}")
        return report("$toDate string formats", len(STRINGS), divergent)


if __name__ == "__main__":
    raise SystemExit(main())
