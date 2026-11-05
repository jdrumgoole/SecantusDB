#!/usr/bin/env bash
# Fix several backlog items, each in its own fresh headless session
# (scripts/fix_one.sh -p), with up to two sessions at once.
#
#   scripts/fix_many.sh postgres 10        # ten items, two lanes
#   scripts/fix_many.sh -j 1 postgres 4    # one at a time
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
if [[ "${1:-}" == "-j" ]]; then
    jobs=$2
    shift 2
fi
backlog=${1:?usage: $0 [-j N] <postgres|mongodb> <count>}
count=${2:?usage: $0 [-j N] <postgres|mongodb> <count>}

cd "$(dirname "$0")/.."
out=.scratch/fix_many
mkdir -p "$out"
rm -f "$out/STOP"

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
    scripts/fix_one.sh -p "$backlog" >"$out/$n.log" 2>&1
    local rc=$?
    local pr
    pr=$(grep -oE 'pull/[0-9]+|PR #[0-9]+' "$out/$n.log" | tail -1)
    echo "$(date '+%F %T') item $n exit $rc ${pr:-no PR}" >>"$out/summary.txt"
    if [[ $rc -ne 0 ]]; then
        echo "$n" >"$out/STOP"
        echo "$(date '+%F %T') STOPPING: item $n exited $rc; see $out/$n.log" >>"$out/summary.txt"
    fi
}
export -f run
export backlog jobs out

seq 1 "$count" | xargs -P "$jobs" -I{} bash -c 'run {}'
echo "$(date '+%F %T') DONE" >>"$out/summary.txt"
