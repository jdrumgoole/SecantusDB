#!/usr/bin/env bash
# Block until the machine's 1-minute load average is under a ceiling, so a
# new build or test step does not pile onto ones already running.
#
#   scripts/wait_for_load.sh [label]
#
# SECANTUS_LOAD_CEILING  load average to wait under (default 10: this Mac has
#                        12 cores, and CLAUDE.md's budget is one job per core)
# SECANTUS_LOAD_MAX_WAIT seconds to wait at most (default 1800); after that it
#                        goes ahead and says so, rather than wedge a run
#
# Why: on 2026-11-02 two fix_many lanes, a cut-off session's leftover gate and
# another session's builds held the load average at 68 on 12 cores. A timing
# budget (RP-077's latency test) failed for no reason in the change, and two
# gates collided on the TLS login probe's port. Caps on cargo jobs and pytest
# workers bound each process; only waiting bounds how many run at once.
set -uo pipefail

label=${1:-step}
ceiling=${SECANTUS_LOAD_CEILING:-10}
max_wait=${SECANTUS_LOAD_MAX_WAIT:-1800}

load1() {
    if [[ -r /proc/loadavg ]]; then
        cut -d' ' -f1 /proc/loadavg
    else
        sysctl -n vm.loadavg | tr -d '{}' | awk '{print $1}'
    fi
}

under() { awk -v l="$1" -v c="$ceiling" 'BEGIN { exit !(l < c) }'; }

waited=0
l=$(load1)
while ! under "$l"; do
    if [[ $waited -ge $max_wait ]]; then
        echo "wait_for_load: $label: load $l still >= $ceiling after ${waited}s; going ahead" >&2
        exit 0
    fi
    [[ $waited -eq 0 ]] && echo "wait_for_load: $label: load $l >= $ceiling; waiting" >&2
    sleep 30
    waited=$((waited + 30))
    l=$(load1)
done
[[ $waited -gt 0 ]] && echo "wait_for_load: $label: load $l after ${waited}s; starting" >&2
exit 0
