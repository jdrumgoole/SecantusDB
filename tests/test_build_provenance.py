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
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from tools.provenance import (  # noqa: E402
    REBUILD_PGSERVER_CMD as _REBUILD_PGSERVER_CMD,
)
from tools.provenance import (
    REBUILD_RS_CMD as _REBUILD_RS_CMD,
)
from tools.provenance import (
    REBUILD_SERVER_CMD as _REBUILD_SERVER_CMD,
)
from tools.provenance import (
    binary_staleness,
    extension_staleness,
    require_fresh_pgserver,
    resolve_binary,
    stale_artifact_message,
)
from tools.provenance import (
    committed_crates_tree as _committed_crates_tree,
)

from conftest import (  # noqa: E402
    _CORE_CRATES,
    _committed_source_tree,
    stale_core_message,
)

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


# --------------------------------------------------------------------------- #
# The generic check, covering the three artifacts beyond `_secantus_core`.
#
# `_secantus_core` had this protection from #1489; `_secantus_server`,
# `secantusd-pg` and `secantusd-rs` did not. On 2026-09-27 that cost nine
# false-regression diagnoses in one session -- including one reported as "main
# is broken" from an extension six days behind the checkout, and six separate
# occasions where a stale `secantusd-pg` read as somebody else's bug.
#
# The binaries already CARRIED a source stamp and nothing read it, which is the
# worst arrangement: the evidence sat in `--version` output that no automated
# thing looked at.
# --------------------------------------------------------------------------- #


def test_a_generic_mismatch_is_reported() -> None:
    """The case it exists for, with the artifact named so the reader knows which."""
    msg = stale_artifact_message("_secantus_server", "aaaa", "bbbb", "./inv x", "cost")
    assert msg is not None
    assert "_secantus_server" in msg
    assert "aaaa" in msg and "bbbb" in msg


def test_a_generic_match_is_silent() -> None:
    assert stale_artifact_message("_secantus_server", "same", "same", "./inv x", "c") is None


@pytest.mark.parametrize(
    ("built", "current"),
    [
        ("", "current-tree"),  # unstamped artifact: predates the stamp, or no git at build
        ("built-tree", ""),  # git cannot answer: an sdist, a container without history
        ("", ""),  # neither side known
    ],
)
def test_generic_unknown_provenance_is_silent(built: str, current: str) -> None:
    """Silence when it cannot be sure is the half that keeps the check ALIVE.

    A false positive is how a check gets disabled, and the configurations here
    are all legitimate: whole CI lanes run without these artifacts, and an sdist
    build has no git history to stamp from.
    """
    assert stale_artifact_message("x", built, current, "./inv x", "c") is None


def test_each_rebuild_command_is_a_real_invoke_task() -> None:
    """A remedy that does not exist, or rebuilds the WRONG artifact, is worse
    than no remedy: it sends the reader on a detour and teaches them to distrust
    the check.

    The first draft of this table pointed `secantusd-rs` at `rust-server-build`,
    which rebuilds the embedded extension instead of the binary. This asserts
    each command against the task definitions rather than trusting the name.
    """
    tasks = (REPO / "rust_tasks.py").read_text()
    for cmd, expected_task in (
        (_REBUILD_SERVER_CMD, "rust-server-build"),
        (_REBUILD_PGSERVER_CMD, "rust-pgserver-build"),
        (_REBUILD_RS_CMD, "rust-binary-build"),
    ):
        assert expected_task in cmd, f"{cmd!r} should invoke {expected_task}"
        assert f'name="{expected_task}"' in tasks, f"{expected_task} is not a real task"
    # And the three are DISTINCT: the bug being guarded against was two
    # artifacts sharing one (wrong) command.
    assert len({_REBUILD_SERVER_CMD, _REBUILD_PGSERVER_CMD, _REBUILD_RS_CMD}) == 3


def test_the_crates_tree_hash_is_real_and_stable() -> None:
    """The identifier the server extension and both binaries are stamped with."""
    tree = _committed_crates_tree()
    assert tree, "git should be able to hash crates/ in a checkout"
    assert len(tree) == 40 and all(c in "0123456789abcdef" for c in tree)
    assert _committed_crates_tree() == tree, "must not vary between calls"


def test_the_crates_tree_differs_from_the_core_tree() -> None:
    """They are deliberately different identifiers, not two names for one thing.

    `_secantus_core` is stamped with just its two crates, so an unrelated crate's
    change does not read as stale there. The server extension links most of the
    workspace, so it takes the whole tree. Conflating them would either
    over-report on core or under-report on the server.
    """
    assert _committed_crates_tree() != _committed_source_tree()
    assert len(_CORE_CRATES) == 2


# --------------------------------------------------------------------------- #
# The check moved out of `conftest.py` on 2026-09-28 so that PROBES, GAUGES and
# BENCHMARKS can reach it. Before that it ran under pytest and nowhere else,
# which left the repo's primary bug-finding method — an ad-hoc differential
# probe — completely unguarded. A probe of `secantusd-pg` duly ran against a
# binary from a different `crates/` tree, and only a hand-read of `--version`
# caught it.
#
# These pin the part that is easy to regress: the probes must CALL the check.
# --------------------------------------------------------------------------- #

PROBES = REPO / "tools" / "probes"


def test_binary_staleness_abstains_when_the_binary_is_absent() -> None:
    """A missing build is somebody else's message to write.

    Every caller already has a clearer one ("... is not built -- cargo build"),
    and a staleness checker that also reports absence makes two different
    failures share one confusing text.
    """
    assert binary_staleness(REPO / "nope" / "secantusd-pg", "secantusd-pg", "./inv x") is None


def test_binary_staleness_reports_a_real_mismatch(monkeypatch: pytest.MonkeyPatch) -> None:
    """The case it exists for, driven through the binary path rather than the
    pure message function.

    Both halves are faked because arranging a genuinely stale binary means
    building one — and the first attempt to verify this by pointing a fresh
    binary at another worktree proved nothing, because no commit between the two
    checkouts had touched `crates/`, so the trees were identical and abstaining
    was CORRECT. A probe that cannot fail is not evidence.
    """
    import tools.provenance as provenance

    monkeypatch.setattr(provenance, "binary_source_tree", lambda _p: "built-from-this")
    monkeypatch.setattr(provenance, "committed_crates_tree", lambda _r=None: "but-tested-that")
    msg = provenance.binary_staleness(Path(__file__), "secantusd-pg", "./inv rust-pgserver-build")
    assert msg is not None
    assert "built-from-this" in msg and "but-tested-that" in msg
    assert "./inv rust-pgserver-build" in msg


def test_require_fresh_pgserver_aborts_rather_than_warning(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A warning is what the last seven incidents proved nobody reads.

    A probe prints hundreds of lines; a warning among them is invisible, and the
    whole point is that the run must not produce a number at all.
    """
    import tools.provenance as provenance

    monkeypatch.setattr(provenance, "binary_source_tree", lambda _p: "old")
    monkeypatch.setattr(provenance, "committed_crates_tree", lambda _r=None: "new")
    with pytest.raises(SystemExit) as excinfo:
        require_fresh_pgserver(Path(__file__))
    assert "secantusd-pg" in str(excinfo.value)


def test_extension_staleness_reads_the_source_stamp() -> None:
    """The extensions carry `__source_tree__` where the binaries print `tree:`."""

    class Stale:
        __source_tree__ = "extension-tree"

    msg = extension_staleness(Stale(), "_secantus_server", "./inv x")
    # Only a real mismatch reports; whether it does depends on the checkout, so
    # assert the shape of the decision rather than the verdict.
    if msg is not None:
        assert "extension-tree" in msg and "_secantus_server" in msg

    class Unstamped:
        pass

    assert extension_staleness(Unstamped(), "_secantus_server", "./inv x") is None


def test_resolve_binary_falls_back_to_the_exe_suffix(tmp_path: Path) -> None:
    """Cargo emits `.exe` on Windows, so the bare name never exists there.

    That one missing suffix made the pytest guard skip both binaries on the only
    platform where staleness had already bitten. It lives in one place now so it
    cannot be forgotten by the fourth caller.
    """
    exe = tmp_path / "secantusd-pg.exe"
    exe.write_text("")
    assert resolve_binary(tmp_path / "secantusd-pg") == exe

    plain = tmp_path / "secantusd-rs"
    plain.write_text("")
    assert resolve_binary(plain) == plain

    missing = tmp_path / "absent"
    assert resolve_binary(missing) == missing


@pytest.mark.parametrize(
    ("probe", "call"),
    [
        ("_servers.py", "require_fresh_server_extension"),
        ("pg_differential.py", "require_fresh_pgserver"),
    ],
)
def test_each_probe_launcher_checks_provenance(probe: str, call: str) -> None:
    """The gap this refactor closed, pinned so it cannot quietly reopen.

    A source check rather than a behavioural one: running a probe needs a live
    mongod or a built binary, neither of which a unit test should demand. What
    would actually regress is somebody adding a launch path and not calling the
    check, and that is visible in the source.
    """
    text = (PROBES / probe).read_text()
    assert call in text, f"{probe} launches a server without checking its provenance"


def test_the_shared_module_is_importable_without_pytest() -> None:
    """The whole point: a probe is not a pytest run.

    `tools.provenance` must import with nothing but the repo root on the path —
    no pytest, no conftest, no `src/` layout assumptions — or the probes cannot
    use it and the gap reopens.
    """
    result = subprocess.run(
        [sys.executable, "-c", "import tools.provenance as p; print(p.PGSERVER_REL)"],
        cwd=REPO,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert "secantusd-pg" in result.stdout


def test_a_copied_conftest_still_loads(tmp_path: Path) -> None:
    """`conftest.py` must import from a directory that is not the checkout.

    `tests/test_crash_stall_watchdog.py` writes a verbatim copy into a tmp dir so
    its nested session exercises the real watchdog. When the provenance import
    moved to a repo-root package, that copy could no longer resolve `tools` and
    failed to LOAD — which is not one test failing but every test in the lane, on
    every platform. Ten lanes went red at once.

    So the import is discovered and optional, and this pins it. The assertion is
    that a nested pytest run gets far enough to report no tests, rather than
    dying in conftest.
    """
    (tmp_path / "conftest.py").write_text((REPO / "tests" / "conftest.py").read_text())
    (tmp_path / "test_nothing.py").write_text("def test_ok():\n    assert True\n")
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "-n0", "-p", "no:randomly", "-q", str(tmp_path)],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=300,
    )
    combined = result.stdout + result.stderr
    assert "ImportError while loading conftest" not in combined, combined
    assert "No module named 'tools'" not in combined, combined
    assert result.returncode == 0, combined
