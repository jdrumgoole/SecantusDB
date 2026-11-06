#!/usr/bin/env bash
# Fix several backlog items, each in its own fresh headless session
# (scripts/fix_one.sh -p), with up to two sessions at once.
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
# (rebase, keeping one commit per item) is done by whoever runs the batch.
#
# Two lanes because CLAUDE.md allows two building agents per session (four
# machine-wide); a lane spends most of its time waiting on CI, so two lanes
# roughly double throughput. The lanes cannot take the same item: fix-one's
# claim is a pushed branch, checked before work starts. Do not raise -j above
# 2 while another session builds. Each lane also waits for the load average
# to fall under SECANTUS_LOAD_CEILING (default 10) before it starts.
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

run() {
    local n=$1
    [[ -e $out/STOP ]] && return 0
    # Stagger the lanes so the second does not pick while the first is
    # claiming the same item.
    [[ $n -le $jobs ]] && sleep $(((n - 1) * 120))
    # Start no session while the machine is already over budget: the other
    # lane, a leftover gate or another session may be building
    # (scripts/wait_for_load.sh has the 2026-11-02 numbers).
    scripts/wait_for_load.sh "fix_many item $n" 2>>"$out/summary.txt"
    [[ -e $out/STOP ]] && return 0
    scripts/fix_one.sh -p "$backlog" ${into:+into "$into"} >"$out/$n.log" 2>&1
    local rc=$?
    local pr
    pr=$(grep -oE 'pull/[0-9]+|PR #[0-9]+|landed on [^ ]+ at [0-9a-f]+' "$out/$n.log" | tail -1)
    echo "$(date '+%F %T') item $n exit $rc ${pr:-no PR}" >>"$out/summary.txt"
    if [[ $rc -ne 0 ]]; then
        echo "$n" >"$out/STOP"
        echo "$(date '+%F %T') STOPPING: item $n exited $rc; see $out/$n.log" >>"$out/summary.txt"
    fi
}
export -f run
export backlog jobs out into

seq 1 "$count" | xargs -P "$jobs" -I{} bash -c 'run {}'
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
