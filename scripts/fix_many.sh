#!/usr/bin/env bash
# Fix several backlog items, each in its own fresh headless session
# (scripts/fix_one.sh -p), with up to two (or three, see below) at once.
#
#   scripts/fix_many.sh postgres 10        # ten items, two lanes
#   scripts/fix_many.sh -j 1 postgres 4    # one at a time
#   scripts/fix_many.sh -i pg-batch postgres 8   # land on pg-batch, not main
#   scripts/fix_many.sh -l ids.txt -i rm-batch mongodb 8  # item n works line n's ID
#
# -l FILE: one backlog ID per line (blank lines and `#` comments skipped),
# picked beforehand -- e.g. from a `tools/backlog.py verify` run, with the
# Decision-needed and `Verify: none` items left out -- instead of each
# session re-reading the whole backlog to choose. Past the end of the list,
# sessions pick for themselves.
#
# In integration mode a session that exits 0 without `landed on <branch> at
# <sha>` in its report, with that sha on origin/<branch>, stops the batch as
# a failure does: on 2026-11-06 three sessions were refused their worktrees
# and exited 0 having done nothing.
#
# -i BRANCH (integration mode): each session gates locally and pushes its
# fix to BRANCH, with no PR and no CI wait (fix-one skill, "Integration
# mode"). BRANCH is created from origin/main if it does not exist. At the
# end nothing is opened here: run scripts/land_batch.sh BRANCH <this pid>
# beside the batch. It lands BRANCH on main in snapshot PRs, each merged
# after green CI.
#
# In integration mode each lane keeps ONE worktree, ../SecantusDB-lane-<BRANCH>-<k>
# (named for the branch, so a PG batch and a MongoDB batch never share one),
# for every item it works: the session switches it to a new branch off
# BRANCH, so its cargo target and venv stay warm (builds go incremental, and
# a fresh 4.3 GB worktree's 2-11 min build and slow teardown go away). The
# lane worktrees outlive the batch for the next one; remove them at the end
# of the session (`git worktree remove --force`, in the background).
#
# Two lanes by default because CLAUDE.md allows two building agents per
# session (four machine-wide). -j 3 only when this is the machine's one
# session (Joe, 2026-11-06), and with `gate_pg.sh --fast` gates, which
# integration mode uses. The lanes cannot take the same item: fix-one's
# claim is a pushed branch, checked before work starts. Each lane also waits
# for the load average to fall under SECANTUS_LOAD_CEILING (default 10)
# before it starts an item.
#
# Each session's report goes to .scratch/fix_many/<n>.log, and one line per
# session (its exit code, plus the PR it merged if any) to
# .scratch/fix_many/summary.txt. Start it through scripts/detached_run.py.
#
# The first session to exit non-zero stops the batch: no new session starts
# (one already running in the other lane finishes). On 2026-11-05 an expired
# login failed all 23 sessions in under a minute. Read that session's log,
# fix the cause, and start a new batch.
set -uo pipefail

jobs=2
into=
list=
while [[ "${1:-}" == -* ]]; do
    case "$1" in
        -j) jobs=$2 ;;
        -i) into=$2 ;;
        -l) list=$(cd "$(dirname "$2")" && pwd)/$(basename "$2") ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift 2
done
backlog=${1:?usage: $0 [-j N] [-i BRANCH] [-l IDS] <postgres|mongodb> <count>}
count=${2:?usage: $0 [-j N] [-i BRANCH] [-l IDS] <postgres|mongodb> <count>}

cd "$(dirname "$0")/.."
# Keep the Mac awake while this runs. An idle Mac on AC power drops into
# "Maintenance Sleep" for 10-20 minutes at a time, and every process stops
# with it: on 2026-11-08 a lander's `sleep 300` came back after 63 minutes
# and the fix-one sessions beside it stalled the same way (RM-443, from
# `pmset -g log`). `caffeinate -i -w $$` holds a no-idle-sleep assertion
# until this script exits, and does nothing where it is not installed.
if command -v caffeinate >/dev/null 2>&1; then
    caffeinate -i -w $$ &
fi
out=.scratch/fix_many
mkdir -p "$out"
rm -f "$out/STOP"
# summary.txt is appended across batches: mark where this one starts.
echo "$(date '+%F %T') START pid $$: $backlog x$count${into:+ into $into}${list:+ from $list}" >>"$out/summary.txt"

if [[ -n $into ]]; then
    [[ $into == main ]] && { echo "-i main: use the default mode" >&2; exit 2; }
    git fetch -q origin
    if ! git ls-remote --exit-code --heads origin "$into" >/dev/null; then
        git push -q origin "origin/main:refs/heads/$into" || exit 1
    elif git merge-base --is-ancestor "origin/$into" origin/main; then
        # Everything on it has landed: start the batch from main, or the lanes
        # base on the last batch's tip and miss what main gained since (batch
        # 13, 2026-11-08: the Backlog-ID reservation in tools/backlog.py).
        # A fast-forward, so a branch with unlanded work is never touched.
        git push -q origin "origin/main:refs/heads/$into" || exit 1
    fi
fi

# Hand out item numbers 1..count across the lanes (mkdir is the lock).
echo 0 >"$out/next"
next_item() {
    until mkdir "$out/.lock" 2>/dev/null; do sleep 1; done
    local n=$(($(cat "$out/next") + 1))
    echo "$n" >"$out/next"
    rmdir "$out/.lock"
    echo "$n"
}

lane() {
    local k=$1 wt=
    # Stagger the lanes so the second does not pick while the first is
    # claiming the same item.
    sleep $(((k - 1) * 120))
    if [[ -n $into ]]; then
        wt=$(cd .. && pwd)/SecantusDB-lane-$into-$k
        if [[ ! -d $wt ]]; then
            git worktree add -q --detach "$wt" "origin/$into" || {
                echo "$(date '+%F %T') lane $k: could not create $wt" >>"$out/summary.txt"
                echo "lane$k" >"$out/STOP"
                return 1
            }
        fi
    fi
    while :; do
        [[ -e $out/STOP ]] && return 0
        local n
        n=$(next_item)
        [[ $n -gt $count ]] && return 0
        # Start no session while the machine is already over budget: the other
        # lane, a leftover gate or another session may be building
        # (scripts/wait_for_load.sh has the 2026-11-02 numbers).
        scripts/wait_for_load.sh "fix_many item $n" 2>>"$out/summary.txt"
        [[ -e $out/STOP ]] && return 0
        # A reused lane's cargo targets grow with every item: cargo keeps
        # each build's incremental state and never removes a stale one. On
        # 2026-11-07 two lanes reached 41 and 50 GB after about six items
        # each (36 GB of it incremental) and filled the disk mid-gate. The
        # incremental state is a cache: clear it before each item (the
        # compiled dependencies stay, so the next build is not cold), and
        # stop the batch rather than start an item on a disk below the floor.
        if [[ -n $wt ]]; then
            rm -rf "$wt"/crates/target/*/incremental "$wt"/crates/*/target/*/incremental
            # cargo also keeps every superseded compiled dependency in
            # target/*/deps, so a lane still grows item by item (lane 1 of
            # the 2026-11-08 batch reached 43 GB after ~8 items). Past the
            # cap, wipe the lane's cargo targets: one cold build (~10-15
            # min) instead of a batch stopped at the disk floor. The lane's
            # .venv and its WiredTiger build/ stay.
            local lane_gb
            lane_gb=$(du -sg "$wt" 2>/dev/null | awk '{print $1}')
            if [[ ${lane_gb:-0} -gt ${SECANTUS_LANE_MAX_GB:-35} ]]; then
                rm -rf "$wt"/crates/target "$wt"/crates/*/target
                echo "$(date '+%F %T') lane $k: ${lane_gb} GB over SECANTUS_LANE_MAX_GB (${SECANTUS_LANE_MAX_GB:-35}): cargo targets wiped" >>"$out/summary.txt"
            fi
        fi
        local free_gb
        free_gb=$(df -g "${wt:-.}" | awk 'NR == 2 { print $4 }')
        if [[ $free_gb -lt ${SECANTUS_MIN_FREE_GB:-40} ]]; then
            echo "$(date '+%F %T') STOPPING: item $n: ${free_gb} GB free, under SECANTUS_MIN_FREE_GB (${SECANTUS_MIN_FREE_GB:-40})" >>"$out/summary.txt"
            echo "disk" >"$out/STOP"
            return 0
        fi
        local id=
        [[ -n $list ]] && id=$(grep -vE '^[[:space:]]*(#|$)' "$list" | sed -n "${n}p" | awk '{print $1}')
        scripts/fix_one.sh -p "$backlog" $id ${into:+into "$into"} ${wt:+lane "$wt"} >"$out/$n.log" 2>&1
        local rc=$?
        if [[ $rc -eq 0 && -n $into ]]; then
            local sha
            # The report is prose in markdown, so the sha may be in backticks
            # or bold, and "landed" may be worded otherwise. Each of these
            # stopped a batch over a fix that HAD landed (2026-11-07):
            # "landed on `rm-batch` at `b2ab0554`", "landed on `pg-batch` at
            # **2bc9d4dc**", "the fix is on `pg-batch` at `2edf432c`". Take
            # any "on <branch> at <sha>"; the ancestry check below is what
            # proves the landing.
            m='[`*_]*'
            sha=$(grep -oE "on $m$into$m at $m[0-9a-f]{7,}" "$out/$n.log" | tail -1 |
                grep -oE '[0-9a-f]{7,}$')
            git fetch -q origin "$into"
            if [[ -z $sha ]] || ! git merge-base --is-ancestor "$sha" "origin/$into" 2>/dev/null; then
                echo "$(date '+%F %T') item $n exit 0 but nothing landed on $into" >>"$out/summary.txt"
                rc=3
            fi
        fi
        local pr
        pr=$(grep -oE 'pull/[0-9]+|PR #[0-9]+|landed on [^ ]+ at [`*_]*[0-9a-f]+' "$out/$n.log" | tail -1 | tr -d '`*_')
        echo "$(date '+%F %T') item $n (lane $k) exit $rc ${pr:-no PR}" >>"$out/summary.txt"
        if [[ $rc -ne 0 ]]; then
            echo "$n" >"$out/STOP"
            echo "$(date '+%F %T') STOPPING: item $n exited $rc; see $out/$n.log" >>"$out/summary.txt"
            return 0
        fi
    done
}

for k in $(seq 1 "$jobs"); do
    lane "$k" &
done
wait

# Integration mode opens no PR here: scripts/land_batch.sh <branch> <pid>
# lands the branch in snapshots while the batch runs (a PR straight from
# the branch had its CI cancelled by every lane's push).
echo "$(date '+%F %T') DONE" >>"$out/summary.txt"
