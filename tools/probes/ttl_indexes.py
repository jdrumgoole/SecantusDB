"""Differential-probe TTL indexes against a real mongod.

Every scenario gets a collection of its own: an index (or several), a handful
of documents whose dates are placed relative to now, and then ONE wait for the
server's TTL monitor to pass over all of them. What is compared is which
documents are left, the replies to the index commands, and the delete events a
change stream saw.

The monitor is asked to run every second (`ttlMonitorSleepSecs`), and a pass
is detected by `serverStatus.metrics.ttl.passes` moving. A server that has
neither is swept with `secantusAdmin.pruneTtl` and says so in the report.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 python tools/probes/ttl_indexes.py
"""

import datetime
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _servers import probe_targets, report  # noqa: E402
from bson import Decimal128, Int64, ObjectId, Timestamp  # noqa: E402
from pymongo.errors import PyMongoError  # noqa: E402

NOISE = ("$clusterTime", "operationTime", "opTime", "electionId", "uuid")
DB = "ttl_probe"
NOW = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)


def ago(seconds):
    return NOW - datetime.timedelta(seconds=seconds)


#: The window every index here uses, and the long one `collMod` moves between.
#: An HOUR, with the "recent" document ten minutes old: the three servers are
#: run one after another, and with a one-minute window a document that was
#: recent when the probe started had really expired by the time the third
#: server was asked, which read as that server expiring too much.
TTL, LONG = 3600, 864000
OLD, RECENT, FUTURE = ago(3 * TTL), ago(600), ago(-3 * TTL)
OLD_OID = ObjectId.from_datetime(OLD)


def ix(key, **options):
    spec = {"key": key, **options}
    spec.setdefault("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return spec


def docs(field, *values):
    return [{"_id": i, field: v} for i, v in enumerate(values)]


#: Values a plain `{t: 1}` TTL index of an hour is shown.
VALUES = [
    OLD,  # 0 expired
    RECENT,  # 1 inside the window
    FUTURE,  # 2
    None,  # 3
    "2001-01-01T00:00:00Z",  # 4 a string is not a date
    int(OLD.timestamp() * 1000),  # 5 nor is a number
    Timestamp(int(OLD.timestamp()), 1),  # 6 nor a Timestamp
    OLD_OID,  # 7 nor an ObjectId
    [OLD, FUTURE],  # 8 an array expires by its EARLIEST date
    [FUTURE, FUTURE],  # 9
    [OLD, "x"],  # 10
    [],  # 11
    ["x", 5],  # 12
    {"t": OLD},  # 13
    datetime.datetime(1960, 1, 1, tzinfo=datetime.timezone.utc),  # 14 before the epoch
    [FUTURE, OLD],  # 15
    [[OLD]],  # 16 nested array
]

#: (label, [index specs], [documents], [commands run after the index build])
SCENARIOS = [
    ("plain", [ix({"t": 1}, expireAfterSeconds=TTL)], docs("t", *VALUES), []),
    (
        "missing field",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        [{"_id": 0}, {"_id": 1, "u": OLD}],
        [],
    ),
    ("zero seconds", [ix({"t": 1}, expireAfterSeconds=0)], docs("t", ago(5), ago(-600)), []),
    ("descending key", [ix({"t": -1}, expireAfterSeconds=TTL)], docs("t", OLD, FUTURE), []),
    ("seconds as long", [ix({"t": 1}, expireAfterSeconds=Int64(TTL))], docs("t", OLD, RECENT), []),
    ("seconds as double", [ix({"t": 1}, expireAfterSeconds=TTL + 0.9)], docs("t", OLD, RECENT), []),
    (
        "seconds as decimal",
        [ix({"t": 1}, expireAfterSeconds=Decimal128(str(TTL)))],
        docs("t", OLD, RECENT),
        [],
    ),
    ("seconds int32 max", [ix({"t": 1}, expireAfterSeconds=2147483647)], docs("t", OLD), []),
    ("seconds over int32", [ix({"t": 1}, expireAfterSeconds=2147483648)], docs("t", OLD), []),
    ("seconds NaN", [ix({"t": 1}, expireAfterSeconds=float("nan"))], docs("t", OLD), []),
    ("seconds negative", [ix({"t": 1}, expireAfterSeconds=-5)], docs("t", OLD), []),
    ("seconds string", [ix({"t": 1}, expireAfterSeconds="60")], docs("t", OLD), []),
    ("seconds null", [ix({"t": 1}, expireAfterSeconds=None)], docs("t", OLD), []),
    ("seconds bool", [ix({"t": 1}, expireAfterSeconds=True)], docs("t", OLD), []),
    (
        "dotted path",
        [ix({"a.t": 1}, expireAfterSeconds=TTL)],
        [
            {"_id": 0, "a": {"t": OLD}},
            {"_id": 1, "a": {"t": FUTURE}},
            {"_id": 2, "a": [{"t": OLD}, {"t": FUTURE}]},
            {"_id": 3, "a": [{"t": FUTURE}]},
            {"_id": 4, "a": [{"t": [FUTURE, OLD]}]},
            {"_id": 5, "a": 5},
        ],
        [],
    ),
    (
        "compound key",
        [ix({"t": 1, "x": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [],
    ),
    (
        "on _id",
        [{"key": {"_id": 1}, "name": "_id_", "expireAfterSeconds": TTL}],
        docs("t", OLD),
        [],
    ),
    (
        "partial",
        [ix({"t": 1}, expireAfterSeconds=TTL, partialFilterExpression={"gone": True})],
        [
            {"_id": 0, "t": OLD, "gone": True},
            {"_id": 1, "t": OLD, "gone": False},
            {"_id": 2, "t": OLD},
        ],
        [],
    ),
    (
        "sparse",
        [ix({"t": 1}, expireAfterSeconds=TTL, sparse=True)],
        docs("t", OLD, None, FUTURE),
        [],
    ),
    ("unique", [ix({"t": 1}, expireAfterSeconds=TTL, unique=True)], docs("t", OLD, FUTURE), []),
    ("hidden", [ix({"t": 1}, expireAfterSeconds=TTL, hidden=True)], docs("t", OLD, FUTURE), []),
    (
        "hidden later",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "hidden": True}}],
    ),
    (
        "two ttl indexes",
        [ix({"t": 1}, expireAfterSeconds=TTL), ix({"u": 1}, expireAfterSeconds=TTL)],
        [
            {"_id": 0, "t": OLD},
            {"_id": 1, "u": OLD},
            {"_id": 2, "t": FUTURE, "u": FUTURE},
            {"_id": 3, "t": FUTURE, "u": OLD},
        ],
        [],
    ),
    (
        "collMod shortens",
        [ix({"t": 1}, expireAfterSeconds=LONG)],
        docs("t", OLD, RECENT, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": TTL}}],
    ),
    (
        "collMod lengthens",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"keyPattern": {"t": 1}, "expireAfterSeconds": LONG}}],
    ),
    (
        "collMod makes a plain index ttl",
        [ix({"t": 1})],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": TTL}}],
    ),
    (
        "collMod seconds negative",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": -1}}],
    ),
    (
        "collMod seconds string",
        [ix({"t": 1}, expireAfterSeconds=LONG)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": "60"}}],
    ),
    (
        "collMod seconds double",
        [ix({"t": 1}, expireAfterSeconds=LONG)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": TTL + 0.5}}],
    ),
    (
        "collMod seconds over int32",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1", "expireAfterSeconds": 2147483648}}],
    ),
    (
        "collMod compound to ttl",
        [ix({"t": 1, "x": 1})],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "t_1_x_1", "expireAfterSeconds": TTL}}],
    ),
    (
        "collMod _id to ttl",
        [],
        docs("t", OLD, FUTURE),
        [{"collMod": "{c}", "index": {"name": "_id_", "expireAfterSeconds": TTL}}],
    ),
    (
        "index dropped",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"dropIndexes": "{c}", "index": "t_1"}],
    ),
    (
        "same key again other seconds",
        [ix({"t": 1}, expireAfterSeconds=LONG)],
        docs("t", OLD, FUTURE),
        [{"createIndexes": "{c}", "indexes": [ix({"t": 1}, expireAfterSeconds=TTL)]}],
    ),
    (
        "same key again not ttl",
        [ix({"t": 1}, expireAfterSeconds=TTL)],
        docs("t", OLD, FUTURE),
        [{"createIndexes": "{c}", "indexes": [ix({"t": 1})]}],
    ),
    ("2dsphere with seconds", [ix({"t": "2dsphere"}, expireAfterSeconds=TTL)], docs("t", OLD), []),
    ("many expired", [ix({"t": 1}, expireAfterSeconds=TTL)], docs("t", *([OLD] * 250), FUTURE), []),
]

#: Scenarios on a collection made a special way: (label, create options).
SPECIAL = [
    ("capped", {"capped": True, "size": 65536}),
    ("validator", {"validator": {"t": {"$type": "date"}}}),
    ("pre-images", {"changeStreamPreAndPostImages": {"enabled": True}}),
    ("clustered", {"clusteredIndex": {"key": {"_id": 1}, "unique": True}}),
    (
        "clustered expiring",
        {"clusteredIndex": {"key": {"_id": 1}, "unique": True}, "expireAfterSeconds": TTL},
    ),
    ("expireAfterSeconds without clustering", {"expireAfterSeconds": TTL}),
]

PARAMETERS = [
    ("get ttlMonitorSleepSecs", {"getParameter": 1, "ttlMonitorSleepSecs": 1}),
    ("get ttlMonitorEnabled", {"getParameter": 1, "ttlMonitorEnabled": 1}),
    ("set ttlMonitorSleepSecs string", {"setParameter": 1, "ttlMonitorSleepSecs": "x"}),
    ("set ttlMonitorSleepSecs zero", {"setParameter": 1, "ttlMonitorSleepSecs": 0}),
    ("set ttlMonitorEnabled", {"setParameter": 1, "ttlMonitorEnabled": True}),
]

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
TYPE_LIST = re.compile(r"\[([a-zA-Z]+(?:, [a-zA-Z]+)+)\]")


def typed(v):
    if isinstance(v, str):
        v = UUID.sub("<uuid>", v)
        return TYPE_LIST.sub(lambda m: "[" + ", ".join(sorted(m.group(1).split(", "))) + "]", v)
    if isinstance(v, bool) or v is None:
        return v
    if isinstance(v, Int64):
        return f"long:{int(v)}"
    if isinstance(v, int):
        return f"int:{v}"
    if isinstance(v, float):
        return f"double:{v}"
    if isinstance(v, dict):
        return {k: typed(x) for k, x in v.items() if k not in NOISE}
    if isinstance(v, (list, tuple)):
        return [typed(x) for x in v]
    return repr(v)


def command(db, cmd):
    try:
        return ("OK", typed(dict(db.command(cmd))))
    except PyMongoError as exc:
        details = typed(dict(getattr(exc, "details", None) or {"errmsg": str(exc)[:300]}))
        details.pop("ok", None)
        return ("ERR", details)


def name_of(label):
    return "c_" + re.sub(r"[^a-z0-9]+", "_", label.lower())


def passes(client):
    ttl = client.admin.command("serverStatus").get("metrics", {}).get("ttl")
    return None if not isinstance(ttl, dict) else ttl.get("passes")


def sweep(client):
    """Wait for two monitor passes; say how the wait was done."""
    try:
        client.admin.command({"setParameter": 1, "ttlMonitorSleepSecs": 1})
        tuned = True
    except PyMongoError:
        tuned = False
    before = passes(client)
    if before is not None and tuned:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if passes(client) >= before + 2:
                # Back to the default, so a second run reads the same value.
                client.admin.command({"setParameter": 1, "ttlMonitorSleepSecs": 60})
                return "monitor"
            time.sleep(0.25)
        return "monitor did not pass in 30s"
    try:
        client.admin.command("secantusAdmin.pruneTtl")
        return "secantusAdmin.pruneTtl"
    except PyMongoError:
        time.sleep(3)
        return "waited 3s"


def run(client):
    client.drop_database(DB)
    db = client[DB]
    results = {}
    for label, cmd in PARAMETERS:
        results[label] = command(client.admin, cmd)
    for label, indexes, documents, after in SCENARIOS:
        coll = name_of(label)
        db[coll].insert_many(documents)
        built = command(db, {"createIndexes": coll, "indexes": indexes}) if indexes else None
        later = [
            command(db, {k: (coll if v == "{c}" else v) for k, v in cmd.items()}) for cmd in after
        ]
        results[label] = {"build": built, "after": later}
    for label, options in SPECIAL:
        coll = name_of(label)
        made = command(db, {"create": coll, **options})
        built = command(
            db, {"createIndexes": coll, "indexes": [ix({"t": 1}, expireAfterSeconds=60)]}
        )
        # The insert's own answer is part of the comparison: a collection one
        # server refused to create is a collection the other one inserts into.
        seeded = command(
            db,
            {"insert": coll, "documents": [{"_id": 0, "t": OLD}, {"_id": 1, "t": FUTURE}]},
        )
        results[label] = {"create": made, "build": built, "insert": seeded}
    # A change stream over one scenario, opened before anything expires.
    db.cs.insert_many([{"_id": 0, "t": OLD}, {"_id": 1, "t": FUTURE}])
    try:
        stream = db.cs.watch(max_await_time_ms=500)
    except PyMongoError as exc:
        stream = None
        results["change stream"] = ("ERR", str(exc)[:200])
    db.command({"createIndexes": "cs", "indexes": [ix({"t": 1}, expireAfterSeconds=60)]})

    how = sweep(client)

    for label, *_ in SCENARIOS:
        coll = name_of(label)
        results[label]["left"] = sorted(d["_id"] for d in db[coll].find({}, {"_id": 1}))
        results[label]["indexes"] = sorted(
            (typed(i) for i in db[coll].list_indexes()), key=lambda i: i["name"]
        )
    for label, _ in SPECIAL:
        coll = name_of(label)
        results[label]["left"] = sorted(d["_id"] for d in db[coll].find({}, {"_id": 1}))
    if stream is not None:
        events = []
        for _ in range(6):
            event = stream.try_next()
            if event is not None:
                events.append((event["operationType"], typed(event.get("documentKey"))))
        stream.close()
        results["change stream"] = ("OK", events)
    ttl = client.admin.command("serverStatus").get("metrics", {}).get("ttl")
    results["serverStatus metrics.ttl"] = (
        None if not isinstance(ttl, dict) else {k: type(v).__name__ for k, v in sorted(ttl.items())}
    )
    return results, how


def self_check(expected):
    """The reference server must have expired the plain case, or nothing here means anything."""
    left = expected["plain"]["left"]
    if 0 in left or 2 not in left:
        sys.exit(f"SELF-CHECK FAILED on the reference server: plain left {left}")


def main():
    with probe_targets() as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        expected, how = run(mon)
        print(f"  mongod   swept by: {how}")
        self_check(expected)
        got = {}
        for name, cli in targets:
            got[name], how = run(cli)
            print(f"  {name:8s} swept by: {how}")
        for label in expected:
            off = {name for name, g in got.items() if g.get(label) != expected[label]}
            if not off:
                continue
            for name in off:
                divergent[name] += 1
            print(f"  {label}")
            print(f"    mongod  : {expected[label]}")
            for name, g in got.items():
                mark = "   <-- diverges" if name in off else ""
                print(f"    {name:8s}: {g.get(label)}{mark}")
        return report("ttl_indexes", len(expected), divergent)


if __name__ == "__main__":
    sys.exit(main())
