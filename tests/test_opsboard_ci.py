"""CI-observation tests. Fully hermetic — the gh runner is injected, so these
never shell out to gh, never authenticate, and never touch the network."""

from __future__ import annotations

import json
from collections.abc import Sequence
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from secantus.opsboard import versions
from secantus.opsboard.app import create_app
from secantus.opsboard.github import GitHubClient

_TOKEN = "test-token-123"

_RUNS = [
    {
        "name": "Tests",
        "status": "completed",
        "conclusion": "success",
        "headBranch": "main",
        "event": "push",
        "createdAt": "2026-07-23T18:00:00Z",
        "url": "https://github.com/x/y/actions/runs/1",
    },
    {
        "name": "Validate",
        "status": "in_progress",
        "conclusion": None,
        "headBranch": "opsboard-ci",
        "event": "pull_request",
        "createdAt": "2026-07-23T18:05:00Z",
        "url": "https://github.com/x/y/actions/runs/2",
    },
    {
        "name": "Publish",
        "status": "completed",
        "conclusion": "failure",
        "headBranch": "main",
        "event": "push",
        "createdAt": "2026-07-23T17:00:00Z",
        "url": "https://github.com/x/y/actions/runs/3",
    },
]


def _ok_runner(payload: list[dict]) -> object:
    calls: list[list[str]] = []

    def run(argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
        calls.append(list(argv))
        if argv[:3] == ["gh", "auth", "status"]:
            return 0, "logged in", ""
        return 0, json.dumps(payload), ""

    run.calls = calls  # type: ignore[attr-defined]
    return run


def _failing_runner(code: int = 127, err: str = "gh not found on PATH") -> object:
    def run(argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
        return code, "", err

    return run


# --------------------------------------------------------------------------- #
# GitHubClient
# --------------------------------------------------------------------------- #


def test_recent_runs_parses_and_buckets() -> None:
    gh = GitHubClient(runner=_ok_runner(_RUNS))
    runs = gh.recent_runs(limit=10)
    assert [r.name for r in runs] == ["Tests", "Validate", "Publish"]
    assert [r.bucket for r in runs] == ["success", "running", "failure"]
    assert runs[1].conclusion == ""  # null conclusion → empty, not "None"


def test_missing_gh_degrades_without_raising() -> None:
    gh = GitHubClient(runner=_failing_runner())
    assert gh.recent_runs() == []
    assert gh.available() is False
    assert gh.last_error and "not found" in gh.last_error


def test_bad_json_degrades() -> None:
    def run(argv: Sequence[str], timeout: float) -> tuple[int, str, str]:
        return 0, "not json", ""

    gh = GitHubClient(runner=run)
    assert gh.recent_runs() == []
    assert gh.last_error == "gh returned non-JSON output"


def test_results_are_cached_within_ttl() -> None:
    runner = _ok_runner(_RUNS)
    gh = GitHubClient(runner=runner, ttl=60.0)
    gh.recent_runs(limit=5)
    gh.recent_runs(limit=5)
    gh.recent_runs(limit=5)
    # One gh invocation despite three calls — a 1s UI poll must not spawn a
    # process per tick.
    assert len(runner.calls) == 1  # type: ignore[attr-defined]


def test_limit_is_bounded() -> None:
    runner = _ok_runner(_RUNS)
    gh = GitHubClient(runner=runner)
    gh.recent_runs(limit=10_000)
    argv = runner.calls[0]  # type: ignore[attr-defined]
    assert "--limit" in argv
    assert argv[argv.index("--limit") + 1] == "100"  # clamped, never unbounded


# --------------------------------------------------------------------------- #
# Version drift (local only — no network)
# --------------------------------------------------------------------------- #


def test_versions_read_from_working_tree(tmp_path: Path) -> None:
    pkg = tmp_path / "src" / "secantus"
    pkg.mkdir(parents=True)
    (pkg / "__init__.py").write_text('__version__ = "0.6.0b1"\n', encoding="utf-8")
    crate = tmp_path / "crates" / "secantus-server"
    crate.mkdir(parents=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "x"\nversion = "0.5.2-beta.16"\n', encoding="utf-8"
    )
    assert versions.python_version(tmp_path) == "0.6.0b1"
    assert versions.rust_version(tmp_path) == "0.5.2-beta.16"


def test_missing_files_do_not_raise(tmp_path: Path) -> None:
    assert versions.python_version(tmp_path) == ""
    assert versions.rust_version(tmp_path) == ""


def test_drift_states(tmp_path: Path) -> None:
    def fake_git(argv: Sequence[str], cwd: str) -> tuple[int, str]:
        return 0, "v0.6.0b1\nv0.6.0b0\n" if "v*" in argv else "secantusdb-v0.5.2-beta.15\n"

    pkg = tmp_path / "src" / "secantus"
    pkg.mkdir(parents=True)
    (pkg / "__init__.py").write_text('__version__ = "0.6.0b1"\n', encoding="utf-8")
    infos = {v.target: v for v in versions.collect(tmp_path, runner=fake_git)}
    assert infos["python"].latest_tag == "v0.6.0b1"
    assert infos["python"].drift == "clean"  # tree matches the tag
    assert infos["rust"].drift == "unknown"  # no Cargo.toml → local unknown


# --------------------------------------------------------------------------- #
# Page
# --------------------------------------------------------------------------- #


@pytest.fixture
def client(tmp_path: Path) -> TestClient:
    app = create_app(
        repo_root=tmp_path,
        token=_TOKEN,
        journal_path=tmp_path / "opsboard.db",
        github_client=GitHubClient(runner=_ok_runner(_RUNS)),
    )
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    return c


def test_ci_page_lists_runs(client: TestClient) -> None:
    body = client.get("/ci").text
    for name in ("Tests", "Validate", "Publish"):
        assert name in body
    assert "Version drift" in body
    assert "opsboard-ci" in body  # a parallel session's branch is visible


def test_ci_page_shows_helpful_message_when_gh_unavailable(tmp_path: Path) -> None:
    app = create_app(
        repo_root=tmp_path,
        token=_TOKEN,
        journal_path=tmp_path / "j.db",
        github_client=GitHubClient(runner=_failing_runner()),
    )
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    body = c.get("/ci").text
    assert c.get("/ci").status_code == 200  # degrades, never 500s
    assert "gh auth login" in body


def test_ci_page_requires_token(tmp_path: Path) -> None:
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    assert TestClient(app).get("/ci").status_code == 401


def test_nav_links_to_ci(client: TestClient) -> None:
    assert 'href="/ci"' in client.get("/").text
