#!/usr/bin/env bash
# Prove scripts/ci_disk_watch.sh works on THIS runner before trusting it:
# a process holds a 3 MB file open, the watchdog runs with thresholds that
# make it fire at once, and the test fails unless the report names the file
# and the holder was killed. Runs in CI before the real watchdog starts, so
# a watchdog that silently cannot see or stop a writer fails the job.
set -u
dir=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/disk-watch-selftest.XXXXXX")
python3 -c "
import os, time
f = open('$dir/big.wt', 'wb'); f.write(os.urandom(3 * 1024 * 1024)); f.flush()
time.sleep(120)" &
writer=$!
sleep 1
DISK_WATCH_MIN_KB=999999999999 DISK_WATCH_FILE_KB=1024 DISK_WATCH_DIRS="$dir" \
    DISK_WATCH_EVERY=1 "$(dirname "$0")/ci_disk_watch.sh" "$dir/report.txt" "$dir" &
watch=$!
for _ in $(seq 1 20); do
    kill -0 "$writer" 2>/dev/null || break
    sleep 1
done
kill "$watch" 2>/dev/null
status=0
if kill -0 "$writer" 2>/dev/null; then
    echo "disk watchdog self-test: the writer was NOT killed"
    kill -9 "$writer"
    status=1
fi
grep -q "big.wt" "$dir/report.txt" 2>/dev/null || { echo "disk watchdog self-test: the report does not name the file"; status=1; }
grep -q "== holder of" "$dir/report.txt" 2>/dev/null || { echo "disk watchdog self-test: no holder found"; status=1; }
cat "$dir/report.txt" 2>/dev/null | head -30
rm -f "$dir/big.wt" "$dir/report.txt"
rmdir "$dir"
[ $status -eq 0 ] && echo "disk watchdog self-test: ok"
exit $status
