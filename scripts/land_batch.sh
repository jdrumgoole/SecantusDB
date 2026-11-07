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
# Exit 0 done; 1 a check on the snapshot failed (bisect its commits; nothing
# merged); 2 a git / gh step failed; 4 this PR's checks passed but another
# run of the same commit failed (an intermittent: read it before merging).
set -uo pipefail
branch=${1:?usage: $0 <integration-branch> [batch-pid]}
batch_pid=${2:-}
[[ $branch == main ]] && { echo "not main" >&2; exit 2; }
ci=$branch-ci
# Which server's batch this is: the MongoDB backlog's integration branches
# start `rm` (rm-batch), the PostgreSQL one's anything else (pg-batch).
case "$branch" in
    rm*) server="Rust MongoDB server"; gate="scripts/gate_rm.py" ;;
    *) server="Rust PG server"; gate="scripts/gate_pg.sh --fast" ;;
esac
cd "$(dirname "$0")/.."
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
    out=$(scripts/wait_ci.sh "$pr" 120 2>&1); rc=$?
    echo "$out" | tail -12
    head=$(gh pr view "$pr" --json headRefOid -q .headRefOid)
    [[ $head == "$snap" ]] || { echo "$ci moved under the wait?"; exit 2; }
    # wait_ci reads every check run on the COMMIT, which includes another
    # PR's runs of the same sha (#2386 and #2387, 2026-11-06). Judge by this
    # PR's own checks once wait_ci says they all finished.
    if [[ $rc -eq 1 ]]; then
        bad=$(gh pr checks "$pr" --json name,state -q '[.[]|select(.state!="SUCCESS" and .state!="SKIPPED")]|map(.name)|join(", ")')
        if [[ -z $bad ]]; then echo "$(date '+%T') a red run on ${snap:0:8} belongs to another PR; this PR's checks all passed; stopping so it is read before merging"; exit 4; fi
    fi
    if [[ $rc -ne 0 ]]; then echo "$(date '+%T') RED on PR #$pr at ${snap:0:8}: not merging; bisect"; exit 1; fi
    gh api -X PUT "repos/$repo/pulls/$pr/merge" -f merge_method=merge -f sha="$snap" -q .merged || { echo "merge refused"; exit 2; }
    echo "$(date '+%T') merged PR #$pr: $(gh pr view "$pr" --json state -q .state)"
done
