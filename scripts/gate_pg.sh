#!/usr/bin/env bash
# The pre-PR gate for a Rust PostgreSQL server change: the PG-scoped subset
# of `./inv rust-gate` plus the suites CI's pg-oracle lane runs, instead of
# the full ~16,600-test pytest suite.
#
#   scripts/gate_pg.sh            # from the worktree root
#   scripts/gate_pg.sh --storage  # also the WT-linked storage crates (the
#                                 # change touched secantus-storage / -wt)
#
# Why not `rust-gate`: its parity step and full suite run the MongoDB-side
# parity files, which CI dropped on 2026-10-20 and which fail locally on
# every branch (63 tests in every fix-one run of 2026-10-31/11-01), so they
# cost ~20-40 min a run and were read past every time. CI still runs the
# whole PR matrix; this gate is what the PR needs to be worth pushing.
#
# Every step runs (a failure does not stop the rest), each step's exit code
# goes to .scratch/gate/<step>.rc, and the log to .scratch/gate/<step>.log.
# The last line printed is `gate=<0|1>`, and the script exits with it.
# Runs for ~35-50 min: start it through scripts/detached_run.py.
set -uo pipefail

storage=0
[[ "${1:-}" == "--storage" ]] && storage=1

root=$(git rev-parse --show-toplevel) || exit 2
cd "$root"
out=.scratch/gate
rm -rf "$out" && mkdir -p "$out"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
py=.venv/bin/python
[[ -x $py ]] || { echo "no worktree venv at $py: run ./inv sync first" >&2; exit 2; }

failed=0
step() {
    local name=$1
    shift
    echo "==> $name: $*"
    local start=$SECONDS
    "$@" >"$out/$name.log" 2>&1
    local rc=$?
    echo "$rc" >"$out/$name.rc"
    echo "    $name=$rc ($((SECONDS - start))s)"
    [[ $rc -eq 0 ]] || failed=1
}

# Rust: the PG crate (fmt, clippy -D warnings, cargo test) and the clean
# workspace, which holds secantus-pgplan and secantus-pgcatalog.
step pgserver-test ./inv rust-pgserver-test
step workspace bash -c 'cd crates && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test -- --test-threads=4'
if [[ $storage -eq 1 ]]; then
    step storage-test ./inv rust-storage-test
fi

# The binary the suites below launch, from HEAD.
step pgbuild ./inv rust-pgserver-build

# What CI's pg-oracle lane runs (.github/workflows/test.yml).
step pg-suites env PYTHONPATH=tests "$py" -m pytest \
    tests/test_rust_pgserver_slice.py tests/test_rust_pgserver_differential.py \
    tests/test_rust_pg_binary_smoke.py tests/test_rust_pg_builtin_coverage.py \
    tests/test_rust_pg_dollar_names.py tests/test_rust_pg_corpora.py \
    tests/test_rust_pg_rule_trees.py \
    -q -n 4 --dist worksteal -rs -p no:cacheprovider

pg15_bin=${SECANTUS_PG15_BIN:-/opt/homebrew/opt/postgresql@15/bin}
step login env SECANTUS_PG15_BIN="$pg15_bin" SECANTUS_REQUIRE_PG15=1 \
    "$py" -m pytest tests/test_rust_pg_login_differential.py -q -n 3 -rs -p no:cacheprovider

# The backlog's gap and regression tests against the PostgreSQL 15 reference.
step backlog env \
    SECANTUS_PG_ORACLE_DSN_15="${SECANTUS_PG_ORACLE_DSN_15:-host=127.0.0.1 port=5415 dbname=postgres user=$USER}" \
    SECANTUS_PG15_BIN="$pg15_bin" \
    "$py" tools/backlog.py verify --file backlog-rust-postgres.md --regressions

step lint ./inv lint
step ruff-check uvx ruff@0.15.20 check .
step ruff-format uvx ruff@0.15.20 format --check .

# A WiredTiger panic in any log is the headline, whatever the exit codes say.
if grep -lE 'WT_PANIC|Bad address' "$out"/*.log 2>/dev/null | grep -v pgbuild; then
    echo "    WT_PANIC / Bad address in the logs above"
    failed=1
fi

echo "gate=$failed"
exit "$failed"
