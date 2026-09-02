"""Feed wrong-typed arguments to commands that structurally dereference them.

Twice this session a caller-supplied scalar reached code assuming a document and
crashed as "internal server error": `pipeline: [42]` -> len(), `update: 5` ->
.keys(). This sweeps the same shape across command arguments that are documents,
arrays, or nested specs, and compares against mongod.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from _servers import probe_targets, report  # noqa: E402

BAD = [5, "x", True, [1, 2]]


def cases():
    for bad in BAD:
        yield (f"find.filter={bad!r}", {"find": "c", "filter": bad})
        yield (f"find.sort={bad!r}", {"find": "c", "sort": bad})
        yield (f"find.projection={bad!r}", {"find": "c", "projection": bad})
        yield (f"count.query={bad!r}", {"count": "c", "query": bad})
        yield (f"distinct.query={bad!r}", {"distinct": "c", "key": "a", "query": bad})
        yield (f"delete.q={bad!r}", {"delete": "c", "deletes": [{"q": bad, "limit": 1}]})
        yield (
            f"update.q={bad!r}",
            {"update": "c", "updates": [{"q": bad, "u": {"$set": {"a": 1}}}]},
        )
        yield (f"update.u={bad!r}", {"update": "c", "updates": [{"q": {}, "u": bad}]})
        yield (f"insert.documents={bad!r}", {"insert": "c", "documents": bad})
        yield (f"aggregate.pipeline={bad!r}", {"aggregate": "c", "pipeline": bad, "cursor": {}})
        yield (
            f"createIndexes.key={bad!r}",
            {"createIndexes": "c", "indexes": [{"key": bad, "name": "i"}]},
        )
        yield (f"fam.query={bad!r}", {"findAndModify": "c", "query": bad, "remove": True})
        yield (
            f"fam.sort={bad!r}",
            {"findAndModify": "c", "query": {}, "sort": bad, "remove": True},
        )
        yield (
            f"fam.fields={bad!r}",
            {"findAndModify": "c", "query": {}, "fields": bad, "remove": True},
        )


def run(cli, dbn, cmd):
    db = cli[dbn]
    db.c.drop()
    db.c.insert_one({"_id": 1, "a": 1})
    try:
        db.command(dict(cmd))
        return ("OK", None)
    except Exception as e:
        return ("ERR", getattr(e, "code", None))


with probe_targets() as (mon, targets):
    divergent = {label: 0 for label, _ in targets}
    crashes = {label: 0 for label, _ in targets}
    total = 0
    for i, (label, cmd) in enumerate(cases()):
        total += 1
        expected = run(mon, f"t{i}", cmd)
        got = {name: run(cli, f"t{i}", cmd) for name, cli in targets}
        # A code-1 answer is an unhandled exception reaching the wire, which is
        # what this probe exists to catch; it is called out separately from an
        # ordinary mismatch.
        for name, value in got.items():
            if value == ("ERR", 1):
                crashes[name] += 1
        off = {k for k, v in got.items() if v != expected}
        if not off:
            continue
        for name in off:
            divergent[name] += 1
        print(f"  {label}")
        print(f"    mongod  : {expected}")
        for name, value in got.items():
            mark = "   <-- diverges" if name in off else ""
            crash = "  CRASH" if value == ("ERR", 1) else ""
            print(f"    {name:8s}: {value}{crash}{mark}")
        print()
    if any(crashes.values()):
        print(
            "  CRASHES (code 1, an unhandled exception on the wire): "
            + ", ".join(f"{k} {v}" for k, v in crashes.items() if v)
        )
    sys.exit(report("wrong-typed document arguments", total, divergent))
