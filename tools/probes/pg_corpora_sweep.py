"""Run every corpus under ``pg_corpora/`` through ``pg_differential.py`` against
the Rust PostgreSQL server, and print one line per corpus with a total.

``pg_differential.py`` takes one corpus; this is the loop around it, so "how
many divergences are there today" is one command and a number anyone can
reproduce.

Usage::

    python tools/probes/pg_corpora_sweep.py [--out DIR] [--jobs N] [--record DIR] [NAME ...]

**One corpus at a time is the default, and the only run to quote.** The
corpora share one reference database: an event trigger one corpus creates
fires for another's DDL, and two corpora that create the same schema drop it
under each other. Four at once reported 39 divergences on 2026-10-12 where a
serial run reported 13. ``--jobs`` is for a quick first pass; rerun whatever
it reports, by name, without it.

Each corpus's full output is kept in ``--out`` (default: a directory under
the system temporary directory, printed at the end).

``--record DIR`` also writes each corpus's REFERENCE answers to
``DIR/<corpus>.json`` (``pg_differential.py --record``). That is how
``tools/probes/pg_corpora/expected/`` is made, which
``tests/test_rust_pg_corpora.py`` holds the server to in CI, where there is
no PostgreSQL to ask. Record against fresh reference clusters, not the shared
ones: corpora leave roles behind on a cluster they ran on, and a later
corpus's answer can depend on them.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
CORPORA = REPO_ROOT / "tools" / "probes" / "pg_corpora"
PROBE = REPO_ROOT / "tools" / "probes" / "pg_differential.py"


def _run(name: str, out: pathlib.Path, record: pathlib.Path | None = None) -> tuple[str, str, int]:
    """(corpus, divergences or what went wrong, checks)."""
    setup = CORPORA / f"{name}.setup.sql"
    if not setup.exists():
        return name, "NO-SETUP", 0
    extra = [f"--record={record / f'{name}.json'}"] if record is not None else []
    try:
        done = subprocess.run(
            [sys.executable, str(PROBE), str(setup), str(CORPORA / f"{name}.sql")]
            + ["--types", "--rust"]
            + extra,
            capture_output=True,
            text=True,
            timeout=900,
            cwd=REPO_ROOT,
        )
    except subprocess.TimeoutExpired:
        return name, "TIMEOUT", 0
    (out / f"{name}.txt").write_text(done.stdout + "\n--- stderr ---\n" + done.stderr)
    match = re.search(r"=== (\d+) divergences out of (\d+) checks", done.stdout)
    if not match:
        # The probe did not reach its summary: it is not a count of zero.
        return name, "DID-NOT-FINISH", 0
    # Nor is a probe that printed its summary and then died. matviews.sql's
    # did (writing its record), and was counted as a clean corpus.
    if "Traceback (most recent call last)" in done.stderr or (
        record is not None and not (record / f"{name}.json").exists()
    ):
        return name, "DID-NOT-FINISH", 0
    return name, match.group(1), int(match.group(2))


def main(argv: list[str]) -> int:
    jobs = 1
    out = None
    record = None
    names: list[str] = []
    args = iter(argv)
    for arg in args:
        if arg == "--jobs":
            jobs = int(next(args))
        elif arg == "--out":
            out = pathlib.Path(next(args))
        elif arg == "--record":
            record = pathlib.Path(next(args))
            record.mkdir(parents=True, exist_ok=True)
        else:
            names.append(arg)
    if out is None:
        out = pathlib.Path(tempfile.mkdtemp(prefix="secantus-pg-corpora-sweep-"))
    out.mkdir(parents=True, exist_ok=True)
    every = sorted(
        p.name[: -len(".sql")] for p in CORPORA.glob("*.sql") if not p.name.endswith(".setup.sql")
    )
    unknown = sorted(set(names) - set(every))
    if unknown:
        print(f"no such corpus: {', '.join(unknown)}", file=sys.stderr)
        return 2
    chosen = names or every
    with ThreadPoolExecutor(jobs) as pool:
        results = list(pool.map(lambda n: _run(n, out, record), chosen))

    unfinished = [(n, d) for n, d, _ in results if not d.isdigit()]
    counted = [(n, int(d), c) for n, d, c in results if d.isdigit()]
    for name, divergences, checks in sorted(counted, key=lambda r: (-r[1], r[0])):
        if divergences:
            print(f"{divergences:>6} of {checks:>4}  {name}")
    for name, why in unfinished:
        print(f"{why:>14}  {name}")
    total = sum(d for _, d, _ in counted)
    checks = sum(c for _, _, c in counted)
    clean = sum(1 for _, d, _ in counted if d == 0)
    print(
        f"\n=== {total} divergences in {checks} checks; {clean} of {len(chosen)} corpora "
        f"have none ({out})"
    )
    if jobs > 1:
        print(
            f"    run with --jobs {jobs}: corpora interfere on the shared reference. "
            "Rerun the ones listed above by name, one at a time, before quoting this."
        )
    if unfinished:
        print(f"    {len(unfinished)} did not finish, and are in no count above.")
    return 1 if total or unfinished else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
