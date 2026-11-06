#!/usr/bin/env bash
# Fix several backlog items, each in its own fresh headless session
# (scripts/fix_one.sh -p), with up to two (or three, see below) at once.
#
#   scripts/fix_many.sh postgres 10        # ten items, two lanes
#   scripts/fix_many.sh -j 1 postgres 4    # one at a time
#   scripts/fix_many.sh -i pg-batch postgres 8   # land on pg-batch, not main
#
# -i BRANCH (integration mode): each session gates locally and pushes its
# fix to BRANCH, with no PR and no CI wait (fix-one skill, "Integration
# mode"). BRANCH is created from origin/main if it does not exist. At the
# end the batch opens (or reuses) one PR from BRANCH to main and prints its
# number; CI on that PR is the check for the whole batch, and its merge
# (a merge commit, pinned to the head CI passed) is done by whoever runs
# the batch.
#
# In integration mode each lane keeps ONE worktree, ../SecantusDB-lane<k>,
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
while [[ "${1:-}" == -* ]]; do
    case "$1" in
        -j) jobs=$2 ;;
        -i) into=$2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift 2
done
backlog=${1:?usage: $0 [-j N] [-i BRANCH] <postgres|mongodb> <count>}
count=${2:?usage: $0 [-j N] [-i BRANCH] <postgres|mongodb> <count>}

cd "$(dirname "$0")/.."
out=.scratch/fix_many
mkdir -p "$out"
rm -f "$out/STOP"

if [[ -n $into ]]; then
    [[ $into == main ]] && { echo "-i main: use the default mode" >&2; exit 2; }
    git fetch -q origin
    if ! git ls-remote --exit-code --heads origin "$into" >/dev/null; then
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
        wt=$(cd .. && pwd)/SecantusDB-lane$k
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
        scripts/fix_one.sh -p "$backlog" ${into:+into "$into"} ${wt:+lane "$wt"} >"$out/$n.log" 2>&1
        local rc=$?
        local pr
        pr=$(grep -oE 'pull/[0-9]+|PR #[0-9]+|landed on [^ ]+ at [0-9a-f]+' "$out/$n.log" | tail -1)
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

if [[ -n $into ]]; then
    git fetch -q origin
    ahead=$(git rev-list --count "origin/main..origin/$into")
    if [[ $ahead -gt 0 ]]; then
        pr=$(gh pr list --head "$into" --base main --state open --json number -q '.[0].number')
        if [[ -z $pr ]]; then
            url=$(gh pr create --base main --head "$into" \
                --title "Rust PG server: fix-one batch ($ahead items, $into)" \
                --body "$(git log --format='- %s' "origin/main..origin/$into")") || exit 1
            pr=${url##*/}
        fi
        echo "$(date '+%F %T') batch PR #$pr ($ahead commits from $into)" >>"$out/summary.txt"
    fi
fi
echo "$(date '+%F %T') DONE" >>"$out/summary.txt"
