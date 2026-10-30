#!/usr/bin/env bash
# Fix one backlog item in a FRESH Claude Code session: the equivalent of
# typing `/clear` and then `/fix-one <args>`. A new session starts with an
# empty context, which a skill cannot give itself (`/clear` is a built-in
# command nothing else can trigger).
#
#   scripts/fix_one.sh postgres            # pick an item from the PG backlog
#   scripts/fix_one.sh mongodb RM-142      # a named item
#   scripts/fix_one.sh pg hardest          # with a choice rule
#   scripts/fix_one.sh -p postgres         # headless (claude -p): no questions
#
# Interactive is the default because the skill may stop to ask Joe for a
# decision; `-p` runs it unattended and prints the final report.
set -euo pipefail

headless=0
if [[ "${1:-}" == "-p" ]]; then
    headless=1
    shift
fi
if [[ $# -lt 1 ]]; then
    echo "usage: $0 [-p] <postgres|mongodb|backlog-file> [ID|rule]" >&2
    exit 2
fi
case "$1" in
    python* | *backlog-python*)
        echo "$0: the Python backlogs are a record, not worked" >&2
        exit 2
        ;;
esac

# Run from the main checkout, whatever directory this was called from: the
# skill makes its own worktree.
cd "$(dirname "$0")/.."
command -v claude >/dev/null || {
    echo "$0: the claude CLI is not on PATH" >&2
    exit 127
}

prompt="/fix-one $*"
stamp() { date '+%Y-%m-%d %H:%M:%S %Z'; }

# Bracket the session with timestamps so a log shows when it ran and how
# long it took; not exec'd, so the end line can follow claude's exit.
echo "[$(stamp)] start: $prompt"
status=0
if [[ $headless -eq 1 ]]; then
    claude -p "$prompt" || status=$?
else
    claude "$prompt" || status=$?
fi
echo "[$(stamp)] end: $prompt (exit $status)"
exit "$status"
