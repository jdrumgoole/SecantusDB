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
# Runs for ~35-50 min (~12-15 with --fast): start it through
# scripts/detached_run.py.
#
#   scripts/gate_pg.sh --fast     # leave pg-suites to CI (integration mode)
#   scripts/gate_pg.sh --only fmt,lint   # rerun just these steps
#   scripts/gate_pg.sh --list     # print the steps a run would take, and stop
#
# The cheap checks (fmt, lint, ruff, backlog-ids) run FIRST and stop the gate when one
# fails: a session reran a whole 40-min gate for one `cargo fmt` slip on
# 2026-11-06. Every other step still runs whatever the others did.
#
# --fast skips pg-suites (20-27 min of a 40-min gate, measured over four
# fix-one sessions on 2026-11-06). Use it ONLY for a fix that lands on an
# integration branch (fix-one "Integration mode"): CI's pg-oracle job runs
# that same list on the branch's PR to main, before anything reaches main.
# The skip is printed, so a log shows the gate did not run it.
#
# --only reruns named steps after a fix that only they check (a format or
# lint slip). After any other code change, rerun the whole gate. Each step
# writes the commit it ran on to .scratch/gate/<step>.sha, and the summary
# table flags a result from an older commit.
set -uo pipefail

storage=0 fast=0 list=0 only=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage) storage=1 ;;
        --fast) fast=1 ;;
        --list) list=1 ;;
        --only) only=",$2,"; shift ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

root=$(git rev-parse --show-toplevel) || exit 2
cd "$root"
out=.scratch/gate
if [[ -z $only && $list -eq 0 ]]; then
    rm -rf "$out"
fi
mkdir -p "$out"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
py=.venv/bin/python
[[ $list -eq 1 || -x $py ]] || { echo "no worktree venv at $py: run ./inv sync first" >&2; exit 2; }
head=$(git rev-parse --short HEAD)

failed=0
ran=()
step() {
    local name=$1
    shift
    if [[ -n $only && $only != *",$name,"* ]]; then
        return 0
    fi
    if [[ $list -eq 1 ]]; then
        echo "$name: $*"
        return 0
    fi
    echo "==> $name: $*"
    # Each step waits its turn on a busy machine (another lane's gate, another
    # session's build): a timing test run at load 68 fails for no reason in
    # the change. A gate that waited says so in its output.
    scripts/wait_for_load.sh "gate $name"
    local start=$SECONDS
    "$@" >"$out/$name.log" 2>&1
    local rc=$?
    echo "$rc" >"$out/$name.rc"
    echo "$head" >"$out/$name.sha"
    echo "    $name=$rc ($((SECONDS - start))s)"
    ran+=("$name")
    [[ $rc -eq 0 ]] || failed=1
}

summary() {
    [[ $list -eq 1 ]] && exit 0
    # A WiredTiger panic in any log is the headline, whatever the exit codes say.
    if grep -lE 'WT_PANIC|Bad address' "$out"/*.log 2>/dev/null | grep -v pgbuild; then
        echo "    WT_PANIC / Bad address in the logs above"
        failed=1
    fi
    if [[ -n $only ]]; then
        echo "    every step on record (--only reran: ${ran[*]:-nothing}):"
        for f in "$out"/*.rc; do
            local n sha
            n=$(basename "$f" .rc)
            sha=$(cat "$out/$n.sha" 2>/dev/null)
            echo "      $n=$(cat "$f") at ${sha:-?}$([[ $sha != "$head" ]] && echo "  (older commit than HEAD $head)")"
        done
    fi
    echo "gate=$failed"
    exit "$failed"
}

# The cheap checks first; any failure here stops the gate.
step fmt bash -c 'cd crates/secantus-pg && cargo fmt --check && cd .. && cargo fmt --check'
step lint ./inv lint
step ruff-check uvx ruff@0.15.20 check .
step ruff-format uvx ruff@0.15.20 format --check .
# Every Backlog-ID this branch adds is its own reservation on origin (or an
# item it inherited from another branch there). A hand-numbered ID that
# another lane had reserved passed this gate and went red in CI a cycle
# later on snapshot PR #2580 (2026-11-09).
step backlog-ids "$py" tools/backlog.py check-ids --file backlog-rust-postgres.md
if [[ $failed -ne 0 ]]; then
    echo "    a cheap check failed: fix it and rerun the gate (nothing slower ran)"
    summary
fi

# Rust: the PG crate (fmt, clippy -D warnings, cargo test) and the clean
# workspace, which holds secantus-pgplan and secantus-pgcatalog.
step pgserver-test ./inv rust-pgserver-test
step workspace bash -c 'cd crates && cargo clippy --all-targets -- -D warnings && cargo test -- --test-threads=4'
if [[ $storage -eq 1 ]]; then
    step storage-test ./inv rust-storage-test
fi

# The binary the suites below launch, from HEAD.
step pgbuild ./inv rust-pgserver-build

# What CI's pg-oracle lane runs (.github/workflows/test.yml).
if [[ $fast -eq 1 ]]; then
    if [[ -z $only || $only == *",pg-suites,"* ]]; then
        echo "    pg-suites SKIPPED (--fast): CI's pg-oracle job runs it on the integration branch's PR"
    fi
else
    step pg-suites env PYTHONPATH=tests "$py" -m pytest \
        tests/test_rust_pgserver_slice.py tests/test_rust_pgserver_differential.py \
        tests/test_rust_pg_binary_smoke.py tests/test_rust_pg_builtin_coverage.py \
        tests/test_rust_pg_dollar_names.py tests/test_rust_pg_corpora.py \
        tests/test_rust_pg_rule_trees.py \
        -q -n 4 --dist worksteal -rs -p no:cacheprovider
fi

pg15_bin=${SECANTUS_PG15_BIN:-/opt/homebrew/opt/postgresql@15/bin}
step login env SECANTUS_PG15_BIN="$pg15_bin" SECANTUS_REQUIRE_PG15=1 \
    "$py" -m pytest tests/test_rust_pg_login_differential.py -q -n 3 -rs -p no:cacheprovider

# The backlog's gap and regression tests against the PostgreSQL 15 reference.
# Four workers, as the other pytest steps use: each starts its own server,
# and -n auto's twelve held the load at 54 on 2026-11-03 beside another lane.
step backlog env \
    SECANTUS_BACKLOG_WORKERS=4 \
    SECANTUS_PG_ORACLE_DSN_15="${SECANTUS_PG_ORACLE_DSN_15:-host=127.0.0.1 port=5415 dbname=postgres user=$USER}" \
    SECANTUS_PG15_BIN="$pg15_bin" \
    "$py" tools/backlog.py verify --file backlog-rust-postgres.md --regressions

summary
