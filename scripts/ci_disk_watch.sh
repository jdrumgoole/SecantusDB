#!/usr/bin/env bash
# CI disk watchdog for RP-312: something writes tens of GB within seconds and
# fills the runner's disk so completely that the Actions runner itself dies,
# leaving no log to name the writer (2026-11-01 pg-oracle, 2026-11-03
# storage-engine). This runs in the background for the whole job. When free
# space on the watched filesystem falls under the threshold it writes a report
# -- the largest files, the processes holding them, and each one's pytest
# worker (whose process title names the running test) -- and then KILLS the
# processes holding the largest files, so the runner survives and the test
# that wrote the data fails by name. A later step prints the report and fails
# the job: a triggered watchdog is a bug report, never a pass.
#
#   scripts/ci_disk_watch.sh <report-file> [watched-path]
#
# DISK_WATCH_MIN_KB  trigger when free space is under this (default 10 GB)
# DISK_WATCH_FILE_KB a file this big counts as a suspect (default 1 GB)
# DISK_WATCH_DIRS    where to look for big files (default /tmp, the runner
#                    temp and work dirs)
# DISK_WATCH_EVERY   seconds between samples (default 2)
set -u

report=${1:?usage: $0 <report-file> [watched-path]}
watched=${2:-/}
min_kb=${DISK_WATCH_MIN_KB:-10485760}
file_kb=${DISK_WATCH_FILE_KB:-1048576}
every=${DISK_WATCH_EVERY:-2}
dirs=${DISK_WATCH_DIRS:-/tmp ${RUNNER_TEMP:-} ${GITHUB_WORKSPACE:-}}

free_kb() { df -Pk "$watched" | awk 'NR == 2 { print $4 }'; }

# PIDs holding a file open: /proc on Linux, lsof elsewhere.
holders() {
    local f=$1 pid
    if [[ -d /proc/self/fd ]]; then
        for pid in /proc/[0-9]*; do
            ls -l "$pid/fd" 2>/dev/null | grep -qF -- "-> $f" && echo "${pid#/proc/}"
        done
    else
        lsof -t -- "$f" 2>/dev/null
    fi
}

# A process and its ancestors: the pytest-xdist worker's title names the test.
lineage() {
    local pid=$1 n=0
    while [[ -n $pid && $pid -gt 1 && $n -lt 8 ]]; do
        ps -o pid=,ppid=,etime=,command= -p "$pid" 2>/dev/null | cut -c1-300
        pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
        n=$((n + 1))
    done
}

fired=0
while :; do
    kb=$(free_kb)
    if [[ -n $kb && $kb -lt $min_kb && $fired -eq 0 ]]; then
        fired=1
        {
            echo "RP-312 disk watchdog fired at $(date -u '+%F %T') UTC: ${kb} KB free on $watched (threshold ${min_kb} KB)"
            df -h "$watched"
            echo
            echo "== largest files (>= ${file_kb} KB) under: $dirs"
            # shellcheck disable=SC2086
            find $dirs -xdev -type f -size +"${file_kb}"k -exec ls -ln {} + 2>/dev/null |
                sort -k5 -n -r | head -20 >"$report.files"
            cat "$report.files"
            echo
            echo "== largest directories"
            # shellcheck disable=SC2086
            du -xk -d 4 $dirs 2>/dev/null | sort -n -r | head -25
            echo
            killed=""
            while read -r _ _ _ _ _ _ _ _ path; do
                [[ -n $path ]] || continue
                for pid in $(holders "$path"); do
                    echo "== holder of $path"
                    lineage "$pid"
                    killed="$killed $pid"
                done
            done < <(head -5 "$report.files")
            if [[ -n $killed ]]; then
                echo
                echo "== killing the holders of the largest files:$killed"
                # shellcheck disable=SC2086
                kill -9 $killed 2>/dev/null
            else
                echo "== no process holds the largest files; nothing killed"
            fi
            echo
            echo "== every pytest worker at the time (its title is its test)"
            ps -Ao pid=,etime=,command= | grep -E 'pytest-xdist|python -m pytest' | grep -v grep | cut -c1-300
        } >"$report" 2>&1
        rm -f "$report.files"
    fi
    sleep "$every"
done
