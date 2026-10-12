"""Call every built-in function PostgreSQL 15 has, on PostgreSQL and on the
Rust PostgreSQL server, and report where the answers differ.

The corpora under ``pg_corpora/`` measure the functions somebody thought to
write a line for. This measures the rest: it reads ``pg_proc`` on the
reference server, builds one call per function from a sample value for each
argument type, and runs the same call on both servers.

Only IMMUTABLE and STABLE functions are called. A function that changes
something (``pg_terminate_backend``, ``lo_unlink``, ``set_config``) is
VOLATILE, so the filter is also what makes this safe to run against a
reference server other sessions share.

Usage::

    python tools/probes/pg_builtin_coverage.py [--json OUT] [--only NAME ...]

``SECANTUS_PG_ORACLE_DSN_15`` names the reference (default: port 5415).

What it reports, most useful first:

``MISSING``     PostgreSQL answers (a value, or an error of its own about the
                sample) and the Rust server says the function does not exist
                (42883) or is not supported (0A000). Each is labelled with
                what in the catalog refers to the function -- an operator, a
                type's input or output, an index, an aggregate, a cast --
                and the summary counts them apart from the functions nothing
                refers to, which are the ones a statement calls by name.
``CRASH``       the Rust server dropped the connection.
``SHOULD-ERROR`` / ``SPURIOUS-ERROR`` / ``ERRDIFF`` / ``VALUE`` / ``TYPE``
                as ``pg_differential.py`` names them.

A sample is one value per type, so a ``VALUE`` line is a lead and an agreement
is weak evidence: this finds the function nobody implemented, and a corpus is
still what pins one that somebody did. Functions that read the server's own
state (``pg_stat_*``, sizes, backend ids) differ by construction and are left
out by name; the list is `STATE`, each with its reason.

The run checks itself first: ``abs(-2)`` and ``upper('abc')`` must agree and
``secantus_no_such_function(1)`` must be 42883 on both, or nothing is reported.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

import psycopg

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from tools.provenance import PGSERVER_REL, require_fresh_pgserver, resolve_binary  # noqa: E402

REFERENCE = os.environ.get(
    "SECANTUS_PG_ORACLE_DSN_15", "host=127.0.0.1 port=5415 dbname=postgres user=postgres"
)

#: One sample per argument type, as SQL. A type with no sample leaves its
#: functions uncalled, and the run says how many.
SAMPLES: dict[str, str] = {
    "int2": "3::int2",
    "int4": "2::int4",
    "int8": "5::int8",
    "numeric": "1.5::numeric",
    "float4": "1.5::float4",
    "float8": "2.5::float8",
    "text": "'abc'::text",
    "varchar": "'abc'::varchar",
    "bpchar": "'abc'::bpchar",
    "name": "'abc'::name",
    "char": "'a'::\"char\"",
    "bool": "true",
    "date": "'2020-02-03'::date",
    "time": "'04:05:06'::time",
    "timetz": "'04:05:06+00'::timetz",
    "timestamp": "'2020-02-03 04:05:06'::timestamp",
    "timestamptz": "'2020-02-03 04:05:06+00'::timestamptz",
    "interval": "'1 day 02:03:04'::interval",
    "bytea": "'\\x616263'::bytea",
    "json": "'{\"a\":[1,2]}'::json",
    "jsonb": "'{\"a\":[1,2]}'::jsonb",
    "jsonpath": "'$.a'::jsonpath",
    "uuid": "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid",
    "inet": "'192.168.1.5/24'::inet",
    "cidr": "'192.168.1.0/24'::cidr",
    "macaddr": "'08:00:2b:01:02:03'::macaddr",
    "macaddr8": "'08:00:2b:01:02:03:04:05'::macaddr8",
    "bit": "B'101'",
    "varbit": "B'101'::varbit",
    "money": "'1.50'::money",
    "oid": "1::oid",
    "regclass": "'pg_class'::regclass",
    "regtype": "'int4'::regtype",
    "regconfig": "'english'::regconfig",
    "tsvector": "'a b'::tsvector",
    "tsquery": "'a & b'::tsquery",
    "point": "'(1,2)'::point",
    "box": "'((0,0),(1,1))'::box",
    "lseg": "'[(0,0),(1,1)]'::lseg",
    "line": "'{1,-1,0}'::line",
    "path": "'((0,0),(1,1),(2,0))'::path",
    "polygon": "'((0,0),(1,1),(2,0))'::polygon",
    "circle": "'<(0,0),1>'::circle",
    "int4range": "'[1,5)'::int4range",
    "int8range": "'[1,5)'::int8range",
    "numrange": "'[1.5,5)'::numrange",
    "daterange": "'[2020-01-01,2020-02-01)'::daterange",
    "tsrange": "'[2020-01-01 00:00,2020-02-01 00:00)'::tsrange",
    "tstzrange": "'[2020-01-01 00:00+00,2020-02-01 00:00+00)'::tstzrange",
    "int4multirange": "'{[1,5)}'::int4multirange",
    "int8multirange": "'{[1,5)}'::int8multirange",
    "nummultirange": "'{[1.5,5)}'::nummultirange",
    "datemultirange": "'{[2020-01-01,2020-02-01)}'::datemultirange",
    "pg_lsn": "'0/16'::pg_lsn",
    "xid": "'1'::xid",
    "xid8": "'1'::xid8",
    "tid": "'(0,1)'::tid",
    "_int2": "'{1,2,3}'::int2[]",
    "_int4": "'{1,2,3}'::int4[]",
    "_int8": "'{1,2,3}'::int8[]",
    "_numeric": "'{1.5,2.5}'::numeric[]",
    "_float4": "'{1.5,2.5}'::float4[]",
    "_float8": "'{1.5,2.5}'::float8[]",
    "_text": "'{a,b}'::text[]",
    "_bool": "'{t,f}'::bool[]",
    "_oid": "'{1,2}'::oid[]",
    "_name": "'{a,b}'::name[]",
    "_char": "'{a,b}'::\"char\"[]",
    "_json": "ARRAY['1'::json]",
    "_jsonb": "ARRAY['1'::jsonb]",
    "_tsvector": "ARRAY['a b'::tsvector]",
    "_interval": "ARRAY['1 day'::interval]",
    "_bytea": "ARRAY['\\x61'::bytea]",
    # The polymorphic types, all of one family so a signature that ties them
    # together (anyarray with anyelement) is consistent.
    "any": "2::int4",
    "anyelement": "2::int4",
    "anynonarray": "2::int4",
    "anycompatible": "2::int4",
    "anycompatiblenonarray": "2::int4",
    "anyarray": "'{1,2,3}'::int4[]",
    "anycompatiblearray": "'{1,2,3}'::int4[]",
    "anyrange": "'[1,5)'::int4range",
    "anycompatiblerange": "'[1,5)'::int4range",
    "anymultirange": "'{[1,5)}'::int4multirange",
    "anycompatiblemultirange": "'{[1,5)}'::int4multirange",
    "anyenum": None,  # needs a user type
}

#: Functions whose answer is the server's own state, by name pattern. They are
#: called on neither server: two servers differ on them by construction.
STATE: list[tuple[str, str]] = [
    (r"^pg_stat_get_|^pg_stat_", "run-time statistics"),
    (r"_size$|_size_pretty$|^pg_relation_filenode$|^pg_relation_filepath$", "on-disk layout"),
    (r"^pg_backend_pid$|^pg_my_temp_schema$|^pg_postmaster_start_time$", "process identity"),
    (r"^pg_conf_load_time$|^pg_current_logfile$|^pg_is_in_recovery$", "process state"),
    (r"^txid_|^pg_current_xact_id|^pg_current_snapshot|^pg_snapshot_|^pg_xact_", "live xids"),
    (r"^pg_last_|^pg_wal|^pg_walfile|^pg_current_wal|^pg_is_wal", "the WAL, which is absent"),
    (r"^inet_client_|^inet_server_", "the connection's addresses"),
    (r"^now$|^transaction_timestamp$|^statement_timestamp$|^current_", "the clock or session"),
    (r"^pg_tablespace_|^pg_database_|^pg_total_|^pg_table_|^pg_indexes_", "on-disk layout"),
    (r"^pg_get_keywords$|^pg_available_|^pg_timezone_|^pg_options_to_table$", "host tables"),
    (r"^pg_show_all_|^pg_hba_file_rules$|^pg_config$|^pg_control_", "host configuration"),
    (r"^pg_lock_status$|^pg_prepared_|^pg_cursor$|^pg_listening_channels$", "session state"),
    (r"^pg_replication_|^pg_get_replication|^pg_show_replication", "replication, out of scope"),
    (r"^pg_stat_file$|^pg_read_|^pg_ls_", "the host file system"),
    (r"^getdatabaseencoding$|^pg_client_encoding$|^version$|^pg_encoding_", "build identity"),
    (r"^to_reg|^pg_get_|^pg_describe_object$|^pg_identify_object", "this cluster's catalog oids"),
    (r"^has_.*_privilege$|^pg_has_role$|^row_security_active$", "this cluster's roles"),
    (r"^obj_description$|^col_description$|^shobj_description$", "this cluster's catalog oids"),
    (r"^pg_.*_is_visible$|^format_type$|^pg_typeof$|^pg_collation_", "this cluster's catalog oids"),
    (r"^binary_upgrade_|^pg_partition_|^pg_sequence_|^pg_mcv_list_items$", "internal"),
    (r"^pg_event_trigger_|^pg_extension_|^pg_safe_snapshot|^pg_isolation_test", "internal"),
    (r"^pg_nextoid$|^pg_filenode_relation$|^pg_index_|^pg_indexam_", "this cluster's catalog oids"),
    (r"^pg_tablespace_location$|^pg_column_|^pg_trigger_depth$|^pg_jit_", "internal"),
    (r"^amvalidate$|^satisfies_hash_partition$|^pg_opfamily_|^pg_operator_", "internal"),
    (r"^pg_blocking_pids$|^pg_logical_|^pg_export_|^pg_notification_", "session state"),
]

_UNSUPPORTED = ("42883", "0A000")


def _state_reason(name: str) -> str | None:
    for pattern, reason in STATE:
        if re.search(pattern, name):
            return reason
    return None


class _RustServer:
    """`secantusd-pg` on a store of its own, restarted if it goes away."""

    def __init__(self) -> None:
        self.binary = resolve_binary(REPO_ROOT / PGSERVER_REL)
        if not self.binary.exists():
            raise SystemExit(f"{self.binary} is not built -- cd crates/secantus-pg && cargo build")
        require_fresh_pgserver(self.binary)
        self.store_dir = tempfile.mkdtemp(prefix=f"secantus-probe-{os.getpid()}-pg-builtin-")
        self.proc: subprocess.Popen | None = None
        self.port = 0
        self.start()

    def start(self) -> None:
        self.proc = subprocess.Popen(
            [str(self.binary), self.store_dir, "127.0.0.1:0"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        line = self.proc.stdout.readline() if self.proc.stdout else ""
        match = re.search(r"listening on \S+:(\d+)", line)
        if not match:
            raise SystemExit(f"secantusd-pg did not start: {line!r}")
        self.port = int(match.group(1))

    def alive(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def connect(self) -> psycopg.Connection:
        return _session(f"host=127.0.0.1 port={self.port} dbname=postgres user=postgres")

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        shutil.rmtree(self.store_dir, ignore_errors=True)


def _session(dsn: str) -> psycopg.Connection:
    conn = psycopg.connect(dsn, autocommit=True, connect_timeout=10)
    # The settings an answer's TEXT depends on, the same on both.
    for setting in (
        "SET timezone = 'UTC'",
        "SET datestyle = 'ISO, MDY'",
        "SET intervalstyle = 'postgres'",
        "SET extra_float_digits = 1",
        "SET bytea_output = 'hex'",
        "SET statement_timeout = '5s'",
    ):
        conn.execute(setting)
    return conn


def _functions(ref: psycopg.Connection) -> list[dict]:
    rows = ref.execute(
        """
        SELECT p.oid::int, p.proname, p.prokind, p.proretset, p.provariadic::int,
               (SELECT array_agg(t.typname ORDER BY a.ord)
                  FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY a(typ, ord)
                  JOIN pg_type t ON t.oid = a.typ) AS argtypes,
               coalesce(ag.aggkind::text, '') AS aggkind,
               pg_get_function_identity_arguments(p.oid) AS ident,
               CASE
                 WHEN EXISTS (SELECT 1 FROM pg_operator o WHERE o.oprcode = p.oid)
                   THEN 'operator'
                 WHEN EXISTS (SELECT 1 FROM pg_type t WHERE p.oid IN (
                        t.typinput, t.typoutput, t.typreceive, t.typsend, t.typmodin,
                        t.typmodout, t.typanalyze, t.typsubscript))
                   THEN 'type input/output'
                 WHEN EXISTS (SELECT 1 FROM pg_amproc a WHERE a.amproc = p.oid)
                   THEN 'index support'
                 WHEN EXISTS (SELECT 1 FROM pg_aggregate a WHERE p.oid IN (
                        a.aggtransfn, a.aggfinalfn, a.aggcombinefn, a.aggserialfn,
                        a.aggdeserialfn, a.aggmtransfn, a.aggminvtransfn, a.aggmfinalfn))
                   THEN 'aggregate support'
                 WHEN EXISTS (SELECT 1 FROM pg_cast c WHERE c.castfunc = p.oid)
                   THEN 'cast'
                 WHEN EXISTS (SELECT 1 FROM pg_operator o WHERE p.oid IN (o.oprrest, o.oprjoin))
                   THEN 'selectivity'
                 ELSE ''
               END AS role
          FROM pg_proc p
          JOIN pg_namespace n ON n.oid = p.pronamespace
          LEFT JOIN pg_aggregate ag ON ag.aggfnoid = p.oid
         WHERE n.nspname = 'pg_catalog'
           AND p.prokind IN ('f', 'a', 'w')
           AND p.provolatile IN ('i', 's')
         ORDER BY p.proname, p.oid
        """
    ).fetchall()
    keys = ("oid", "name", "kind", "retset", "variadic", "argtypes", "aggkind", "ident", "role")
    return [dict(zip(keys, r, strict=True)) for r in rows]


def _call(fn: dict) -> str | None:
    """The SQL that calls `fn` with a sample per argument, or None if a type
    has no sample."""
    args = []
    argtypes = fn["argtypes"] or []
    for i, typ in enumerate(argtypes):
        sample = SAMPLES.get(typ)
        if sample is None:
            return None
        if fn["variadic"] and i == len(argtypes) - 1 and typ != "any":
            sample = f"VARIADIC {sample}"
        args.append(sample)
    call = f'pg_catalog."{fn["name"]}"({", ".join(args)})'
    if fn["kind"] == "a":
        if fn["aggkind"] != "n":
            return None  # ordered-set and hypothetical-set: WITHIN GROUP, a corpus's job
        return f"SELECT {call} FROM (VALUES (1), (2)) v(i)"
    if fn["kind"] == "w":
        return f"SELECT {call} OVER (ORDER BY i) FROM (VALUES (1), (2)) v(i)"
    return f"SELECT {call}"


def _run(conn: psycopg.Connection, sql: str) -> tuple:
    """('ok', repr(rows), type oids) | ('err', sqlstate, message) | ('lost', ...)."""
    try:
        cur = conn.execute(sql)
        rows = cur.fetchall() if cur.description else []
        oids = tuple(c.type_code for c in cur.description or ())
        return ("ok", repr(rows), oids)
    except psycopg.OperationalError as exc:
        if conn.closed or conn.broken:
            return ("lost", type(exc).__name__, str(exc).strip().splitlines()[0:1])
        return ("err", exc.sqlstate, str(exc).strip().splitlines()[0])
    except psycopg.Error as exc:
        if exc.sqlstate is None:
            # Nothing the server said: the client could not read the answer.
            return ("client", type(exc).__name__, str(exc).strip().splitlines()[0])
        return ("err", exc.sqlstate, str(exc).strip().splitlines()[0])


def _classify(want: tuple, got: tuple) -> str | None:
    if got[0] == "lost":
        return "CRASH"
    if want[0] == "client" or got[0] == "client":
        return None if want[:2] == got[:2] else "CLIENT"
    if got[0] == "err" and got[1] in _UNSUPPORTED and not (want[0] == "err" and want[1] == got[1]):
        return "MISSING"
    if want[0] == "err" and got[0] == "err":
        return None if want[1:] == got[1:] else "ERRDIFF"
    if want[0] == "err":
        return "SHOULD-ERROR"
    if got[0] == "err":
        return "SPURIOUS-ERROR"
    if want[1] != got[1]:
        return "VALUE"
    # A user type's oid is each server's own.
    norm = lambda oids: tuple("user" if o >= 16384 else o for o in oids)  # noqa: E731
    return None if norm(want[2]) == norm(got[2]) else "TYPE"


def _self_check(ref: psycopg.Connection, sec: psycopg.Connection) -> None:
    for sql in ("SELECT pg_catalog.abs(-2::int4)", "SELECT pg_catalog.upper('abc'::text)"):
        want, got = _run(ref, sql), _run(sec, sql)
        if want[0] != "ok" or _classify(want, got) is not None:
            raise SystemExit(f"self-check failed on {sql}: reference {want}, server {got}")
    sql = "SELECT secantus_no_such_function(1)"
    want, got = _run(ref, sql), _run(sec, sql)
    if (want[0], want[1], got[0], got[1]) != ("err", "42883", "err", "42883"):
        raise SystemExit(f"self-check failed on {sql}: reference {want}, server {got}")
    major = int(ref.execute("SHOW server_version_num").fetchone()[0]) // 10000
    if major != 15:
        raise SystemExit(f"the reference at {REFERENCE} is PostgreSQL {major}, not 15")


def main(argv: list[str]) -> int:
    out = None
    if "--json" in argv:
        out = argv[argv.index("--json") + 1]
    only = set(argv[argv.index("--only") + 1 :]) if "--only" in argv else None

    ref = _session(REFERENCE)
    server = _RustServer()
    try:
        sec = server.connect()
        _self_check(ref, sec)
        functions = _functions(ref)
        counts: dict[str, int] = {}
        findings: list[dict] = []
        called = no_sample = state = 0
        for fn in functions:
            if only is not None and fn["name"] not in only:
                continue
            if _state_reason(fn["name"]) is not None:
                state += 1
                continue
            sql = _call(fn)
            if sql is None:
                no_sample += 1
                continue
            called += 1
            want = _run(ref, sql)
            if want[0] == "lost":
                raise SystemExit(f"the REFERENCE dropped the connection on: {sql}")
            got = _run(sec, sql)
            kind = _classify(want, got)
            if got[0] == "lost":
                if not server.alive():
                    kind = "CRASH (the server process died)"
                    server.start()
                sec = server.connect()
            if kind is None:
                continue
            counts[kind] = counts.get(kind, 0) + 1
            findings.append(
                {"kind": kind, "name": fn["name"], "ident": fn["ident"], "sql": sql}
                | {"role": fn["role"]}
                | {"pg": list(want), "sec": list(got)}
            )
        order = ["CRASH", "MISSING", "SHOULD-ERROR", "SPURIOUS-ERROR", "VALUE", "TYPE", "ERRDIFF"]

        def rank(f: dict) -> tuple:
            first = next((i for i, k in enumerate(order) if f["kind"].startswith(k)), len(order))
            return (first, f["name"])

        findings.sort(key=rank)
        for f in findings:
            role = f" [{f['role']}]" if f["role"] else ""
            print(f"--- {f['kind']}{role}  {f['name']}({f['ident']})")
            print(f"  SQL: {f['sql']}\n   PG: {f['pg'][1:]}\n  SEC: {f['sec'][1:]}")
        differing = sum(counts.values())
        print(
            f"\n=== {called} functions called, {called - differing} the same, "
            f"{differing} differ: {dict(sorted(counts.items()))}"
        )
        # A function nothing but an operator, a type or an index refers to is
        # seldom called by its name; one with no such role is what a
        # statement is written with.
        roles: dict[str, int] = {}
        for f in findings:
            if f["kind"] == "MISSING":
                key = f["role"] or "called by name"
                roles[key] = roles.get(key, 0) + 1
        if roles:
            print(f"    MISSING by what refers to the function: {dict(sorted(roles.items()))}")
        print(
            f"    not called: {state} that read the server's own state, "
            f"{no_sample} with an argument type that has no sample "
            f"(of {len(functions)} immutable or stable functions in pg_catalog)"
        )
        if out:
            pathlib.Path(out).write_text(json.dumps(findings, indent=1))
        return 1 if differing else 0
    finally:
        ref.close()
        server.stop()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
