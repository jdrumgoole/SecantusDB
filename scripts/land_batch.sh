#!/usr/bin/env bash
# Land an integration branch (fix_many -i) on main in SNAPSHOTS, while a
# batch keeps pushing to it.
#
#   scripts/land_batch.sh pg-batch [batch-pid]
#
# Each round freezes origin/<branch> on <branch>-ci, opens (or reuses) a PR
# from <branch>-ci to main, waits for CI on that fixed commit
# (scripts/wait_ci.sh), and merges it with a merge commit pinned to the
# tested sha, so the branch's commits become main's history and the next
# snapshot carries only newer ones. The lanes' pushes go to <branch>, so
# they do not cancel the run, which a PR straight from <branch> suffered:
# three lanes push faster than CI's 25 min (2026-11-06).
#
# With a batch-pid it keeps going until that process has exited and
# nothing is left to land; without one it lands what is there and stops.
#
# SECANTUS_LAND_MIN=N (default 1): while the batch is still running, wait
# until at least N commits (one per item) are on <branch> before freezing a
# snapshot, so each full CI cycle checks N items together rather than
# whatever one lane pushed last (Joe, 2026-11-10: "5 items at a time before
# a full ci cycle"). Once the batch has exited, the remainder lands whatever
# its size.
#
# Exit 0 done; 1 a check on the snapshot failed (bisect its commits; nothing
# merged); 2 a git / gh step failed; 4 this PR's checks passed but another
# run of the same commit failed, or a workflow run failed with jobs that
# never started (scripts/wait_ci.sh): read it before merging.
# A failed claude-review alone does not block: it is advisory.
set -uo pipefail
branch=${1:?usage: $0 <integration-branch> [batch-pid]}
batch_pid=${2:-}
land_min=${SECANTUS_LAND_MIN:-1}
[[ $land_min =~ ^[1-9][0-9]*$ ]] || { echo "SECANTUS_LAND_MIN must be a positive integer" >&2; exit 2; }
[[ $branch == main ]] && { echo "not main" >&2; exit 2; }
ci=$branch-ci
# Which server's batch this is: the MongoDB backlog's integration branches
# start `rm` (rm-batch), the PostgreSQL one's anything else (pg-batch).
case "$branch" in
    rm*) server="Rust MongoDB server"; gate="scripts/gate_rm.py" ;;
    *) server="Rust PG server"; gate="scripts/gate_pg.sh --fast" ;;
esac
cd "$(dirname "$0")/.."
# Keep the Mac awake while this runs. An idle Mac on AC power drops into
# "Maintenance Sleep" for 10-20 minutes at a time, and every process stops
# with it: on 2026-11-08 a lander's `sleep 300` came back after 63 minutes
# and the fix-one sessions beside it stalled the same way (RM-443, from
# `pmset -g log`). `-i` alone was not enough: once the Mac had slept, it
# kept going "back to sleep" from each short dark wake (`Sleep Service Back
# to Sleep`), which only `-s` (no system sleep, honoured on AC power) stops.
# `caffeinate -i -s -w $$` holds both until this script exits, and does
# nothing where caffeinate is not installed.
if command -v caffeinate >/dev/null 2>&1; then
    caffeinate -i -s -w $$ &
fi
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 2
while :; do
    git fetch -q origin
    ahead=$(git rev-list --count origin/main..origin/$branch)
    if [[ $ahead -eq 0 ]]; then
        if [[ -z $batch_pid ]] || ! kill -0 "$batch_pid" 2>/dev/null; then
            echo "$(date '+%T') nothing left to land"
            exit 0
        fi
        sleep 300; continue
    fi
    if [[ $ahead -lt $land_min && -n $batch_pid ]] && kill -0 "$batch_pid" 2>/dev/null; then
        sleep 300; continue
    fi
    snap=$(git rev-parse origin/$branch)
    git push -q -f origin "$snap:refs/heads/$ci" || { echo "push failed"; exit 2; }
    pr=$(gh pr list --head $ci --base main --state open --json number -q '.[0].number')
    if [[ -z $pr ]]; then
        url=$(gh pr create --base main --head $ci \
            --title "$server: fix-one batch snapshot of $branch (${snap:0:8}, $ahead commits)" \
            --body "$(git log --reverse --format='- %s' origin/main..$snap)

A snapshot of \`$branch\`, frozen on \`$ci\` so the lanes' later pushes don't cancel its CI. Each commit passed \`$gate\` locally. This run is their cross-platform check. It will be merged with a merge commit, pinned to the commit CI passed.

🤖 Generated with [Claude Code](https://claude.com/claude-code)") || exit 2
        pr=${url##*/}
    fi
    echo "$(date '+%T') PR #$pr at ${snap:0:8} ($ahead commits)"
    out=$(scripts/wait_ci.sh "$pr" 120 "$snap" 2>&1); rc=$?
    echo "$out" | tail -12
    head=$(gh pr view "$pr" --json headRefOid -q .headRefOid)
    [[ $head == "$snap" ]] || { echo "$ci moved under the wait?"; exit 2; }
    # wait_ci reads every check run on the COMMIT, which includes another
    # PR's runs of the same sha (#2386 and #2387, 2026-11-06). Judge by this
    # PR's own checks once wait_ci says they all finished.
    if [[ $rc -eq 1 ]]; then
        # No checks reported is not "all passed": it is a run that has not
        # started, and is judged red.
        bad=$(gh pr checks "$pr" --json name,state -q 'if length == 0 then "(no checks reported)" else [.[]|select(.state!="SUCCESS" and .state!="SKIPPED")]|map(.name)|join(", ") end')
        if [[ -z $bad ]]; then echo "$(date '+%T') this PR's checks all passed but wait_ci found a red run on ${snap:0:8} (another PR's, or a workflow run whose jobs never started); stopping so it is read before merging"; exit 4; fi
        # claude-review is advice, not a test: it reviews the diff and its
        # own setup has failed on upstream outages (a Bun download 504,
        # 2026-11-08), which held a green snapshot for hours. Every
        # commit already passed the gate and every test lane here.
        if [[ $bad == claude-review ]]; then
            echo "$(date '+%T') only claude-review failed on PR #$pr: advisory, merging"
            rc=0
        fi
    fi
    if [[ $rc -ne 0 ]]; then echo "$(date '+%T') RED on PR #$pr at ${snap:0:8}: not merging; bisect"; exit 1; fi
    gh api -X PUT "repos/$repo/pulls/$pr/merge" -f merge_method=merge -f sha="$snap" -q .merged || { echo "merge refused"; exit 2; }
    echo "$(date '+%T') merged PR #$pr: $(gh pr view "$pr" --json state -q .state)"
done
