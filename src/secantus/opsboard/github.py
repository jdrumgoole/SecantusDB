"""Read-only GitHub Actions observation via the ``gh`` CLI.

This is **Tier 1** of the Ops Board's cross-session tracking (see
``tasks/opsboard-plan.md`` §4): a workflow run triggered by *anyone* — your
push, a parallel session, a cron, a release tag — is visible here with full
status. Nothing needs to opt in, because GitHub is the shared source of truth.

Design constraints:

* **Never breaks the page.** ``gh`` may be absent, unauthenticated, rate-limited
  or offline. Every call returns a degraded-but-valid result and records
  ``last_error`` for the UI to surface, rather than raising.
* **Injectable + hermetic.** The subprocess runner is a constructor argument so
  tests drive canned JSON and never touch the network.
* **Bounded and cached.** Every query takes an explicit limit (no unbounded
  listing) and results are cached for ``ttl`` seconds so a 1-second UI poll
  doesn't spawn a ``gh`` process per tick.
"""

from __future__ import annotations

import json
import subprocess
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass

# (argv, timeout) -> (returncode, stdout, stderr)
Runner = Callable[[Sequence[str], float], tuple[int, str, str]]

DEFAULT_REPO = "jdrumgoole/SecantusDB"


def _subprocess_runner(argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
    try:
        proc = subprocess.run(list(argv), capture_output=True, text=True, timeout=timeout)
    except FileNotFoundError:
        return 127, "", "gh not found on PATH"
    except subprocess.TimeoutExpired:
        return 124, "", f"gh timed out after {timeout}s"
    except OSError as exc:  # pragma: no cover - defensive
        return 1, "", str(exc)
    return proc.returncode, proc.stdout, proc.stderr


@dataclass(frozen=True)
class WorkflowRun:
    name: str
    status: str  # queued | in_progress | completed
    conclusion: str  # success | failure | cancelled | skipped | "" while running
    branch: str
    event: str
    created_at: str
    url: str

    @property
    def bucket(self) -> str:
        """Coarse state for colouring: running | success | failure | other."""
        if self.status != "completed":
            return "running"
        if self.conclusion == "success":
            return "success"
        if self.conclusion in ("failure", "timed_out", "startup_failure"):
            return "failure"
        return "other"


class GitHubClient:
    def __init__(
        self,
        *,
        repo: str = DEFAULT_REPO,
        repo_root: str | None = None,
        runner: Runner | None = None,
        ttl: float = 30.0,
        timeout: float = 15.0,
    ) -> None:
        self.repo = repo
        self.repo_root = repo_root
        self._run = runner or _subprocess_runner
        self.ttl = ttl
        self.timeout = timeout
        self.last_error: str | None = None
        self._cache: dict[str, tuple[float, object]] = {}

    # -- plumbing ---------------------------------------------------------

    def _cached(self, key: str, produce: Callable[[], object]) -> object:
        now = time.monotonic()
        hit = self._cache.get(key)
        if hit is not None and (now - hit[0]) < self.ttl:
            return hit[1]
        value = produce()
        self._cache[key] = (now, value)
        return value

    def _gh_json(self, argv: Sequence[str]) -> object | None:
        code, out, err = self._run(argv, self.timeout)
        if code != 0:
            self.last_error = (err or out or f"gh exited {code}").strip().splitlines()[0][:200]
            return None
        try:
            return json.loads(out) if out.strip() else None
        except json.JSONDecodeError:
            self.last_error = "gh returned non-JSON output"
            return None

    # -- queries ----------------------------------------------------------

    def recent_runs(self, *, limit: int = 20) -> list[WorkflowRun]:
        """Most recent workflow runs across all workflows (bounded)."""
        limit = max(1, min(int(limit), 100))

        def produce() -> object:
            data = self._gh_json(
                [
                    "gh",
                    "run",
                    "list",
                    "--repo",
                    self.repo,
                    "--limit",
                    str(limit),
                    "--json",
                    "name,status,conclusion,headBranch,event,createdAt,url",
                ]
            )
            if not isinstance(data, list):
                return []
            self.last_error = None
            return [
                WorkflowRun(
                    name=str(r.get("name", "")),
                    status=str(r.get("status", "")),
                    conclusion=str(r.get("conclusion") or ""),
                    branch=str(r.get("headBranch", "")),
                    event=str(r.get("event", "")),
                    created_at=str(r.get("createdAt", "")),
                    url=str(r.get("url", "")),
                )
                for r in data
            ]

        result = self._cached(f"runs:{limit}", produce)
        return list(result) if isinstance(result, list) else []

    def available(self) -> bool:
        """Whether gh is usable (present + authenticated) — cached."""

        def produce() -> object:
            code, _out, err = self._run(["gh", "auth", "status"], self.timeout)
            if code != 0:
                self.last_error = (err or "gh not available").strip().splitlines()[0][:200]
            return code == 0

        return bool(self._cached("available", produce))


__all__ = ["GitHubClient", "WorkflowRun", "Runner", "DEFAULT_REPO"]
