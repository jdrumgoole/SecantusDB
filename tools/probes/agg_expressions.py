"""Sweep every aggregation EXPRESSION operator against mongod.

The argument-type sweeps covered command arguments; the change-stream, update
and findAndModify probes covered those surfaces. The expression language — the
largest operator family in the server, ~143 of them — had never been measured
operator by operator, and the two forays into it that did happen (the `$redact`
decision variables, `$$REMOVE`) each found data-correctness bugs including
crashes. This is that sweep.

It is deliberately GENERIC: rather than hand-writing cases per operator, it
feeds each one a shared corpus of argument values in 1- and 2-argument form.
Most combinations are type errors, and that is the point — comparing the ERROR
is as much a conformance check as comparing a value, and the wrong-typed
argument class has been the most productive one in this repo.

Run it like the other probes; `PROBE_SERVER` points at an already-running
server (this is how the Rust server is swept), otherwise an embedded Python one
is started.
"""

import datetime
import os
import sys
import tempfile

import pymongo
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus import SecantusDBServer

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY")

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)

#: The seeded document. Field names are short so the case labels stay readable.
DOC = {"_id": 1, "n": 2, "d": 1.5, "s": "abc", "arr": [3, 1, 2], "o": {"k": 1}, "t": WHEN}

#: One corpus, reused for every operator. 143 operators times this times two
#: arities is ~6k comparisons.
#:
#: The second block was added 2026-09-03 after five CRASH-CLASS bugs -- each an
#: `internal server error` reachable from any query -- turned up in a value
#: class this list did not contain. The corpus had run tens of thousands of
#: times without one infinity in it, so no amount of re-running could have found
#: them. A value CLASS that is absent is invisible in exactly the way a passing
#: test is: it costs nothing and proves nothing.
VALUES = [
    0,
    1,
    -1,
    1.5,
    Int64(2**40),
    Decimal128("2.5"),
    "abc",
    "",
    True,
    None,
    [3, 1, 2],
    [],
    {"k": 1},
    OID,
    WHEN,
    "$n",
    "$s",
    "$arr",
    "$o",
    "$nosuch",
    # --- classes the original list had none of -------------------------
    # Non-finite, in both numeric types. `math.ceil(inf)` raises
    # `OverflowError` and `Decimal` traps `InvalidOperation`, so these reach
    # unguarded code as exceptions rather than values.
    float("inf"),
    float("-inf"),
    float("nan"),
    Decimal128("Infinity"),
    Decimal128("NaN"),
    # Signed zero, which several operators are supposed to preserve.
    -0.0,
    Decimal128("-0"),
    # The boundaries where a width changes: int32 overflow, int64 overflow,
    # and a double too large for either.
    2**31 - 1,
    -(2**31),
    Int64(2**63 - 1),
    1e308,
    # The BSON types the list skipped entirely.
    MinKey(),
    MaxKey(),
    Binary(b"z"),
    Timestamp(1, 1),
    Regex("a", "i"),
    Code("x=1"),
    # Empty and nested containers, which the flat `[3, 1, 2]` does not reach.
    {},
    [[1]],
]

#: `datetime_conversion="DATETIME_AUTO"` is required, not a nicety: with the
#: widened corpus some operators legitimately produce a date outside Python's
#: `datetime` range (`{$toDate: Int64(2**63 - 1)}` is year 292278994), and the
#: DEFAULT codec raises `InvalidBSON` while DECODING the reply. That kills the
#: probe rather than reporting a divergence -- and it kills it identically for
#: mongod, so it is the client that cannot cope, not either server.
_CLIENT_OPTS = dict(
    directConnection=True,
    serverSelectionTimeoutMS=8000,
    datetime_conversion="DATETIME_AUTO",
)

#: Operators whose single argument is conventionally an array of operands, so
#: the 1-argument form would be meaningless. They still get the 2-arg form.
_NARY = {"$add", "$multiply", "$concat", "$concatArrays", "$setUnion", "$setIntersection"}


def operators():
    import secantus.expressions as expressions

    return sorted(getattr(expressions, "_OPS", {}))


def cases():
    for op in operators():
        for v in VALUES:
            if op not in _NARY:
                yield (f"{op}/1:{v!r}", {op: v})
        # Two-argument form, against a small slice of the corpus, so operators
        # that need a pair are exercised too.
        for v in VALUES[:8]:
            yield (f"{op}/2:{v!r}", {op: [v, 1]})


# Expressions whose VALUE cannot be compared: two correct servers disagree by
# design. Their error behaviour is still compared.
NON_DETERMINISTIC = frozenset({"$rand"})


def run(cli, dbn, expr):
    db = cli[dbn]
    try:
        out = list(db.c.aggregate([{"$addFields": {"z": expr}}]))
        if not out:
            return ("EMPTY", None)
        return ("OK", "MISSING" if "z" not in out[0] else repr(out[0]["z"]))
    except pymongo.errors.PyMongoError as e:
        d = getattr(e, "details", None)
        msg = d.get("errmsg", "") if isinstance(d, dict) else str(e)
        return (getattr(e, "code", None), msg[:160])
    except (TypeError, ValueError, OverflowError, RecursionError):
        return ("CLIENT", None)


def main():
    if SERVER:
        srv = None
        sec = pymongo.MongoClient(SERVER, **_CLIENT_OPTS)
        print(f"  server under test: {SERVER}")
    else:
        d = tempfile.mkdtemp()
        srv = SecantusDBServer(port=0, storage_path=d)
        srv.start()
        sec = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", **_CLIENT_OPTS)
        print("  server under test: embedded Python SecantusDBServer")
    mon = pymongo.MongoClient(MONGOD, **_CLIENT_OPTS)

    # One database, seeded once: every case is a read, so they cannot interfere.
    for cli in (mon, sec):
        db = cli["exprsweep"]
        db.c.drop()
        db.c.insert_one(dict(DOC))

    value_diffs, code_diffs, msg_diffs, total = [], [], [], 0
    for label, expr in cases():
        if ONLY and ONLY not in label:
            continue
        total += 1
        m = run(mon, "exprsweep", expr)
        o = run(sec, "exprsweep", expr)
        if m == o:
            continue
        if m[0] == "OK" and o[0] == "OK":
            # `$rand` returns a different draw every call, so two successes
            # ARE agreement -- comparing the values reported a "wrong answer"
            # the moment `$rand` started succeeding correctly.
            if label.split("/")[0] in NON_DETERMINISTIC:
                continue
            value_diffs.append((label, m, o))
        elif m[0] != o[0]:
            code_diffs.append((label, m, o))
        else:
            msg_diffs.append((label, m, o))

    print(
        f"  cases: {total}   WRONG VALUE: {len(value_diffs)}   "
        f"different code: {len(code_diffs)}   message only: {len(msg_diffs)}\n"
    )
    for title, rows in (
        ("WRONG VALUE (both succeeded, answers differ)", value_diffs),
        ("DIFFERENT CODE / one errored", code_diffs),
        ("MESSAGE ONLY (code matches)", msg_diffs),
    ):
        if not rows:
            continue
        print(f"  === {title}: {len(rows)} ===")
        for label, m, o in rows:
            print(f"    {label}")
            print(f"        mongod {m}")
            print(f"        ours   {o}")
        print()
    sec.close()
    if srv is not None:
        srv.stop()
    return 1 if (value_diffs or code_diffs or msg_diffs) else 0


sys.exit(main())
