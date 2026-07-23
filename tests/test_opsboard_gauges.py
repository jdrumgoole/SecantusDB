"""Tests for the gauge matrix page (13 gauges × 2 servers, data-driven)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from secantus.opsboard import registry
from secantus.opsboard.app import create_app

_TOKEN = "test-token-123"
_REPO_ROOT = Path(__file__).resolve().parents[1]

_FAKE_INVOKE_FILE = """\
import sys

print("CHILD-RAN", *sys.argv[1:])
sys.exit(0)
"""


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SECANTUS_OPSBOARD_DB", str(tmp_path / "opsboard.db"))
    monkeypatch.setenv("SECANTUS_OPSBOARD_LOGS", str(tmp_path / "logs"))
    fake = tmp_path / "fake_invoke.py"
    fake.write_text(_FAKE_INVOKE_FILE, encoding="utf-8")
    monkeypatch.setenv("SECANTUS_OPSBOARD_INVOKE", f"{sys.executable} {fake}")


@pytest.fixture
def client(tmp_path: Path) -> TestClient:
    app = create_app(repo_root=_REPO_ROOT, token=_TOKEN, journal_path=tmp_path / "opsboard.db")
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    return c


# --------------------------------------------------------------------------- #
# Catalog
# --------------------------------------------------------------------------- #


def test_thirteen_gauges_declared() -> None:
    assert len(registry.GAUGES) == 13
    keys = [g.key for g in registry.GAUGES]
    assert len(set(keys)) == 13  # unique
    for expected in ("pymongo", "go", "node", "java", "kotlin", "ruby", "rust", "c"):
        assert expected in keys


def test_every_gauge_has_a_task_per_server() -> None:
    for spec in registry.GAUGES:
        for server, _name in registry.SERVERS:
            task = registry.gauge_task(spec.key, server)
            assert task is not None, f"{spec.key}/{server} missing"
            assert task.argv == [spec.task, "--server", server]
            assert task.detail
            assert task.est_seconds > 0


def test_gauge_tasks_are_startable_by_key() -> None:
    # resolve_task must find generated gauge tasks, else /jobs/start 404s.
    for spec in registry.GAUGES:
        for server, _name in registry.SERVERS:
            assert registry.resolve_task(f"gauge-{spec.key}-{server}") is not None


# --------------------------------------------------------------------------- #
# Page
# --------------------------------------------------------------------------- #


def test_gauges_page_lists_every_gauge_and_server(client: TestClient) -> None:
    body = client.get("/gauges").text
    for spec in registry.GAUGES:
        assert spec.label in body
        assert f'id="dlg-gauge-{spec.key}"' in body
        for server, _name in registry.SERVERS:
            assert f'value="gauge-{spec.key}-{server}"' in body
    assert "Python server" in body and "Rust server" in body


def test_gauges_page_requires_token(tmp_path: Path) -> None:
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    assert TestClient(app).get("/gauges").status_code == 401


def test_starting_a_gauge_uses_the_right_server(client: TestClient) -> None:
    client.post("/jobs/start", data={"task_key": "gauge-go-rust"}, follow_redirects=False)
    job = client.app.state.journal.list(limit=1)[0][0]
    assert job.argv == ["validate-go", "--server", "rust"]
    assert job.target == "rust"  # infer_target read the --server flag


def test_gauges_page_shows_scores_when_reports_exist(client: TestClient, tmp_path: Path) -> None:
    docs = Path(client.app.state.repo_root) / "docs"
    # The real checkout has reports; assert a score renders rather than "no report".
    if (docs / "validation-report.md").is_file():
        body = client.get("/gauges").text
        assert "score" in body
        assert "no report" not in body or body.count("no report") < 26


def test_nav_links_to_gauges(client: TestClient) -> None:
    assert 'href="/gauges"' in client.get("/").text
