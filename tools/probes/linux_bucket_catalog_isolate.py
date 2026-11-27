"""RM-710: isolate the pieces of Linux mongod's bucketCatalog.memoryUsage.

Like linux_bucket_catalog.py (same Docker/Colima setup), but inserts through
`db.command("insert")` so the driver adds no `_id`, and varies what is in
the catalog one structure at a time. Usage:

    DOCKER_HOST=unix://$HOME/.colima/default/docker.sock \\
        .venv/bin/python tools/probes/linux_bucket_catalog_isolate.py 8.2.11

Prints one JSON line per (version, case).
"""

import datetime as dt
import json
import sys

import pymongo

T = dt.datetime(2026, 1, 1)


def _helpers():
    import subprocess
    import time

    def start(version):
        cid = subprocess.check_output(
            [
                "docker",
                "run",
                "-d",
                "--rm",
                "--platform",
                "linux/amd64",
                "-p",
                "127.0.0.1::27017",
                f"mongo:{version}",
                "mongod",
                "--bind_ip_all",
                "--quiet",
            ],
            text=True,
        ).strip()
        port = int(
            subprocess.check_output(["docker", "port", cid, "27017/tcp"], text=True)
            .strip()
            .splitlines()[0]
            .rsplit(":", 1)[1]
        )
        deadline = time.time() + 90
        while True:
            try:
                c = pymongo.MongoClient(port=port, serverSelectionTimeoutMS=2000)
                c.admin.command("ping")
                c.close()
                return cid, port
            except Exception:
                if time.time() > deadline:
                    raise
                time.sleep(1)

    def stop(cid):
        subprocess.run(["docker", "stop", "-t", "5", cid], capture_output=True, check=True)

    return start, stop


start, stop = _helpers()


def mem(c):
    return c.admin.command("serverStatus").get("bucketCatalog", {}).get("memoryUsage")


def mk(c, meta=True, name="ts"):
    db = c["rm_isolate"]
    opts = {"timeField": "t", "metaField": "m"} if meta else {"timeField": "t"}
    db.create_collection(name, timeseries=opts)
    return db


def ins(db, docs, name="ts"):
    r = db.command("insert", name, documents=docs)
    assert r.get("ok") == 1 and not r.get("writeErrors"), r


def d(**kw):
    return {"t": T, **kw}


def case_t_m(c):
    db = mk(c)
    ins(db, [d(m="a")])
    return mem(c)


def case_t_only(c):
    db = mk(c, meta=False)
    ins(db, [d()])
    return mem(c)


def case_t_m_x(c):
    db = mk(c)
    ins(db, [d(m="a", x=1)])
    return mem(c)


def case_t_m_x_y(c):
    db = mk(c)
    ins(db, [d(m="a", x=1, y=2)])
    return mem(c)


def case_t_m_xyz(c):
    db = mk(c)
    ins(db, [d(m="a", x=1, y=2, z=3)])
    return mem(c)


def case_n_metas(n):
    def f(c):
        db = mk(c)
        out = []
        for i in range(n):
            ins(db, [d(m=f"meta{i}")])
            out.append(mem(c))
        return out

    return f


def case_two_colls(c):
    db = mk(c)
    mk(c, name="ts2") if False else db.create_collection(
        "ts2", timeseries={"timeField": "t", "metaField": "m"}
    )
    ins(db, [d(m="a")])
    a = mem(c)
    ins(db, [d(m="a")], name="ts2")
    return [a, mem(c)]


def case_same_series_twice(c):
    db = mk(c)
    ins(db, [d(m="a")])
    a = mem(c)
    ins(db, [{"t": T + dt.timedelta(seconds=1), "m": "a"}])
    return [a, mem(c)]


def case_same_series_same_call(c):
    db = mk(c)
    ins(db, [d(m="a"), {"t": T + dt.timedelta(seconds=1), "m": "a"}])
    return mem(c)


def case_types(typ):
    vals = {
        "int32": 1,
        "int64": 1 << 40,
        "double": 1.5,
        "str5": "hello",
        "str20": "x" * 20,
        "bool": True,
        "null": None,
        "oid": __import__("bson").ObjectId(),
        "obj": {"a": 1},
        "arr": [1, 2],
        "date": T,
        "dec": __import__("bson").Decimal128("1.5"),
    }

    def f(c):
        db = mk(c)
        ins(db, [d(m="a", x=vals[typ])])
        return mem(c)

    return f


def case_long_field(c):
    db = mk(c)
    ins(db, [d(m="a", **{"f" * 20: 1})])
    return mem(c)


def case_meta_obj(c):
    db = mk(c)
    ins(db, [d(m={"a": 1, "b": "x" * 600})])
    return mem(c)


def case_commit_in_one_call_n(n):
    def f(c):
        db = mk(c)
        ins(db, [{"t": T + dt.timedelta(seconds=i), "m": "a"} for i in range(n)])
        return mem(c)

    return f


def case_gen(meta, x=None, extra=None):
    def f(c):
        db = mk(c)
        doc = d(m=meta)
        if x is not None:
            doc["x"] = x
        if extra:
            doc.update(extra)
        ins(db, [doc])
        return mem(c)

    return f


CASES = {
    "{t,m}": case_t_m,
    "{t} no meta": case_t_only,
    "{t,m,x}": case_t_m_x,
    "{t,m,x,y}": case_t_m_x_y,
    "{t,m,x,y,z}": case_t_m_xyz,
    "n metas 1..4 (series)": case_n_metas(4),
    "two collections": case_two_colls,
    "same series, 2nd call": case_same_series_twice,
    "same series, same call": case_same_series_same_call,
    "long field name": case_long_field,
    "meta obj with 600B string": case_meta_obj,
    "1000 meas": case_commit_in_one_call_n(5),
}
for typ in [
    "int32",
    "int64",
    "double",
    "str5",
    "str20",
    "bool",
    "null",
    "oid",
    "obj",
    "arr",
    "date",
    "dec",
]:
    CASES[f"x:{typ}"] = case_types(typ)

for n in (400, 480, 497, 505, 520, 1000, 2000):
    CASES[f"meta str {n}"] = case_gen("s" * n)
CASES["meta obj {a:1}"] = case_gen({"a": 1})
CASES["meta obj {a:{b:1}}"] = case_gen({"a": {"b": 1}})
CASES["meta arr [1,2]"] = case_gen([1, 2])
CASES["meta int"] = case_gen(7)
for name, v in {
    "obj {a:1}": {"a": 1},
    "obj {a:1,b:2}": {"a": 1, "b": 2},
    "obj {a:{b:1}}": {"a": {"b": 1}},
    "obj {}": {},
    "arr []": [],
    "arr [1]": [1],
    "arr [1,2,3]": [1, 2, 3],
    "arr [[1]]": [[1]],
    "obj {a:'hello'}": {"a": "hello"},
    "str15": "y" * 15,
    "str16": "y" * 16,
}.items():
    CASES[f"x:{name}"] = case_gen("a", v)


def steps(*calls):
    """`memoryUsage` after each call (a list of documents) into one series."""

    def f(c):
        db = mk(c)
        out = []
        for docs in calls:
            ins(db, docs)
            out.append(mem(c))
        return out

    return f


def sec(i):
    return T + dt.timedelta(seconds=i)


CASES["obj x3 in one call"] = steps([d(m="a", x={"a": i}) for i in range(3)])
CASES["obj x1 then x1 then x1"] = steps(*[[d(m="a", x={"a": i})] for i in range(3)])
CASES["obj two fields x4 calls"] = steps(*[[d(m="a", x={"a": i, "b": f"s{i}"})] for i in range(4)])
CASES["arr x3 in one call"] = steps([d(m="a", x=[i, i + 1]) for i in range(3)])
CASES["int ramp 100 in one call"] = steps([{"t": sec(i), "m": "a", "x": i * 7} for i in range(100)])
CASES["int ramp 10 calls of 10"] = steps(
    *[
        [{"t": sec(10 * k + i), "m": "a", "x": (10 * k + i) * 7} for i in range(10)]
        for k in range(10)
    ]
)
CASES["double noise 30 calls"] = steps(
    *[[{"t": sec(i), "m": "a", "x": 1.0 + (i * 37 % 11) / 10} for i in [k]] for k in range(30)]
)
CASES["string varying 6 calls"] = steps(
    *[[{"t": sec(k), "m": "a", "x": f"word{k * k}"}] for k in range(6)]
)
CASES["missing field then present"] = steps(
    [d(m="a", x=1)], [{"t": sec(1), "m": "a"}], [{"t": sec(2), "m": "a", "x": 5}]
)
CASES["time backward archive then reopen"] = steps(
    [{"t": sec(1800), "m": "a"}],
    [{"t": sec(-120), "m": "a"}],
    [{"t": sec(1801), "m": "a"}],
    [{"t": sec(-121), "m": "a"}],
    [{"t": sec(1802), "m": "a"}],
)
CASES["schema change"] = steps(
    [d(m="a", x=1)], [{"t": sec(1), "m": "a", "x": "str"}], [{"t": sec(2), "m": "a", "x": 2}]
)
CASES["two series x2 calls"] = steps(
    [d(m="a"), d(m="b")], [{"t": sec(1), "m": "a"}, {"t": sec(1), "m": "b"}]
)


CASES["obj long strings 20 calls"] = steps(
    *[
        [{"t": sec(k), "m": "a", "x": {"a": "s" * 30 + str(k), "b": k * 13, "c": 1.5 * k}}]
        for k in range(20)
    ]
)
CASES["obj 40 measurements in one call"] = steps(
    [
        {"t": sec(k), "m": "a", "x": {"a": "word" + str(k * 7), "b": k * 13, "c": 1.5 * k}}
        for k in range(40)
    ]
)
CASES["obj and scalar alternate"] = steps(
    *[[{"t": sec(k), "m": "a", "x": ({"a": k} if k % 2 else k)}] for k in range(8)]
)
CASES["arrays of varying length"] = steps(
    *[[{"t": sec(k), "m": "a", "x": list(range(k % 5 + 1))}] for k in range(10)]
)
CASES["nested obj 3 calls"] = steps(
    *[
        [{"t": sec(k), "m": "a", "x": {"p": {"q": k, "r": "z" * k}, "s": [k, k + 1]}}]
        for k in range(3)
    ]
)
CASES["many fields 40 in one doc"] = steps(
    [{"t": sec(0), "m": "a", **{f"f{i}": i for i in range(40)}}]
)
CASES["many fields grow over calls"] = steps(
    *[[{"t": sec(k), "m": "a", **{f"f{i}": i for i in range(k * 5 + 1)}}] for k in range(6)]
)
CASES["fields in different order"] = steps(
    [{"t": sec(0), "m": "a", "p": 1, "q": 2, "r": 3}],
    [{"t": sec(1), "m": "a", "r": 3, "q": 2, "p": 1}],
    [{"t": sec(2), "m": "a", "q": 2, "z": 9}],
)
CASES["50 buckets via time forward"] = steps(
    *[
        [{"t": dt.datetime(2026, 1, 1) + dt.timedelta(hours=2 * k), "m": "a", "x": k}]
        for k in range(6)
    ]
)


CASES["arr [0] then [0,1]"] = steps(
    [{"t": sec(0), "m": "a", "x": [0]}], [{"t": sec(1), "m": "a", "x": [0, 1]}]
)
CASES["arr [0,1] then [0]"] = steps(
    [{"t": sec(0), "m": "a", "x": [0, 1]}], [{"t": sec(1), "m": "a", "x": [0]}]
)
CASES["arr [0] and [0,1] one call"] = steps(
    [{"t": sec(0), "m": "a", "x": [0]}, {"t": sec(1), "m": "a", "x": [0, 1]}]
)
CASES["obj {a} then {a,b}"] = steps(
    [{"t": sec(0), "m": "a", "x": {"a": 0}}], [{"t": sec(1), "m": "a", "x": {"a": 0, "b": 1}}]
)
CASES["obj {a,b} then {a}"] = steps(
    [{"t": sec(0), "m": "a", "x": {"a": 0, "b": 1}}], [{"t": sec(1), "m": "a", "x": {"a": 0}}]
)
CASES["arr [0] x3 then [0,1]"] = steps(
    *[[{"t": sec(k), "m": "a", "x": [0]}] for k in range(3)], [{"t": sec(3), "m": "a", "x": [0, 1]}]
)
CASES["arr [0,1] then [0,1,2]"] = steps(
    [{"t": sec(0), "m": "a", "x": [0, 1]}], [{"t": sec(1), "m": "a", "x": [0, 1, 2]}]
)


def lifecycle(c):
    db = mk(c)
    ins(db, [d(m="a")])
    out = [mem(c)]
    db.create_collection("ts2", timeseries={"timeField": "t", "metaField": "m"})
    ins(db, [d(m="b")], name="ts2")
    out.append(mem(c))
    db.drop_collection("ts")
    out.append(mem(c))
    ins(db, [{"t": sec(1), "m": "b"}], name="ts2")
    out.append(mem(c))
    db.drop_collection("ts2")
    out.append(mem(c))
    return out


def deletes(c):
    db = mk(c)
    ins(db, [d(m="a"), d(m="b")])
    out = [mem(c)]
    db.command("delete", "ts", deletes=[{"q": {"m": "a"}, "limit": 0}])
    out.append(mem(c))
    ins(db, [{"t": sec(1), "m": "b"}])
    out.append(mem(c))
    ins(db, [{"t": sec(2), "m": "a"}])
    out.append(mem(c))
    return out


CASES["lifecycle drops"] = lifecycle
CASES["deletes then inserts"] = deletes


def main() -> None:
    want = sys.argv[2:] if len(sys.argv) > 2 else None
    version = sys.argv[1]
    for name, fn in CASES.items():
        if want and not any(w in name for w in want):
            continue
        cid, port = start(version)
        try:
            c = pymongo.MongoClient(port=port)
            print(json.dumps({"version": version, "case": name, "got": fn(c)}), flush=True)
            c.close()
        finally:
            stop(cid)


if __name__ == "__main__":
    main()
