#!/usr/bin/env bash
# Wait for every check on a PR's head commit to finish, then report them.
#
#   scripts/wait_ci.sh <PR> [max-minutes] [sha]   # default 120
#
# sha: the commit just pushed to the PR's branch. GitHub moves the PR's head
# a few seconds after a push, and until it does the PR still names the
# previous commit, whose finished checks would be reported as this push's
# (land_batch.sh read PR #2441's red run on d373904b as ed796664's,
# 2026-11-07). Waits up to 10 min for the head to become sha (exit 2 if not).
#
# Exit 0: every check completed and passed (success / skipped / neutral),
#         and so did every workflow run of the PR's branch on the commit.
# Exit 1: a check completed with any other conclusion (it is named), or a
#         workflow run did: a job that never started leaves no check run,
#         so the check runs alone read green (PR #2563's Tests run created
#         3 of its 8 jobs and concluded failure during a GitHub Actions
#         incident, 2026-11-09, and was merged on "all 6 checks passed").
# Exit 2: still running at the deadline, or the PR could not be read.
# Exit 3: the PR conflicts with its base, so GitHub starts no CI at all;
#         rebase onto origin/main, push, and wait again.
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
want=${3:-}
sha=$(gh pr view "$pr" --json headRefOid -q .headRefOid) || exit 2
branch=$(gh pr view "$pr" --json headRefName -q .headRefName) || exit 2
if [[ -n $want ]]; then
    tries=0
    until [[ $sha == "$want"* ]]; do
        tries=$((tries + 1))
        if [[ $tries -gt 60 ]]; then
            echo "PR #$pr still at ${sha:0:8}, not ${want:0:8}, after 10 min"
            exit 2
        fi
        sleep 10
        sha=$(gh pr view "$pr" --json headRefOid -q .headRefOid) || exit 2
    done
fi
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
    # No checks yet: a conflicting PR never gets any (a fix-one session
    # waited 70 min on #2308 for CI that could not start, 2026-11-04).
    if [[ "${total:-0}" -eq 0 ]]; then
        state=$(gh api "repos/$repo/pulls/$pr" --jq .mergeable_state 2>/dev/null)
        if [[ "$state" == dirty ]]; then
            echo "CONFLICT: PR #$pr conflicts with its base; no CI will run. Rebase onto origin/main and push."
            exit 3
        fi
    fi
    if [[ $SECONDS -ge $deadline ]]; then
        echo "TIMEOUT after $max min: $pending of $total checks still running"
        gh api "$api" --jq '.check_runs[] | select(.status != "completed") | "  running: \(.name)"'
        exit 2
    fi
    sleep 60
done

# The workflow runs of this PR's branch on the commit: each must finish
# and pass too. Another PR's runs of the same sha are not this PR's.
runs="repos/$repo/actions/runs?head_sha=$sha&per_page=100"
pick=".workflow_runs[] | select(.head_branch == \"$branch\")"
while :; do
    running=$(gh api "$runs" --jq "[$pick | select(.status != \"completed\")] | length" \
        2>/dev/null || echo 1)
    [[ "${running:-1}" -eq 0 ]] && break
    if [[ $SECONDS -ge $deadline ]]; then
        echo "TIMEOUT after $max min: $running workflow run(s) still running"
        exit 2
    fi
    sleep 60
done

# Counted again: the first loop can end while later workflows' checks have
# yet to register (PR #2565 reported "all 2 checks" of 10).
total=$(gh api "$api" --jq .total_count) || exit 2
gh api "$api" --jq '.check_runs[] | "  \(.conclusion)\t\(.name)"' | sort
bad=$(gh api "$api" --jq '[.check_runs[] | select(.conclusion | IN("success","skipped","neutral") | not)] | length')
if [[ "$bad" -ne 0 ]]; then
    echo "RED: $bad check(s) did not pass"
    exit 1
fi
badruns=$(gh api "$runs" \
    --jq "[$pick | select(.conclusion | IN(\"success\",\"skipped\",\"neutral\") | not) | \"\\(.name): \\(.conclusion)\"] | join(\", \")")
if [[ -n "$badruns" ]]; then
    echo "RED: every check passed but a workflow run did not ($badruns); a job that never started has no check"
    exit 1
fi
echo "GREEN: all $total checks passed"
