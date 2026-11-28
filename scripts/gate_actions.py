#!/usr/bin/env python3
"""Gate the working tree on GitHub Actions: CI's `Tests` workflow, on Linux.

    scripts/gate_actions.py                 # from the worktree root
    scripts/gate_actions.py --name RM-543   # names the gate branch
    scripts/gate_actions.py --keep          # leave the gate branch afterwards

`scripts/gate_rm.py` runs this in place of its own build and test steps when
the worktree holds `.scratch/gate-actions` or `SECANTUS_GATE=actions` is set
(the trial of 2026-11-28, `tasks/plan-cloud-lanes.md`, "An alternative: the
gate on GitHub Actions"). Measured that day: the local `--fast` gate took
28-34 min of a lane item's ~40, at the Mac's load ceiling, while the snapshot
PR's `Tests` run covered more (the full backlog verify, the storage crates
every time) in 13.5 min, on the Linux exemplar build, on runners that cost a
public repo nothing. Two Linux-only differences had passed the lanes' macOS
gates that day and turned snapshots red (RM-543's SBE slot order, RM-448's
profiler key order).

What it does:

1. Snapshots the working tree (tracked changes and new files that are not
   ignored) into a commit on HEAD through a temporary index, so the
   session's own index and HEAD are untouched. It refuses a snapshot that
   carries build output (CLAUDE.md: never commit build output; a gate
   branch is pushed to GitHub too).
2. Pushes it to `gate/<name>-<sha8>` and dispatches `test.yml` there.
3. Waits for the run, starting it again once if no run appears in
   `SECANTUS_CI_RETRIGGER_MIN` minutes (15), as `wait_ci.sh` does for PRs.
4. Prints each job's conclusion, saves the log of every job that did not
   pass under `.scratch/gate/actions-<job>.log`, and deletes the gate
   branch.

Exit 0 when every job passed or was skipped; 1 when one did not (named);
2 when the run could not be started, read, or finished within
`--max-minutes` (90). What it does not cover: macOS and Windows (the
snapshot PR's CI matrix, and the weekly run), and the PostgreSQL server's
suites unless the change reaches them (`pg-oracle` filters itself).
"""

from __future__ import annotations

import argparse
import calendar
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

WORKFLOW = "test.yml"
#: Paths a snapshot must not carry: build output and stores
#: (`tests/test_no_build_output.py` guards the repo the same way).
FORBIDDEN = re.compile(
    r"(^|/)(target[^/]*|build|dist|\.venv|\.scratch|node_modules)/"
    r"|\.(so|dylib|a|rlib|o|whl|profraw|wt|dll|exe)$"
)
MAX_FILE_BYTES = 2_000_000
PASSED = {"success", "skipped", "neutral"}


def git(*args: str, env: dict[str, str] | None = None, check: bool = True) -> str:
    return subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
        check=check,
        env={**os.environ, **(env or {})},
    ).stdout.strip()


def gh_api(path: str, *args: str) -> object:
    out = subprocess.run(
        ["gh", "api", path, *args], capture_output=True, text=True, check=True
    ).stdout
    return json.loads(out) if out.strip() else None


def snapshot() -> str:
    """A commit of the working tree on HEAD, made through a temporary index."""
    with tempfile.TemporaryDirectory() as tmp:
        env = {"GIT_INDEX_FILE": str(Path(tmp) / "index")}
        git("read-tree", "HEAD", env=env)
        git("add", "-A", ".", env=env)
        tree = git("write-tree", env=env)
    changed = git("diff", "--name-only", "HEAD", tree).splitlines()
    bad = [p for p in changed if FORBIDDEN.search(p)]
    big = [p for p in changed if Path(p).is_file() and Path(p).stat().st_size > MAX_FILE_BYTES]
    if bad or big:
        sys.exit(f"gate_actions: refusing to push build output or large files: {bad + big}")
    if tree == git("rev-parse", "HEAD^{tree}"):
        print("    the working tree is HEAD's: gating HEAD", flush=True)
        return git("rev-parse", "HEAD")
    msg = f"gate snapshot of the working tree on {git('rev-parse', '--short', 'HEAD')}"
    return git("commit-tree", tree, "-p", "HEAD", "-m", msg)


def find_run(repo: str, branch: str, since: float) -> dict | None:
    runs = gh_api(
        f"repos/{repo}/actions/workflows/{WORKFLOW}/runs?branch={branch}"
        "&event=workflow_dispatch&per_page=20"
    )
    for r in runs.get("workflow_runs", []) if isinstance(runs, dict) else []:
        created = calendar.timegm(time.strptime(r["created_at"], "%Y-%m-%dT%H:%M:%SZ"))
        if created >= since - 120:
            return r
    return None


def gate_base() -> str:
    """The commit the change sits on: where it meets `origin/rm-batch`, or
    `origin/main`. test.yml runs a dispatch with it as a PR's run."""
    for ref in ("origin/rm-batch", "origin/main"):
        base = git("merge-base", "HEAD", ref, check=False)
        if base:
            return base
    return ""


def dispatch(branch: str, base: str) -> None:
    cmd = ["gh", "workflow", "run", WORKFLOW, "--ref", branch]
    if base:
        cmd += ["-f", f"gate_base={base}"]
    subprocess.run(cmd, check=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--name", default="", help="names the gate branch (default: the branch, or the worktree)"
    )
    ap.add_argument("--keep", action="store_true", help="leave the gate branch afterwards")
    ap.add_argument("--max-minutes", type=int, default=90)
    args = ap.parse_args()

    repo = subprocess.run(
        ["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    sha = snapshot()
    base = gate_base()
    # A lane's worktree sits on a detached HEAD: name the gate after it.
    head = git("rev-parse", "--abbrev-ref", "HEAD")
    name = args.name or (Path.cwd().name if head == "HEAD" else head)
    name = re.sub(r"[^A-Za-z0-9._-]+", "-", name)
    branch = f"gate/{name}-{sha[:8]}"
    out = Path(".scratch/gate")
    out.mkdir(parents=True, exist_ok=True)

    git("push", "-q", "origin", f"{sha}:refs/heads/{branch}")
    print(f"    pushed {sha[:8]} to {branch}", flush=True)
    try:
        since = time.time()
        dispatch(branch, base)
        retrigger = int(os.environ.get("SECANTUS_CI_RETRIGGER_MIN", "15")) * 60
        deadline = since + args.max_minutes * 60
        run = None
        retried = False
        while run is None:
            time.sleep(20)
            try:
                run = find_run(repo, branch, since)
            except (subprocess.CalledProcessError, json.JSONDecodeError):
                run = None  # a transient API error: look again
            if run is None and time.time() - since > retrigger and not retried:
                print(f"    no run after {retrigger // 60} min: dispatching again", flush=True)
                dispatch(branch, base)
                retried = True
            if run is None and time.time() > deadline:
                print(f"TIMEOUT: no run of {WORKFLOW} on {branch}", flush=True)
                return 2
        print(f"    run {run['id']}: {run['html_url']}", flush=True)
        while run.get("status") != "completed":
            if time.time() > deadline:
                print(f"TIMEOUT after {args.max_minutes} min: run {run['id']} still running")
                return 2
            time.sleep(60)
            try:
                run = gh_api(f"repos/{repo}/actions/runs/{run['id']}")
            except (subprocess.CalledProcessError, json.JSONDecodeError):
                continue
        jobs = gh_api(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100")
        failed = []
        for job in sorted(jobs["jobs"], key=lambda j: j["name"]):
            print(f"      {job['conclusion']}\t{job['name']}", flush=True)
            if job["conclusion"] not in PASSED:
                failed.append(job["name"])
                log = out / f"actions-{re.sub(r'[^A-Za-z0-9._-]+', '_', job['name'])}.log"
                text = subprocess.run(
                    ["gh", "api", f"repos/{repo}/actions/jobs/{job['id']}/logs"],
                    capture_output=True,
                    text=True,
                ).stdout
                log.write_text(text)
                print(f"        log: {log}", flush=True)
        if not jobs["jobs"]:
            print(f"RED: run {run['id']} ({run['conclusion']}) started no job", flush=True)
            return 1
        if failed or run.get("conclusion") not in PASSED:
            print(f"RED: {', '.join(failed) or run.get('conclusion')}", flush=True)
            return 1
        print(f"GREEN: all {len(jobs['jobs'])} jobs passed", flush=True)
        return 0
    finally:
        if not args.keep:
            git("push", "-q", "origin", "--delete", branch, check=False)


if __name__ == "__main__":
    sys.exit(main())
