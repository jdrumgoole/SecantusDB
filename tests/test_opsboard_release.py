"""Release readiness + the confirm gate, and Tier-3 process discovery.

The gate tests matter most: a release pushes a tag that triggers publication, so
every refusal path is asserted explicitly. Nothing here ever starts a real
release — the invoke child is faked.
"""

from __future__ import annotations

import sys
from collections.abc import Sequence
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from secantus.opsboard import discovery, readiness
from secantus.opsboard.app import create_app

_TOKEN = "test-token-123"

_FAKE_INVOKE_FILE = """\
import sys

print("CHILD-RAN", *sys.argv[1:])
sys.exit(0)
"""


def _ready_repo(tmp_path: Path) -> Path:
    """A tree that passes every local readiness check."""
    (tmp_path / "changelog.d").mkdir(parents=True, exist_ok=True)
    (tmp_path / "changelog.d" / "some-change.md").write_text("### x\n", encoding="utf-8")
    return tmp_path


def _git_ok(argv: Sequence[str], cwd: str) -> tuple[int, str]:
    if argv[0] == "branch":
        return 0, "main\n"
    if argv[0] == "status":
        return 0, " M vendor/wiredtiger\n"  # tolerated drift only
    if argv[0] == "rev-list":
        return 0, "0\t0\n"
    return 1, ""


def _git_dirty(argv: Sequence[str], cwd: str) -> tuple[int, str]:
    if argv[0] == "branch":
        return 0, "feature-x\n"
    if argv[0] == "status":
        return 0, " M src/secantus/server.py\n"
    if argv[0] == "rev-list":
        return 0, "2\t3\n"
    return 1, ""


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SECANTUS_OPSBOARD_DB", str(tmp_path / "opsboard.db"))
    monkeypatch.setenv("SECANTUS_OPSBOARD_LOGS", str(tmp_path / "logs"))
    fake = tmp_path / "fake_invoke.py"
    fake.write_text(_FAKE_INVOKE_FILE, encoding="utf-8")
    monkeypatch.setenv("SECANTUS_OPSBOARD_INVOKE", f"{sys.executable} {fake}")


@pytest.fixture
def client(tmp_path: Path) -> TestClient:
    repo = _ready_repo(tmp_path)
    app = create_app(
        repo_root=repo,
        token=_TOKEN,
        journal_path=tmp_path / "opsboard.db",
        readiness_runner=_git_ok,
    )
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    return c


# --------------------------------------------------------------------------- #
# Readiness
# --------------------------------------------------------------------------- #


def test_ready_tree_has_no_blockers(tmp_path: Path) -> None:
    checks = readiness.collect(_ready_repo(tmp_path), runner=_git_ok)
    assert readiness.blockers(checks) == []
    by_name = {c.name: c for c in checks}
    assert by_name["On main"].state == "ok"
    assert by_name["Working tree clean"].state == "ok"  # vendor drift tolerated
    assert by_name["In sync with origin"].state == "ok"
    assert by_name["Changelog fragments"].state == "ok"


def test_dirty_tree_and_wrong_branch_block(tmp_path: Path) -> None:
    checks = readiness.collect(_ready_repo(tmp_path), runner=_git_dirty)
    names = {c.name for c in readiness.blockers(checks)}
    assert "On main" in names
    assert "Working tree clean" in names
    assert "In sync with origin" in names


def test_missing_changelog_fragment_blocks(tmp_path: Path) -> None:
    (tmp_path / "changelog.d").mkdir()
    (tmp_path / "changelog.d" / "README.md").write_text("readme", encoding="utf-8")
    checks = readiness.collect(tmp_path, runner=_git_ok)
    assert "Changelog fragments" in {c.name for c in readiness.blockers(checks)}


def test_unknown_blocking_check_fails_safe(tmp_path: Path) -> None:
    # Nothing verifiable: no git, no changelog.d. Every blocking check is
    # "unknown" — which must BLOCK, not silently permit an irreversible release.
    checks = readiness.collect(tmp_path, runner=lambda argv, cwd: (1, ""))
    blocking = readiness.blockers(checks)
    assert {c.name for c in blocking} >= {
        "On main",
        "Working tree clean",
        "In sync with origin",
        "Changelog fragments",
    }
    assert all(c.state == "unknown" for c in blocking)


def test_ci_check_is_advisory_and_never_blocks(tmp_path: Path) -> None:
    class _Failing:
        def recent_runs(self, limit: int = 20):  # noqa: ANN201
            class R:
                branch, bucket, name = "main", "failure", "Tests"

            return [R()]

    checks = readiness.collect(_ready_repo(tmp_path), github=_Failing(), runner=_git_ok)
    ci = next(c for c in checks if c.name == "Recent CI on main")
    assert ci.state == "bad"
    assert ci.blocking is False
    assert readiness.blockers(checks) == []  # advisory → does not block


# --------------------------------------------------------------------------- #
# The confirm gate — every refusal path
# --------------------------------------------------------------------------- #


def _post(client: TestClient, **data: str):  # noqa: ANN201
    return client.post("/release/start", data=data, follow_redirects=False)


def test_release_requires_matching_typed_confirmation(client: TestClient) -> None:
    r = _post(client, task_key="py-release-prepare", version="0.6.0", confirm="yes")
    assert r.status_code == 400
    assert "exactly match" in r.json()["detail"]


def test_release_rejects_bad_version(client: TestClient) -> None:
    r = _post(client, task_key="py-release-prepare", version="not-a-version", confirm="x")
    assert r.status_code == 400
    assert "X.Y.Z" in r.json()["detail"]


def test_release_rejects_mismatched_confirmation(client: TestClient) -> None:
    r = _post(client, task_key="py-release-prepare", version="0.6.0", confirm="0.6.1")
    assert r.status_code == 400


def test_release_rejects_non_release_task(client: TestClient) -> None:
    r = _post(client, task_key="py-test", version="0.6.0", confirm="0.6.0")
    assert r.status_code == 400
    assert "not a release-class task" in r.json()["detail"]


def test_release_rejects_unknown_task(client: TestClient) -> None:
    assert _post(client, task_key="nope", confirm="x").status_code == 404


def test_release_starts_when_everything_matches(client: TestClient) -> None:
    r = _post(client, task_key="py-release-prepare", version="0.6.0", confirm="0.6.0")
    assert r.status_code == 303
    job = client.app.state.journal.list(limit=1)[0][0]
    assert job.argv == ["release-prepare", "0.6.0"]  # version passed through


def test_blocking_checks_stop_release_without_override(tmp_path: Path) -> None:
    # No changelog.d at all → blocking check fails.
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    r = c.post(
        "/release/start",
        data={"task_key": "py-release-prepare", "version": "0.6.0", "confirm": "0.6.0"},
        follow_redirects=False,
    )
    # Either blocked by readiness (expected) — never a silent success.
    assert r.status_code == 400
    assert "readiness" in r.json()["detail"]


def test_override_allows_release_despite_blockers(tmp_path: Path) -> None:
    # Explicit override is the only way past a blocking check — and it must be
    # deliberate, alongside the typed confirmation.
    app = create_app(repo_root=tmp_path, token=_TOKEN, journal_path=tmp_path / "j.db")
    c = TestClient(app)
    c.headers.update({"X-Opsboard-Token": _TOKEN})
    r = c.post(
        "/release/start",
        data={
            "task_key": "py-release-prepare",
            "version": "0.6.0",
            "confirm": "0.6.0",
            "override": "yes",
        },
        follow_redirects=False,
    )
    assert r.status_code == 303


def test_release_page_renders(client: TestClient) -> None:
    body = client.get("/release").text
    assert "Readiness" in body
    assert "irreversible" in body
    assert "type the version to confirm" in body


# --------------------------------------------------------------------------- #
# Tier-3 discovery
# --------------------------------------------------------------------------- #

_PS = """\
  101   05:00 /usr/bin/python -m pytest tests/
  102 1-02:03 cargo test --workspace
  103   00:10 /usr/bin/ssh somewhere
  104   00:20 python -m secantus.opsboard
"""


def test_scan_finds_build_tools_only() -> None:
    procs = discovery.scan(runner=lambda argv: (0, _PS))
    pids = [p.pid for p in procs]
    assert 101 in pids and 102 in pids  # pytest + cargo
    assert 103 not in pids  # unrelated process ignored
    assert 104 not in pids  # the board itself excluded


def test_scan_filters_known_tracked_pids() -> None:
    procs = discovery.scan(known_pids=[101], runner=lambda argv: (0, _PS))
    assert 101 not in [p.pid for p in procs]  # already tracked → not duplicated


def test_scan_degrades_when_ps_fails() -> None:
    assert discovery.scan(runner=lambda argv: (1, "")) == []


def test_scan_short_circuits_on_windows_only_for_real_ps(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # With the REAL ps (no runner) Windows returns nothing — `ps` doesn't exist
    # there. With an injected runner there's no platform dependency, so parsing
    # must still work (this is what broke the Windows CI shard).
    monkeypatch.setattr(discovery.os, "name", "nt")
    assert discovery.scan() == []
    assert discovery.scan(runner=lambda argv: (0, _PS)) != []


def test_scan_respects_limit() -> None:
    assert len(discovery.scan(runner=lambda argv: (0, _PS), limit=1)) == 1
