"""Turn two recordings of the corpora into ``pg_corpora/expected/``.

``pg_corpora_sweep.py --record DIR`` writes PostgreSQL's answers for every
corpus. Run it TWICE, each time on freshly initialised reference clusters, and
give both directories here::

    python tools/probes/pg_corpora_expect.py RECORD_1 RECORD_2

Each corpus's answers go to ``tools/probes/pg_corpora/expected/<corpus>.json``,
which ``tests/test_rust_pg_corpora.py`` holds the Rust PostgreSQL server to in
CI, where there is no PostgreSQL to ask.

A check is marked VOLATILE, and the test then compares only whether it
succeeded and the column types, when

- the two recordings disagree (PostgreSQL itself does not give one answer), or
- the statement reads the clock or the process (``now()``, ``random()``,
  ``pg_backend_pid()`` ...). Two recordings a minute apart agree on
  ``current_date``; the record would still be wrong tomorrow.

Every other check is compared exactly: status, rows, error text and types.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
EXPECTED = REPO_ROOT / "tools" / "probes" / "pg_corpora" / "expected"

#: Statements whose answer depends on when or where they run, not on the data.
CLOCK_OR_PROCESS = re.compile(
    r"\b(now|clock_timestamp|statement_timestamp|transaction_timestamp|timeofday"
    r"|current_date|current_time|current_timestamp|localtime|localtimestamp"
    r"|random|gen_random_uuid|gen_random_bytes|gen_salt|setseed"
    r"|pg_backend_pid|pg_postmaster_start_time|pg_conf_load_time|inet_server_port"
    r"|inet_client_port|inet_client_addr|txid_current|txid_current_if_assigned"
    r"|pg_current_xact_id|pg_current_snapshot|txid_current_snapshot"
    r"|pg_current_wal_lsn|pg_stat_activity|pg_locks|pg_sleep)\b"
    r"|'(now|today|tomorrow|yesterday)'"
    # One-argument age() measures from the current date.
    r"|\bage\s*\(\s*[^,()]*(\([^()]*\)[^,()]*)*\)",
    re.IGNORECASE,
)

#: `pg_timezone_names`' abbreviation, offset and DST flag are each zone's
#: rule in force TODAY: a recording made in one season is not the answer in
#: another (Europe/London is BST in October and GMT in November, and
#: Palestine's zones change on a date set each year).
TODAYS_ZONE_RULES = re.compile(
    r"\bpg_timezone_names\b.*\b(abbrev|utc_offset|is_dst)\b"
    r"|\b(abbrev|utc_offset|is_dst)\b.*\bpg_timezone_names\b"
    r"|select\s+\*\s+from\s+pg_timezone_names\b",
    re.IGNORECASE,
)


def _same(x: list, y: list) -> bool:
    """Two recordings of one check agree. A user type's oid (>= 16384) is the
    cluster's own, as the test treats it: two clusters numbering the same
    composite type differently gave the same answer."""

    def user(oids: list) -> list:
        return ["user" if isinstance(o, int) and o >= 16384 else o for o in oids]

    return x[:4] == y[:4] and user(x[4]) == user(y[4])


def _load(path: pathlib.Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    first, second = (pathlib.Path(a) for a in argv)
    names = sorted(p.stem for p in first.glob("*.json"))
    missing = sorted({p.stem for p in second.glob("*.json")} ^ set(names))
    if missing:
        print(f"recorded in only one run: {', '.join(missing)}", file=sys.stderr)
        return 1
    EXPECTED.mkdir(parents=True, exist_ok=True)
    total = volatile_total = 0
    for name in names:
        a, b = _load(first / f"{name}.json"), _load(second / f"{name}.json")
        if a["reference"] != b["reference"] or [c[:2] for c in a["checks"]] != [
            c[:2] for c in b["checks"]
        ]:
            print(f"{name}: the two recordings are not the same statements", file=sys.stderr)
            return 1
        volatile: dict[str, str] = {}
        for i, (x, y) in enumerate(zip(a["checks"], b["checks"], strict=True)):
            if not _same(x, y):
                volatile[str(i)] = "PostgreSQL answered differently in two recordings"
            elif CLOCK_OR_PROCESS.search(x[0]):
                volatile[str(i)] = "reads the clock or the server process"
            elif TODAYS_ZONE_RULES.search(x[0]):
                volatile[str(i)] = "reads the time zone rules in force today"
        lines = ",\n".join(json.dumps(c, ensure_ascii=False) for c in a["checks"])
        (EXPECTED / f"{name}.json").write_text(
            f'{{"reference": {json.dumps(a["reference"])},\n'
            f'"volatile": {json.dumps(volatile, ensure_ascii=False)},\n'
            f'"checks": [\n{lines}\n]}}\n',
            encoding="utf-8",
        )
        total += len(a["checks"])
        volatile_total += len(volatile)
    print(
        f"wrote {len(names)} corpora, {total} checks ({volatile_total} volatile) to "
        f"{EXPECTED.relative_to(REPO_ROOT)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
