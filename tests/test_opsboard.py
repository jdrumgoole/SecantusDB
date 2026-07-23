"""Tests for the Ops Board web app.

Drives the real FastAPI app via Starlette's ``TestClient`` and the real jobkit
runner (with a fake ``invoke`` child via ``SECANTUS_OPSBOARD_INVOKE``) — no
mock. Also enforces the pywebview "no runtime CDN" rule.
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from secantus.opsboard.app import create_app

_TOKEN = "test-token-123"
_REPO_ROOT = Path(__file__).resolve().parents[1]
_TEMPLATES = _REPO_ROOT / "src/secantus/opsboard/templates"
_STATIC = _REPO_ROOT / "src/secantus/opsboard/static"

# Written to a FILE (not python -c) so the override round-trips through shlex on
# Windows (where -c quoting would break the split).
_FAKE_INVOKE_FILE = """\
import sys

args = sys.argv[1:]
print("CHILD-RAN", *args)
code = int(args[-1]) if args and args[-1].lstrip("-").isdigit() else 0
sys.exit(code)
"""


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SECANTUS_OPSBOARD_DB", str(tmp_path / "opsboard.db"))
    monkeypatch.setenv("SECANTUS_OPSBOARD_LOGS", str(tmp_path / "logs"))
    fake = tmp_path / "fake_invoke.py"
    fake.write_text(_FAKE_INVOKE_FILE)
    monkeypatch.setenv("SECANTUS_OPSBOARD_INVOKE", f"{sys.executable} {fake}")


@pytest.fixture
def client(tmp_path: Path) -> TestClient:
    # repo_root is the real checkout so the spawned ``python -m secantus.jobkit``
    # child can import ``secantus`` (relative PYTHONPATH=src resolves from here).
    # The journal, logs, and the invoke child are all faked/redirected to tmp,
    # so nothing real runs against the checkout.
    app = create_app(repo_root=_REPO_ROOT, token=_TOKEN, journal_path=tmp_path / "opsboard.db")
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    return c


# --------------------------------------------------------------------------- #
# Auth + health
# --------------------------------------------------------------------------- #


def test_healthz_is_unauthenticated(tmp_path: Path) -> None:
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    c = TestClient(app)  # no token header
    r = c.get("/healthz")
    assert r.status_code == 200
    assert r.json()["ok"] is True


def test_dashboard_requires_token(tmp_path: Path) -> None:
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    c = TestClient(app)
    assert c.get("/").status_code == 401


def test_dashboard_renders_all_targets(client: TestClient) -> None:
    r = client.get("/")
    assert r.status_code == 200
    body = r.text
    assert "Python server" in body
    assert "Rust server" in body
    assert "PostgreSQL server" in body
    # A non-confirm task is a live button; a release task is disabled.
    assert 'value="py-test"' in body
    assert "disabled" in body  # release-class buttons


# --------------------------------------------------------------------------- #
# pywebview rule: vendor everything, no runtime CDN
# --------------------------------------------------------------------------- #


def test_no_runtime_cdn_dependencies_in_templates() -> None:
    forbidden = ("cdn.jsdelivr.net", "unpkg.com", "cdnjs.cloudflare.com", "https://")
    for tmpl in _TEMPLATES.rglob("*.html"):
        # utf-8 explicitly: templates carry non-ASCII glyphs (⏱ ⇉ ✓ …) that a
        # Windows locale (cp1252) default would fail to decode.
        body = tmpl.read_text(encoding="utf-8")
        for needle in forbidden:
            assert needle not in body, f"{tmpl.name} references {needle}"


def test_vendored_assets_exist_on_disk() -> None:
    assert (_STATIC / "js/htmx.min.js").is_file()
    assert (_STATIC / "css/opsboard.css").is_file()


# --------------------------------------------------------------------------- #
# Job lifecycle through the web app
# --------------------------------------------------------------------------- #


def test_start_job_runs_and_appears_in_history(client: TestClient) -> None:
    r = client.post("/jobs/start", data={"task_key": "py-test"}, follow_redirects=True)
    assert r.status_code == 200  # followed 303 → job detail page
    assert "Job #" in r.text or "logbox" in r.text

    # It shows in the paginated history.
    listing = client.get("/jobs")
    assert listing.status_code == 200
    assert "<code>test</code>" in listing.text


def test_start_unknown_task_404s(client: TestClient) -> None:
    r = client.post("/jobs/start", data={"task_key": "nope"}, follow_redirects=False)
    assert r.status_code == 404


def test_all_gauges_tasks_registered_per_server() -> None:
    from secantus.opsboard import registry

    py = registry.resolve_task("py-gauge-all")
    rs = registry.resolve_task("rs-gauge-all")
    assert py is not None and py[1].argv == ["validate-all", "--server", "python"]
    assert rs is not None and rs[1].argv == ["validate-all", "--server", "rust"]
    assert py[1].jobs_option is True and rs[1].jobs_option is True
    # Exact argv match distinguishes the two despite sharing argv[0].
    assert registry.find_task_by_argv(["validate-all", "--server", "rust"]).key == "rs-gauge-all"


def test_start_all_gauges_passes_parallelism(client: TestClient) -> None:
    client.post(
        "/jobs/start",
        data={"task_key": "py-gauge-all", "jobs": "6"},
        follow_redirects=False,
    )
    job = client.app.state.journal.list(limit=1)[0][0]
    assert job.argv == ["validate-all", "--server", "python", "--jobs", "6"]


def test_parallelism_is_capped(client: TestClient) -> None:
    client.post(
        "/jobs/start",
        data={"task_key": "py-gauge-all", "jobs": "999"},
        follow_redirects=False,
    )
    job = client.app.state.journal.list(limit=1)[0][0]
    assert job.argv[-2:] == ["--jobs", "16"]  # clamped to _MAX_JOBS


def test_jobs_ignored_for_non_jobs_task(client: TestClient) -> None:
    # A task without jobs_option must not get a stray --jobs appended.
    client.post("/jobs/start", data={"task_key": "py-test", "jobs": "8"}, follow_redirects=False)
    job = client.app.state.journal.list(limit=1)[0][0]
    assert "--jobs" not in job.argv


def test_dashboard_renders_info_dialog_per_task(client: TestClient) -> None:
    from secantus.opsboard import registry

    body = client.get("/").text
    for target in registry.TARGETS:
        for task in target.tasks:
            assert f'id="dlg-{task.key}"' in body, f"no info dialog for {task.key}"
    # The dialog carries the long-form detail and the exact command.
    assert "pytest-xdist" in body  # py-test detail
    assert "./inv validate-all --server python" in body


def test_every_task_has_detail_and_estimate() -> None:
    from secantus.opsboard import registry

    for target in registry.TARGETS:
        for task in target.tasks:
            assert task.detail, f"{task.key} has no detail text"
            assert task.est_seconds > 0, f"{task.key} has no fallback estimate"


def test_dashboard_estimate_prefers_measured_history(client: TestClient) -> None:
    journal = client.app.state.journal
    # Record two fast successful runs of `test` — the dashboard should quote
    # the measured median, not the registry's rough 5m figure.
    for _ in range(3):
        jid = journal.create(
            target="python",
            task="test",
            argv=["test"],
            worktree="/w",
            host_pid=1,
            started_at=1000.0,
        )
        journal.finish(jid, 0, ended_at=1010.0)

    body = client.get("/").text
    assert "median of the last 3 successful runs" in body


def test_dashboard_shows_parallelism_input(client: TestClient) -> None:
    body = client.get("/").text
    assert 'name="jobs"' in body
    assert 'value="py-gauge-all"' in body


def test_release_task_requires_confirmation(client: TestClient) -> None:
    # Without confirm=yes → rejected.
    r = client.post("/jobs/start", data={"task_key": "py-release-prepare"}, follow_redirects=False)
    assert r.status_code == 400


def test_job_log_tail_captures_child_output(client: TestClient) -> None:
    client.post("/jobs/start", data={"task_key": "py-test"}, follow_redirects=False)
    # Find the job id from history.
    journal = client.app.state.journal
    # Let the fast child finish and be reaped.
    deadline = time.monotonic() + 5
    jobs = journal.list(limit=1)[0]
    while time.monotonic() < deadline and (not jobs or jobs[0].running):
        time.sleep(0.05)
        jobs = journal.list(limit=1)[0]
    job = jobs[0]
    r = client.get(f"/jobs/{job.id}/log")
    assert r.status_code == 200
    assert "CHILD-RAN" in r.text
    # Done → no more polling trigger emitted.
    assert "every 1s" not in r.text


def test_job_view_renders_progress_and_stepper(client: TestClient, tmp_path: Path) -> None:
    journal = client.app.state.journal
    jid = journal.create(
        target="python", task="py-gate", argv=["py-gate"], worktree="/w", host_pid=1
    )
    log = tmp_path / "job.log"
    log.write_text("==> [1/3] Lint\nlinting\n==> [2/3] Tests\ntests/x PASSED [ 60%]\n")
    journal.set_log_path(jid, str(log))

    r = client.get(f"/jobs/{jid}/view")
    assert r.status_code == 200
    body = r.text
    # Phase stepper with registry-supplied labels (incl. the not-yet-reached one).
    assert "Lint" in body and "Tests" in body and "Perf" in body
    # Determinate overall bar rendered a percentage.
    assert "%" in body
    # Still running → keeps polling.
    assert "every 1s" in body


def test_job_view_stops_polling_when_done(client: TestClient, tmp_path: Path) -> None:
    journal = client.app.state.journal
    jid = journal.create(target="python", task="test", argv=["test"], worktree="/w", host_pid=1)
    log = tmp_path / "job.log"
    log.write_text("all good [ 100%]\n")
    journal.set_log_path(jid, str(log))
    journal.finish(jid, 0)

    r = client.get(f"/jobs/{jid}/view")
    assert r.status_code == 200
    assert "every 1s" not in r.text  # done → no repoll


def test_cancel_all_endpoint_redirects(client: TestClient) -> None:
    r = client.post("/jobs/cancel-all", follow_redirects=False)
    assert r.status_code == 303
    assert "/jobs" in r.headers["location"]


def test_jobs_page_shows_cancel_all_when_running(client: TestClient) -> None:
    journal = client.app.state.journal
    journal.create(target="python", task="t", argv=["t"], worktree="/w", host_pid=os.getpid())
    page = client.get("/jobs")
    assert "Cancel all running" in page.text


def test_jobs_pagination(client: TestClient) -> None:
    journal = client.app.state.journal
    ids = [
        journal.create(target="python", task=f"t{i}", argv=[f"t{i}"], worktree="/w", host_pid=1)
        for i in range(3)
    ]
    # Finish them so reap_stale (host_pid=1 is alive=init) doesn't matter.
    for jid in ids:
        journal.finish(jid, 0)

    page = client.get("/jobs")
    assert page.status_code == 200
    # First page rendered; a "Load older" control appears only when more remain.
    # With 3 rows and page size 50, everything fits on one page.
    assert "t2" in page.text and "t0" in page.text
