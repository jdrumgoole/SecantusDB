"""The stale-build check in `conftest.py`.

A checker that is itself wrong is worse than none, so its decisions are pinned
here. The cases that matter are the SILENT ones: this check runs at collection
for every suite in the repo, and a false positive would fail runs for people
who have done nothing wrong — which is how a check gets switched off.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from conftest import _CORE_CRATES, _committed_source_tree, stale_core_message  # noqa: E402

REPO = Path(__file__).resolve().parent.parent


def test_a_mismatch_is_reported() -> None:
    """The case it exists for."""
    msg = stale_core_message("aaaa-bbbb", "cccc-dddd")
    assert msg is not None
    assert "aaaa-bbbb" in msg and "cccc-dddd" in msg


def test_the_failure_names_the_exact_rebuild_command() -> None:
    """A check that says "stale" without saying "run this" just relocates the
    rediscovery onto the reader."""
    msg = stale_core_message("old", "new")
    assert msg is not None
    assert "invoke sync" in msg
    # And says why the obvious command is not enough, because it isn't.
    assert "path dependency" in msg


def test_a_match_is_silent() -> None:
    assert stale_core_message("same-tree", "same-tree") is None


@pytest.mark.parametrize(
    ("built", "current"),
    [
        ("", "abc-def"),  # an extension built without git history (sdist, container)
        ("abc-def", ""),  # a checkout git cannot read
        ("", ""),  # neither
    ],
)
def test_unknown_provenance_is_silent(built: str, current: str) -> None:
    """Never fail a run the check cannot actually judge.

    The ~1,700 `test_rust_*_parity.py` tests `importorskip` the extension by
    design and whole CI lanes run without it; an unstamped or unreadable build
    must stay out of their way.
    """
    assert stale_core_message(built, current) is None


def test_the_checkout_hash_is_real_and_stable() -> None:
    """`_committed_source_tree` must return something git actually agrees with.

    Guards the half that talks to git: a typo'd path would return "" forever
    and silently disable the check, which is the failure mode that would be
    hardest to notice.
    """
    tree = _committed_source_tree()
    if not tree:
        pytest.skip("no git history here")
    parts = tree.split("-")
    assert len(parts) == 2
    for part, path in zip(parts, _CORE_CRATES, strict=True):
        out = subprocess.run(
            ["git", "rev-parse", f"HEAD:{path}"],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=True,
        )
        assert part == out.stdout.strip()


def test_the_hash_tracks_content_not_commits() -> None:
    """A TREE hash, not the commit SHA — the whole reason this can be strict.

    `git rev-parse HEAD` moves on every commit, so a commit-SHA check would
    report stale constantly and get disabled. The tree hash moves only when the
    crate's content does.
    """
    head = subprocess.run(
        ["git", "rev-parse", "HEAD:crates/secantus-core"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    try:
        older = subprocess.run(
            ["git", "rev-parse", "HEAD~5:crates/secantus-core"],
            cwd=REPO,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except subprocess.CalledProcessError:
        pytest.skip("shallow history")
    commits = subprocess.run(
        ["git", "log", "--oneline", "HEAD~5..HEAD", "--", "crates/secantus-core"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    # If those five commits left the crate alone, the hash must not have moved.
    assert (head == older) == (commits == "")


def test_a_dormant_check_says_so(capsys: pytest.CaptureFixture[str]) -> None:
    """An unstamped extension must ANNOUNCE that the check is off.

    Abstaining silently would leave the check installed, doing nothing, with
    nobody aware — the same "applied and changes no output" failure the check
    exists to catch, one level up. Anyone who never rebuilds stays in that
    state indefinitely.
    """
    from conftest import pytest_report_header

    header = pytest_report_header()
    try:
        import _secantus_core  # type: ignore[import-not-found]
    except ImportError:
        assert header is None, "no extension at all is a deliberate configuration"
        return
    if getattr(_secantus_core, "__source_tree__", ""):
        assert header is None, "a stamped build has nothing to announce"
    else:
        assert header is not None
        assert "UNKNOWN" in header
        assert "invoke sync" in header
