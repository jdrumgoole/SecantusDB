#!/usr/bin/env bash
# Wait for every check on a PR's head commit to finish, then report them.
#
#   scripts/wait_ci.sh <PR> [max-minutes]     # default 120
#
# Exit 0: every check completed and passed (success / skipped / neutral).
# Exit 1: a check completed with any other conclusion (it is named).
# Exit 2: still running at the deadline, or the PR could not be read.
#
# Polls the REST check-runs API once a minute: `gh pr checks` in a loop
# burns the GraphQL quota and its exit codes (8 pending, 1 failed) wedge
# `until` loops. Hand-written waiters in fix-one runs wedged twice in one
# night (a zsh word-splitting bug, a `^build=` grep that ANSI codes defeated);
# use this one. Run it in the background: it outlives the 10-minute
# foreground limit.
set -uo pipefail

pr=${1:?usage: $0 <PR> [max-minutes]}
max=${2:-120}
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 2
sha=$(gh pr view "$pr" --json headRefOid -q .headRefOid) || exit 2
api="repos/$repo/commits/$sha/check-runs?per_page=100"
echo "PR #$pr at ${sha:0:8}: waiting up to $max min"

deadline=$((SECONDS + max * 60))
while :; do
    # total and pending in one call; a transient API error counts as pending.
    read -r total pending < <(gh api "$api" \
        --jq '"\(.total_count) \([.check_runs[] | select(.status != "completed")] | length)"' \
        2>/dev/null || echo "0 1")
    if [[ "${total:-0}" -gt 0 && "${pending:-1}" -eq 0 ]]; then
        break
    fi
    if [[ $SECONDS -ge $deadline ]]; then
        echo "TIMEOUT after $max min: $pending of $total checks still running"
        gh api "$api" --jq '.check_runs[] | select(.status != "completed") | "  running: \(.name)"'
        exit 2
    fi
    sleep 60
done

gh api "$api" --jq '.check_runs[] | "  \(.conclusion)\t\(.name)"' | sort
bad=$(gh api "$api" --jq '[.check_runs[] | select(.conclusion | IN("success","skipped","neutral") | not)] | length')
if [[ "$bad" -ne 0 ]]; then
    echo "RED: $bad check(s) did not pass"
    exit 1
fi
echo "GREEN: all $total checks passed"
