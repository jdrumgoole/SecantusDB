#!/usr/bin/env python3
"""Does a change need CI's pg-oracle lanes? Prints `true` or `false`.

    git diff --name-only BASE...HEAD | python3 tools/ci_pg_needed.py

pg-oracle (three shards, ~30 min, the slowest job of a PR run) checks the
Rust PostgreSQL server against a real PostgreSQL. A change confined to code
and tests the PostgreSQL server does not link -- the MongoDB server's command
layer, its binary, its storage adapter, its wire layer, the MongoDB backlog
and its tests -- cannot change those answers, so its lanes can be skipped.

The test is an ALLOW-list of MongoDB-only paths, not a list of PostgreSQL
paths: a file this list does not name -- a shared crate, a lockfile, the
workflow, a new directory -- runs pg-oracle. An empty change runs it too.
Pushes to main and the weekly run always run it (the workflow decides that).
Stdlib only: CI calls it with the runner's own `python3`.
"""

from __future__ import annotations

import sys

#: Paths the Rust PostgreSQL server neither links nor tests. Checked against
#: `crates/secantus-pg/Cargo.toml` on 2026-11-09: it links secantus-core,
#: -storage, -auth, -pgplan, -pgcatalog and -pgwire, none of these.
MONGO_ONLY = (
    "crates/secantus-commands/",
    "crates/secantusdb/",
    "crates/secantus-storage-adapter/",
    "crates/secantus-wire/",
    "tests/backlog/test_rm_",
    "tests/backlog/opdebug.py",
    "tests/mongod_live/",
    "tasks/backlog-rust-mongodb.md",
    "changelog.d/",
    "scripts/gate_rm.py",
    "scripts/fix_many.sh",
    "scripts/fix_one.sh",
    "scripts/land_batch.sh",
)


def pg_needed(files: list[str]) -> bool:
    """True unless every changed file is MongoDB-only."""
    files = [f for f in files if f.strip()]
    return not files or not all(f.startswith(MONGO_ONLY) for f in files)


def main() -> int:
    files = sys.argv[1:] or sys.stdin.read().splitlines()
    print("true" if pg_needed(files) else "false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
