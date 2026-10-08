"""``release-prepare`` survives ``main`` moving while its tests run.

The release commit and the tag used to go out in one ``git push origin main
vX.Y.Z``. That push is not atomic: on 0.7.0b1 and again on 0.7.0b2 another PR
merged during the ~30 minute test run, the tag was accepted and ``main`` was
rejected, and a published tag pointed at a commit ``main`` did not have.

These drive ``tasks._push_release`` against real git repositories: a bare
origin, the release checkout, and a second clone playing the other session.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
import tasks  # noqa: E402

VERSION = "9.9.9b1"
TAG = f"v{VERSION}"


def _git(cwd: pathlib.Path, *args: str) -> str:
    done = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True)
    return done.stdout.strip()


def _clone(origin: pathlib.Path, dest: pathlib.Path) -> pathlib.Path:
    subprocess.run(["git", "clone", "-q", str(origin), str(dest)], check=True, capture_output=True)
    _git(dest, "config", "user.email", "release@example.invalid")
    _git(dest, "config", "user.name", "release test")
    return dest


def _commit(repo: pathlib.Path, name: str, text: str, message: str) -> str:
    (repo / name).write_text(text)
    _git(repo, "add", name)
    _git(repo, "commit", "-q", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


@pytest.fixture
def repos(tmp_path: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    """``(origin, release checkout, other session's clone)``, the release
    checkout holding an unpushed release commit and its local tag."""
    origin = tmp_path / "origin.git"
    subprocess.run(
        ["git", "init", "-q", "--bare", "-b", "main", str(origin)], check=True, capture_output=True
    )
    seed = _clone(origin, tmp_path / "seed")
    _git(seed, "checkout", "-q", "-b", "main")
    _commit(seed, "version.txt", "1\n", "seed")
    _git(seed, "push", "-q", "origin", "main")

    release = _clone(origin, tmp_path / "release")
    other = _clone(origin, tmp_path / "other")
    _commit(release, "version.txt", "2\n", f"Release {TAG}")
    _git(release, "tag", "-a", TAG, "-m", f"Release {TAG}")
    return origin, release, other


def _origin_tag_commit(origin: pathlib.Path) -> str | None:
    listed = _git(origin, "tag", "-l", TAG)
    return _git(origin, "rev-parse", f"{TAG}^{{commit}}") if listed else None


def test_quiet_main_pushes_commit_then_tag(repos) -> None:
    origin, release, _ = repos
    release_commit = _git(release, "rev-parse", "HEAD")

    tasks._push_release(VERSION, cwd=str(release))

    assert _git(origin, "rev-parse", "main") == release_commit
    assert _origin_tag_commit(origin) == release_commit


def test_main_moved_during_the_run_is_merged_and_the_tag_stays_put(repos) -> None:
    """The 0.7.0b1 / 0.7.0b2 case: another PR landed while the tests ran."""
    origin, release, other = repos
    release_commit = _git(release, "rev-parse", "HEAD")
    theirs = _commit(other, "crates.txt", "bumped\n", "another session's PR")
    _git(other, "push", "-q", "origin", "main")

    tasks._push_release(VERSION, cwd=str(release))

    # The tag is on the commit the tests ran against, not on the merge ...
    assert _origin_tag_commit(origin) == release_commit
    # ... and main carries both that commit and the other session's.
    main = _git(origin, "rev-parse", "main")
    assert main != release_commit
    for commit in (release_commit, theirs):
        subprocess.run(["git", "merge-base", "--is-ancestor", commit, main], cwd=origin, check=True)


def test_a_conflicting_main_publishes_nothing(repos) -> None:
    """A conflict cannot be resolved here, so the tag must never go out."""
    origin, release, other = repos
    theirs = _commit(other, "version.txt", "conflict\n", "another session edits the same line")
    _git(other, "push", "-q", "origin", "main")

    with pytest.raises(SystemExit, match="Nothing was published"):
        tasks._push_release(VERSION, cwd=str(release))

    assert _origin_tag_commit(origin) is None
    assert _git(origin, "rev-parse", "main") == theirs
    assert _git(release, "tag", "-l", TAG) == ""  # the unpushed local tag is gone
    assert _git(release, "status", "--porcelain") == ""  # and no half-finished merge
