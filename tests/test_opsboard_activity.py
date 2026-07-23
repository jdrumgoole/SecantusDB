"""Local-vs-CI origin flagging and GitHub workflow dispatch."""

from __future__ import annotations

import json
import os
from collections.abc import Sequence
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from secantus.opsboard import activity
from secantus.opsboard.app import create_app
from secantus.opsboard.github import GitHubClient, WorkflowRun

_TOKEN = "test-token-123"

_RUNS = [
    WorkflowRun("Tests", "completed", "success", "main", "push", "2026-07-23T18:00:00Z", "u1"),
    WorkflowRun(
        "Validate", "in_progress", "", "feat", "pull_request", "2026-07-23T19:00:00Z", "u2"
    ),
    WorkflowRun("Publish", "completed", "failure", "main", "push", "2026-07-23T17:00:00Z", "u3"),
]

_WORKFLOWS = [
    {"name": "Tests", "id": "1", "state": "active"},
    {"name": "Publish to PyPI", "id": "2", "state": "active"},
    {"name": "Release secantusdb binary", "id": "3", "state": "active"},
    {"name": "Disabled thing", "id": "4", "state": "disabled_manually"},
]


class _Job:
    """Minimal stand-in for a jobkit Job row."""

    def __init__(self, jid: int, task: str, status: str, started: float) -> None:
        self.id, self.task, self.status, self.started_at = jid, task, status, started
        self.target = "python"


# --------------------------------------------------------------------------- #
# Merge + origin
# --------------------------------------------------------------------------- #


def test_merge_flags_origin_and_orders_newest_first() -> None:
    jobs = [_Job(1, "test", "passed", 1_769_000_000.0)]
    feed = activity.merge(jobs, _RUNS, limit=10)
    origins = {a.label: a.origin for a in feed}
    assert origins["test"] == activity.LOCAL
    assert origins["Tests"] == activity.GITHUB
    assert origins["Publish"] == activity.GITHUB
    # Newest first across BOTH origins.
    assert [a.when for a in feed] == sorted((a.when for a in feed), reverse=True)


def test_ci_states_normalise_to_local_vocabulary() -> None:
    feed = {a.label: a.state for a in activity.from_runs(_RUNS)}
    assert feed["Tests"] == "passed"
    assert feed["Validate"] == "running"  # not completed
    assert feed["Publish"] == "failed"


def test_local_rows_link_to_the_job_page() -> None:
    feed = activity.from_jobs([_Job(7, "rust-gate", "running", 1.0)])
    assert feed[0].url == "/jobs/7"
    assert feed[0].is_local is True


def test_unparseable_timestamp_does_not_crash() -> None:
    bad = [WorkflowRun("X", "completed", "success", "m", "push", "not-a-date", "u")]
    feed = activity.from_runs(bad)
    assert feed[0].when == 0.0
    assert feed[0].when_text == ""


def test_merge_is_bounded() -> None:
    jobs = [_Job(i, f"t{i}", "passed", float(i)) for i in range(50)]
    assert len(activity.merge(jobs, _RUNS, limit=5)) == 5


# --------------------------------------------------------------------------- #
# Workflow listing + dispatch gating
# --------------------------------------------------------------------------- #


def _runner(dispatch_rc: int = 0):  # noqa: ANN202
    calls: list[list[str]] = []

    def run(argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
        calls.append(list(argv))
        if argv[:3] == ["gh", "workflow", "list"]:
            return 0, json.dumps(_WORKFLOWS), ""
        if argv[:3] == ["gh", "workflow", "run"]:
            return dispatch_rc, "", "" if dispatch_rc == 0 else "boom"
        if argv[:3] == ["gh", "run", "list"]:
            return 0, "[]", ""
        return 0, "[]", ""

    run.calls = calls  # type: ignore[attr-defined]
    return run


def test_workflows_filters_disabled_and_flags_release_class() -> None:
    gh = GitHubClient(runner=_runner())
    names = {w.name: w for w in gh.workflows()}
    assert "Disabled thing" not in names  # inactive filtered out
    assert names["Tests"].release_class is False
    assert names["Publish to PyPI"].release_class is True
    assert names["Release secantusdb binary"].release_class is True


def test_dispatch_failure_is_reported_not_raised() -> None:
    gh = GitHubClient(runner=_runner(dispatch_rc=1))
    ok, msg = gh.dispatch("Tests")
    assert ok is False
    assert "boom" in msg


@pytest.fixture
def client(tmp_path: Path) -> TestClient:
    app = create_app(
        repo_root=tmp_path,
        token=_TOKEN,
        journal_path=tmp_path / "opsboard.db",
        github_client=GitHubClient(runner=_runner()),
    )
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    return c


def test_dispatch_safe_workflow_needs_no_confirmation(client: TestClient) -> None:
    r = client.post("/ci/dispatch", data={"workflow": "Tests"}, follow_redirects=False)
    assert r.status_code == 303


def test_dispatch_publishing_workflow_requires_exact_name(client: TestClient) -> None:
    r = client.post(
        "/ci/dispatch",
        data={"workflow": "Publish to PyPI", "confirm": "yes"},
        follow_redirects=False,
    )
    assert r.status_code == 400
    assert "publishes" in r.json()["detail"]

    ok = client.post(
        "/ci/dispatch",
        data={"workflow": "Publish to PyPI", "confirm": "Publish to PyPI"},
        follow_redirects=False,
    )
    assert ok.status_code == 303


def test_dispatch_unknown_workflow_404s(client: TestClient) -> None:
    r = client.post("/ci/dispatch", data={"workflow": "nope"}, follow_redirects=False)
    assert r.status_code == 404


def test_ci_page_shows_origin_badges(client: TestClient) -> None:
    journal = client.app.state.journal
    journal.create(target="python", task="test", argv=["test"], worktree="/w", host_pid=1)
    body = client.get("/ci").text
    assert "origin-local" in body
    assert "Start a CI run" in body


# --------------------------------------------------------------------------- #
# Running vs history separation
# --------------------------------------------------------------------------- #


def test_history_excludes_running_jobs(tmp_path: Path) -> None:
    from secantus.jobkit import Journal

    journal = Journal(tmp_path / "j.db")
    done = journal.create(target="python", task="done", argv=["d"], worktree="/w", host_pid=1)
    journal.finish(done, 0)
    journal.create(target="python", task="live", argv=["l"], worktree="/w", host_pid=1)

    all_jobs, _ = journal.list()
    history, _ = journal.list(include_running=False)
    assert {j.task for j in all_jobs} == {"done", "live"}
    assert {j.task for j in history} == {"done"}  # in-flight excluded
    assert {j.task for j in journal.running()} == {"live"}


def test_history_pagination_still_excludes_running(tmp_path: Path) -> None:
    from secantus.jobkit import Journal

    journal = Journal(tmp_path / "j.db")
    ids = []
    for i in range(6):
        jid = journal.create(
            target="python", task=f"t{i}", argv=[f"t{i}"], worktree="/w", host_pid=1
        )
        ids.append(jid)
        if i % 2 == 0:
            journal.finish(jid, 0)  # even ones finish; odd stay running

    page1, cursor = journal.list(limit=2, include_running=False)
    assert all(j.status != "running" for j in page1)
    page2, _ = journal.list(limit=2, before_id=cursor, include_running=False)
    # The cursor page must also filter — not just the first page.
    assert all(j.status != "running" for j in page2)
    assert not ({j.id for j in page1} & {j.id for j in page2})  # no overlap


def test_jobs_page_separates_running_from_history(client: TestClient) -> None:
    journal = client.app.state.journal
    done = journal.create(target="python", task="olddone", argv=["o"], worktree="/w", host_pid=1)
    journal.finish(done, 0)
    journal.create(
        target="python", task="inflight", argv=["i"], worktree="/w", host_pid=os.getpid()
    )

    body = client.get("/jobs").text
    assert "Running now" in body
    assert "History" in body
    assert "inflight" in body and "olddone" in body


def test_running_partial_polls_and_stands_alone(client: TestClient) -> None:
    r = client.get("/jobs/running")
    assert r.status_code == 200
    assert 'id="running-block"' in r.text
    assert 'hx-get="/jobs/running"' in r.text
