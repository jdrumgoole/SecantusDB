"""An unknown expression operator: which code, which envelope, which wording.

mongod does NOT answer this one way. The discriminator is POSITION, measured
against 8.2.11 (2026-09-17):

- the top-level value of a `$project` field is parsed by the PROJECTION parser,
  which has its own code and wording --
  ``31325 Invalid $project :: caused by :: Unknown expression $x``;
- anywhere deeper, including nested inside an expression in that same
  `$project`, the generic expression parser answers
  ``168 ... Unrecognized expression '$x'`` -- note the quotes and the different
  verb;
- `$addFields` / `$set` wrap 168 in their own stage envelope;
- `$group` / `$replaceWith` / `$match`'s `$expr` return a BARE 168, no envelope.

So "`$project` uses 31325" is too coarse a rule to implement from: the same
stage answers both codes depending on how deep the operator sits.

Run: `python tools/probes/unknown_expression_errors.py` with a mongod at
`PROBE_MONGOD` (default 127.0.0.1:27041).
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _servers import probe_targets, report  # noqa: E402

# (name, stage) -- every one names an operator that does not exist, or one that
# exists ONLY as an accumulator and so is unknown in an expression position.
CASES = [
    ("project/top/$nosuch", {"$project": {"n": {"$nosuch": 1}}}),
    ("project/top/$count", {"$project": {"n": {"$count": {}}}}),
    (
        "project/top/$topN",
        {"$project": {"n": {"$topN": {"n": 1, "sortBy": {"a": 1}, "output": "$a"}}}},
    ),
    (
        "project/top/$bottomN",
        {"$project": {"n": {"$bottomN": {"n": 1, "sortBy": {"a": 1}, "output": "$a"}}}},
    ),
    ("project/nested/$nosuch", {"$project": {"n": {"$add": [{"$nosuch": 1}, 1]}}}),
    ("project/nested/$count", {"$project": {"n": {"$add": [{"$count": {}}, 1]}}}),
    ("addFields/top/$nosuch", {"$addFields": {"n": {"$nosuch": 1}}}),
    ("addFields/top/$count", {"$addFields": {"n": {"$count": {}}}}),
    ("addFields/nested/$nosuch", {"$addFields": {"n": {"$add": [{"$nosuch": 1}, 1]}}}),
    ("set/top/$nosuch", {"$set": {"n": {"$nosuch": 1}}}),
    ("group/_id/$nosuch", {"$group": {"_id": {"$nosuch": 1}}}),
    ("replaceWith/$nosuch", {"$replaceWith": {"$nosuch": 1}}),
    ("match/$expr/$nosuch", {"$match": {"$expr": {"$nosuch": 1}}}),
]


def answer(client, stage) -> tuple:
    """`(code, codeName, errmsg)` -- compared as three fields, not one blob.

    Comparing pymongo's rendered string instead would fold three independent
    questions into one number: the CODE (168 vs 31325), the codeName, and the
    stage ENVELOPE around the message. They have different causes and want
    different fixes, so they are reported apart.
    """
    try:
        client.db.command("aggregate", 1, pipeline=[{"$documents": [{}]}, stage], cursor={})
    except Exception as exc:  # noqa: BLE001 - the error IS the measurement
        d = getattr(exc, "details", None) or {}
        return (d.get("code", type(exc).__name__), d.get("codeName"), d.get("errmsg"))
    return ("OK", None, None)


def main() -> int:
    divergent: dict[str, int] = {}
    fields: dict[str, dict[str, int]] = {}
    with probe_targets() as (mongod, targets):
        for name, stage in CASES:
            want = answer(mongod, stage)
            for label, client in targets:
                got = answer(client, stage)
                if got == want:
                    continue
                divergent[label] = divergent.get(label, 0) + 1
                which = [n for n, a, b in zip(("code", "codeName", "errmsg"), want, got) if a != b]
                for n in which:
                    fields.setdefault(label, {})[n] = fields.setdefault(label, {}).get(n, 0) + 1
                print(f"  {name} [{label}] differs in {'+'.join(which)}")
                print(f"      mongod: {want}")
                print(f"      {label:>6}: {got}")
    for label, counts in sorted(fields.items()):
        print(f"  {label}: " + ", ".join(f"{n}={c}" for n, c in sorted(counts.items())))
    return report("unknown_expression_errors", len(CASES), divergent)


if __name__ == "__main__":
    raise SystemExit(main())
