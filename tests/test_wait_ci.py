"""scripts/wait_ci.sh judges a PR's CI before the batch lander merges it.

A job that never starts leaves no check run, so the check runs alone can
read green while the workflow run failed: PR #2563's Tests run created 3 of
its 8 jobs during a GitHub Actions incident (2026-11-09), concluded
`failure`, and was merged on "all 6 checks passed". These run the script
against a stand-in `gh` that answers from fixed JSON through real `jq`
(on PATH on macOS and the CI runners).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHA = "2702bbc3" + "0" * 32

FAKE_GH = r"""#!PYTHON
import json, subprocess, sys
from pathlib import Path
data = json.loads(Path(__file__).with_name("gh.json").read_text())
args = sys.argv[1:]
if args[:2] == ["repo", "view"]:
    print("o/r"); sys.exit(0)
if args[:2] == ["pr", "view"]:
    field = args[args.index("--json") + 1]
    print(data["pr"][field]); sys.exit(0)
if args[0] == "api":
    path = args[1]
    jq = args[args.index("--jq") + 1]
    key = "runs" if "/actions/runs" in path else "checks" if "/check-runs" in path else "pull"
    doc = json.dumps(data[key])
    out = subprocess.run(["jq", "-r", jq], input=doc, capture_output=True, text=True)
    sys.stdout.write(out.stdout); sys.exit(out.returncode)
sys.exit(f"unexpected gh call: {args}")
"""


def _run(tmp_path: Path, checks: list[dict], runs: list[dict]) -> subprocess.CompletedProcess[str]:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    gh.write_text(FAKE_GH.replace("PYTHON", sys.executable))
    gh.chmod(0o755)
    (bin_dir / "gh.json").write_text(
        json.dumps(
            {
                "pr": {"headRefOid": SHA, "headRefName": "rm-batch-ci"},
                "checks": {"total_count": len(checks), "check_runs": checks},
                "runs": {"workflow_runs": runs},
                "pull": {"mergeable_state": "clean"},
            }
        )
    )
    env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    return subprocess.run(
        ["bash", str(ROOT / "scripts/wait_ci.sh"), "2563", "1", SHA[:8]],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
        cwd=ROOT,
    )


def _check(name: str, conclusion: str = "success") -> dict:
    return {"name": name, "status": "completed", "conclusion": conclusion}


def _run_of(name: str, conclusion: str, branch: str = "rm-batch-ci") -> dict:
    return {"name": name, "status": "completed", "conclusion": conclusion, "head_branch": branch}


CHECKS = [_check(n) for n in ("test", "rust-storage", "storage-engine", "docs", "package")]


def test_a_failed_workflow_run_whose_jobs_never_started_is_red(tmp_path: Path) -> None:
    proc = _run(tmp_path, CHECKS, [_run_of("Tests", "failure"), _run_of("Docs", "success")])
    assert proc.returncode == 1, proc.stdout + proc.stderr
    assert "Tests: failure" in proc.stdout


def test_passing_checks_and_runs_are_green(tmp_path: Path) -> None:
    proc = _run(tmp_path, CHECKS, [_run_of("Tests", "success"), _run_of("Docs", "success")])
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "GREEN" in proc.stdout


def test_another_branchs_run_of_the_same_commit_is_not_this_prs(tmp_path: Path) -> None:
    runs = [_run_of("Tests", "success"), _run_of("Tests", "failure", branch="other")]
    proc = _run(tmp_path, CHECKS, runs)
    assert proc.returncode == 0, proc.stdout + proc.stderr


def test_a_failed_check_is_red(tmp_path: Path) -> None:
    checks = [*CHECKS, _check("rust", "failure")]
    proc = _run(tmp_path, checks, [_run_of("Tests", "failure")])
    assert proc.returncode == 1
    assert "check(s) did not pass" in proc.stdout
