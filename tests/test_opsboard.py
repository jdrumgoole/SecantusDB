"""Tests for the Ops Board web app.

Drives the real FastAPI app via Starlette's ``TestClient`` and the real jobkit
runner (with a fake ``invoke`` child via ``SECANTUS_OPSBOARD_INVOKE``) — no
mock. Also enforces the pywebview "no runtime CDN" rule.
"""

from __future__ import annotations

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
        body = tmpl.read_text()
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
