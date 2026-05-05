from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import time
import urllib.error
import urllib.request

from invoke.context import Context
from invoke.tasks import task


@task
def sync(c: Context) -> None:
    c.run("uv sync --extra dev", pty=True)


@task
def test(c: Context, k: str = "", verbose: bool = False) -> None:
    cmd = "uv run python -m pytest"
    if verbose:
        cmd += " -v"
    if k:
        cmd += f" -k {k!r}"
    c.run(cmd, pty=True)


@task(name="test-one")
def test_one(c: Context, nodeid: str) -> None:
    c.run(f"uv run python -m pytest -p no:xdist {nodeid!r}", pty=True)


@task
def perf(c: Context) -> None:
    """Run the performance regression suite (serially, no xdist).

    Benchmarks fight for CPU under parallel workers, amplifying noise to
    the point that the gate becomes flappy — so this task forces serial
    execution and explicitly opts in to the ``perf`` marker excluded
    from the default ``invoke test``. Median time per workload is
    asserted against a hard upper bound calibrated for ``:memory:``
    storage on a quiet 2024-era arm64 mac. Lower the bounds in
    ``tests/test_perf_regression.py`` when an optimisation moves the
    floor.
    """
    c.run(
        "uv run python -m pytest -p no:xdist "
        "-o addopts= -m perf "
        "--benchmark-columns=min,median,max -v "
        "tests/test_perf_regression.py",
        pty=True,
    )


@task
def lint(c: Context) -> None:
    c.run("uv run ruff check src tests", pty=True)
    c.run("uv run ruff format --check src tests", pty=True)


@task
def fmt(c: Context) -> None:
    c.run("uv run ruff format src tests", pty=True)
    c.run("uv run ruff check --fix src tests", pty=True)


@task
def serve(c: Context, host: str = "127.0.0.1", port: int = 27017) -> None:
    c.run(f"uv run python -m secantus --host {host} --port {port}", pty=True)


@task
def docs(c: Context, builder: str = "html", clean: bool = False) -> None:
    # --no-sync skips uv's project rebuild check: docs only need the Python
    # source for autodoc, never a fresh WiredTiger C-extension build. Falling
    # through to `uv sync` here would invoke scikit-build-core's isolated
    # build env, which is sensitive to host cmake/swig setup and unnecessary
    # for a docs build.
    if clean:
        c.run("rm -rf docs/_build", pty=True)
    c.run(
        f"uv run --no-sync sphinx-build -W --keep-going -b {builder} docs docs/_build/{builder}",
        pty=True,
    )


@task(name="docs-serve")
def docs_serve(c: Context, port: int = 8000) -> None:
    docs(c)
    c.run(
        f"uv run --no-sync python -m http.server {port} --directory docs/_build/html",
        pty=True,
    )


@task
def validate(c: Context) -> None:
    """Run pymongo's vendored test suite against an embedded SecantusDB.

    Generates docs/validation-report.md with a per-category pass / fail /
    skip / pass-rate breakdown — the "MongoDB compatibility" gauge.
    """
    import pathlib

    from pymongo_validation.include_paths import INCLUDE

    if not pathlib.Path("vendor/pymongo-tests/test").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    paths = " ".join(INCLUDE)
    # `-p no:cacheprovider`: don't pollute pymongo's tree with .pytest_cache.
    # `-p no:xdist -o addopts=`: pymongo's tests aren't xdist-safe (shared DBs);
    #   override the project-wide `addopts="-n auto"` from pyproject.toml.
    # `-p pymongo_validation.plugin`: load our embedded-server bootstrap.
    # `--continue-on-collection-errors`: a collection failure in one file
    #   shouldn't abort the whole run — we want every category measured.
    # `-c pyproject.toml` forces pytest to use OUR config; without it pytest
    # picks up vendor/pymongo-tests/pyproject.toml (closer to the test files)
    # which has options for plugins we don't load (pytest-asyncio etc).
    # `-o addopts= -o testpaths=`: clear the project-wide xdist + tests/ scoping
    # from our pyproject; this run uses positional paths.
    # PYTHONPATH=. so pytest can import our `pymongo_validation` plugin.
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m pytest "
        "-c pyproject.toml "
        "-o addopts= -o testpaths= "
        "-p no:cacheprovider -p no:xdist -p pymongo_validation.plugin "
        "--continue-on-collection-errors "
        "--json-report --json-report-file=.validation/raw.json "
        f"--no-header --tb=no -q {paths}",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m pymongo_validation.generate_report "
        ".validation/raw.json docs/validation-report.md",
        pty=True,
    )
    print("\nWrote docs/validation-report.md")


@task(name="validate-go")
def validate_go(c: Context) -> None:
    """Run mongo-go-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-go.md with a per-package pass /
    fail / skip / pass-rate breakdown — the Go-driver analogue of the
    pymongo gauge. Requires `go` on PATH (1.21+).
    """
    import pathlib

    # Need both the outer submodule AND its nested `testdata/specifications`
    # submodule (driver-spec test data — without it the bson-corpus tests
    # fail on missing JSON files).
    if (
        not pathlib.Path("vendor/mongo-go-driver/go.mod").exists()
        or not pathlib.Path("vendor/mongo-go-driver/testdata/specifications/source").is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m go_validation.runner",
        pty=True,
        warn=True,  # report is the deliverable
    )
    c.run(
        "uv run --no-sync python -m go_validation.generate_report "
        ".validation/go-raw.ndjson docs/validation-report-go.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-go.md")


@task(name="validate-node")
def validate_node(c: Context) -> None:
    """Run mongo-node-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-node.md with a per-category pass /
    fail / pending / pass-rate breakdown — the Node-driver analogue of
    the pymongo and Go-driver gauges. Requires Node.js (>=20) and npm
    on PATH. First run does a one-time `npm install` (~1-2 min) inside
    vendor/node-mongodb-native/.
    """
    import pathlib

    if not pathlib.Path("vendor/node-mongodb-native/package.json").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m node_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m node_validation.generate_report "
        ".validation/node-raw.json docs/validation-report-node.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-node.md")


@task(name="validate-java")
def validate_java(c: Context) -> None:
    """Run mongo-java-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-java.md with a per-module pass /
    fail / skipped / pass-rate breakdown — the Java-driver analogue of
    the pymongo / Go / Node gauges. Requires a JDK (>=8) on PATH; uses
    the gradle wrapper the driver ships, so no system Gradle install
    needed. First run downloads the gradle distribution + dependencies
    (~150 MB) into ~/.gradle/.
    """
    import pathlib

    # The driver pulls in MongoDB driver-spec test data via a nested
    # submodule (testing/resources/specifications) — without it the
    # bson corpus / vector tests fail with `initializationError` on
    # missing JSON files. Same pattern as the go-driver gauge.
    if (
        not pathlib.Path("vendor/mongo-java-driver/gradlew").exists()
        or not pathlib.Path(
            "vendor/mongo-java-driver/testing/resources/specifications/source"
        ).is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    c.run(
        "PYTHONPATH=. uv run --no-sync python -m java_validation.runner",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m java_validation.generate_report "
        ".validation/java-results docs/validation-report-java.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-java.md")


@task(name="validate-readme")
def validate_readme(c: Context) -> None:
    """HEAD-check every URL in the published PyPI README.

    PyPI doesn't know our git repo, so any relative URL in `README.md`
    renders as a broken link on the project page. This task fetches
    the description PyPI is actually serving, extracts every link/img
    URL, and reports each one's reachability — a thin wrapper over
    `pytest -m online tests/test_pypi_readme_links.py` so failures
    are easy to read in a terminal.

    Run it after every release. Network-dependent and depends on the
    package being published, so it's deliberately excluded from
    `invoke test` (the `online` marker filters it out by default).
    """
    c.run(
        "uv run --no-sync python -m pytest "
        "-p no:xdist -o addopts= -m online -v "
        "tests/test_pypi_readme_links.py",
        pty=True,
    )


@task
def clean(c: Context) -> None:
    c.run(
        "rm -rf build dist *.egg-info .pytest_cache .ruff_cache .coverage htmlcov docs/_build",
        pty=True,
    )


_VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)([ab]\d+|rc\d+)?$")


@task
def release(c: Context, version: str) -> None:
    """Cut a release: prepare + finalize, end-to-end.

    The canonical one-shot release workflow (see `## Releases` in
    CLAUDE.md). Internally calls ``release-prepare`` (fast,
    foreground-friendly) followed by ``release-finalize`` (long
    polling). When invoked from a sub-agent, prefer running the two
    phases separately so the polling phase can use
    ``run_in_background=true`` and escape the per-Bash 10-minute cap.
    """
    release_prepare(c, version)
    release_finalize(c, version)


@task(name="release-prepare")
def release_prepare(c: Context, version: str) -> None:
    """Phase 1 of the release.

    Pre-flight → tests → perf → bump → commit → tag → push → GitHub
    Release. Fits comfortably in 5–7 min on a quiet machine. Sub-agents can run
    this in the foreground with the harness's default Bash timeout.
    Pushing the tag triggers the `Publish to PyPI` workflow
    asynchronously; pushing main triggers the RTD `latest` build
    asynchronously. Both finish independently of this task — wait for
    them via ``release-finalize``.

    Pre-flight requirements (all enforced):
      - On `main` branch.
      - Working tree clean (vendored-submodule drift in either
        lowercase ` m vendor/...` or capital ` M vendor/...` form is
        tolerated; everything else rejects).
      - HEAD == origin/main (no unpushed commits).
      - Tag `vX.Y.Z` not already on origin.
      - `READTHEDOCS_TOKEN` available — exported or in `.env` (this
        phase doesn't use the token, but rejecting now means we don't
        push a release and then discover the token is missing in
        finalize).

    Pipeline:
      1. Full default test suite (`pytest` parallel, perf-excluded).
      2. Perf regression gates (serial).
      3. Bump pyproject.toml + src/secantus/__init__.py + uv.lock.
      4. Commit, annotate-tag, push commit + tag (combined push).
      5. Create a GitHub Release for `vX.Y.Z` with auto-generated
         notes (marked pre-release for `aN`/`bN`/`rcN` versions).
      6. Activate the RTD `vX.Y.Z` slug (best-effort) so its build
         runs concurrent with the GitHub `Publish to PyPI` workflow
         rather than after it. Failure here is non-fatal; finalize
         retries the activation idempotently.
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    _ensure_main_branch_clean()
    _ensure_in_sync_with_origin()
    _ensure_tag_unused(version)
    _ensure_rtd_token()

    print("==> [1/5] Full default test suite")
    c.run("uv run python -m pytest", pty=True)
    print("==> [2/5] Perf regression gates")
    c.run(
        "uv run python -m pytest -p no:xdist -o addopts= -m perf tests/test_perf_regression.py",
        pty=True,
    )

    print(f"==> [3/5] Bumping version files to {version}")
    _bump_version_files(version)
    c.run("uv lock", pty=True)

    print(f"==> [4/6] Committing + tagging v{version}")
    c.run("git add pyproject.toml src/secantus/__init__.py uv.lock", pty=True)
    c.run(f'git commit -m "Release v{version}"', pty=True)
    c.run(f'git tag -a v{version} -m "Release v{version}"', pty=True)
    # Combine the branch and tag pushes into one network round-trip.
    # The publish workflow still fires on the tag ref; nothing else
    # depends on the order of branch-then-tag.
    c.run(f"git push origin main v{version}", pty=True)

    print(f"==> [5/6] Creating GitHub Release v{version}")
    # Pre-release if the version has an `aN` / `bN` / `rcN` suffix.
    is_prerelease = bool(re.search(r"[abc]\d+$|rc\d+$", version))
    cmd = (
        f"gh release create v{version} "
        f"--title 'v{version}' "
        f"--generate-notes "
        f"--target $(git rev-parse HEAD)"
    )
    if is_prerelease:
        cmd += " --prerelease"
    c.run(cmd, pty=True)

    # Activate the RTD slug as early as possible so its build runs
    # concurrent with the GitHub `Publish to PyPI` workflow rather
    # than after it. Best-effort: if the RTD API errors here, finalize
    # will retry — better to push the release than to abort prepare
    # over a transient RTD blip.
    print(f"==> [6/6] Activating RTD `v{version}` slug for early build")
    try:
        _activate_rtd_version(version, _ensure_rtd_token())
    except SystemExit as e:
        print(f"    warning: RTD activate failed in prepare ({e}); finalize will retry")

    print(
        f"\nv{version} prepared, tag pushed, GitHub Release created, RTD build queued.\n"
        f"Run `invoke release-finalize {version}` next to wait for the\n"
        f"publish workflow + PyPI + RTD propagation."
    )


@task(name="release-finalize")
def release_finalize(c: Context, version: str) -> None:
    """Phase 2 of the release.

    Poll publish workflow → PyPI → RTD `latest` → activate `vX.Y.Z`
    slug → poll its build → PATCH RTD `default_version`.

    Polling can run for 15–25 min in the worst case (publish workflow
    builds wheels for cp310-cp313 across 4 platforms; RTD compiles
    WiredTiger from source twice — once for `latest`, once for the
    tag). Sub-agents must call this with ``run_in_background=true``
    on the Bash invocation to escape the harness's 10-min per-call
    cap; foreground in a developer's shell is fine.

    Idempotent: every step short-circuits if the desired state is
    already true (publish workflow already concluded, PyPI already
    lists the version, RTD build already finished, version already
    active, `default_version` already set). Safe to re-run after any
    timeout or interruption.

    Pre-flight requirements:
      - Tag `vX.Y.Z` exists on origin (the prepare phase pushed it).
      - `READTHEDOCS_TOKEN` available.

    Pipeline:
      6. Wait for GitHub `Publish to PyPI` workflow to succeed.
      7. Wait for PyPI to list the new version.
      8. Wait for RTD `latest` to publish a successful build of the
         release commit.
      9. Activate the `vX.Y.Z` slug on RTD and wait for its build.
     10. Set RTD's `default_version` to `vX.Y.Z`.
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    rtd_token = _ensure_rtd_token()
    commit = _resolve_tag_commit(version)

    print(f"==> [6/10] Waiting for GitHub `Publish to PyPI` workflow (commit {commit[:7]})")
    _wait_for_publish_workflow(commit)
    print(f"==> [7/10] Waiting for PyPI to list {version}")
    _wait_for_pypi_version(version)
    print(f"==> [8/10] Waiting for RTD `latest` to build commit {commit[:7]}")
    _wait_for_rtd_build(commit)
    print(f"==> [9/10] Activating + building RTD `v{version}`")
    _activate_rtd_version(version, rtd_token)
    _wait_for_rtd_tag_build(version, rtd_token)
    print(f"==> [10/10] Setting RTD `default_version` to `v{version}`")
    _set_rtd_default_version(version, rtd_token)

    print(f"\nv{version} released; GitHub Release, PyPI, and RTD up to date.")


def _resolve_tag_commit(version: str) -> str:
    """Resolve the commit SHA for ``vX.Y.Z`` on origin.

    Used by ``release-finalize`` to find the release commit when re-run
    later (after any ``main`` HEAD drift). The annotated tag's target
    is the release commit itself, regardless of what's on ``main`` now.
    """
    out = subprocess.run(
        ["git", "rev-parse", f"v{version}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        # Fall back to the remote ref so finalize works even if the
        # local tag was pruned.
        out = subprocess.run(
            ["git", "ls-remote", "origin", f"refs/tags/v{version}^{{}}"],
            capture_output=True,
            text=True,
            check=True,
        )
        line = out.stdout.strip().split("\n", 1)[0]
        if not line:
            raise SystemExit(
                f"tag v{version} not found on origin — "
                f"run `invoke release-prepare {version}` first."
            )
        return line.split()[0]
    return out.stdout.strip()


def _ensure_main_branch_clean() -> None:
    branch = subprocess.run(
        ["git", "branch", "--show-current"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if branch != "main":
        raise SystemExit(f"release must run on main; on {branch!r}")
    status = subprocess.run(
        ["git", "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    # Vendored-submodule drift comes in two flavours, both tolerated:
    #   " m vendor/foo" — modified content inside the submodule (build-time
    #     WiredTiger patching, etc.).
    #   " M vendor/foo" — submodule HEAD shifted because a parallel worktree
    #     pulled or updated the submodule SHA.
    # Neither goes into the release commit (the task only `git add`s
    # pyproject.toml + __init__.py + uv.lock), so they're safe to ignore.
    # Anything else is uncommitted work the release would either include
    # or shadow — reject it.
    bad = [
        line
        for line in status.splitlines()
        if line and not (line.startswith((" m ", " M ")) and "vendor/" in line)
    ]
    if bad:
        raise SystemExit("working tree has uncommitted changes:\n" + "\n".join(bad))


def _ensure_in_sync_with_origin() -> None:
    subprocess.run(["git", "fetch", "origin"], check=True, capture_output=True)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    origin = subprocess.run(
        ["git", "rev-parse", "origin/main"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if head != origin:
        raise SystemExit(
            f"local main ({head[:7]}) is not in sync with origin/main "
            f"({origin[:7]}) — push or pull first."
        )


def _ensure_tag_unused(version: str) -> None:
    out = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"v{version}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if out:
        raise SystemExit(f"tag v{version} already exists on origin — pick a new version.")


def _bump_version_files(version: str) -> None:
    py = pathlib.Path("pyproject.toml")
    init = pathlib.Path("src/secantus/__init__.py")
    py.write_text(
        re.sub(
            r'^version = "[^"]+"',
            f'version = "{version}"',
            py.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )
    init.write_text(
        re.sub(
            r'^__version__ = "[^"]+"',
            f'__version__ = "{version}"',
            init.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )


def _wait_for_publish_workflow(commit: str, *, timeout_s: int = 1200) -> None:
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        out = subprocess.run(
            [
                "gh",
                "run",
                "list",
                "--workflow=publish.yml",
                f"--commit={commit}",
                "--json=status,conclusion,databaseId",
                "--limit=1",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        runs = json.loads(out or "[]")
        if not runs:
            line = "    no publish run for this commit yet; waiting"
        else:
            r = runs[0]
            conc = r.get("conclusion") or ""
            line = f"    run {r['databaseId']}: status={r['status']} conclusion={conc}"
            if r["status"] == "completed":
                if r.get("conclusion") == "success":
                    print(line)
                    return
                raise SystemExit(
                    f"publish workflow {r['databaseId']} concluded {r.get('conclusion')!r}"
                )
        if line != last:
            print(line)
            last = line
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for publish workflow")


def _wait_for_pypi_version(version: str, *, timeout_s: int = 600) -> None:
    url = "https://pypi.org/pypi/SecantusDB/json"
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.load(resp)
        except Exception as e:
            print(f"    PyPI API error: {e}; retrying")
            time.sleep(20)
            continue
        latest = data["info"]["version"]
        if version in data.get("releases", {}):
            print(f"    PyPI lists {version} (info.version={latest})")
            return
        print(f"    PyPI does not list {version} yet (info.version={latest}); waiting")
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for PyPI to list {version}")


def _wait_for_rtd_build(commit: str, *, timeout_s: int = 900) -> None:
    url = "https://readthedocs.org/api/v3/projects/secantusdb/builds/?limit=5"
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.load(resp)
        except Exception as e:
            print(f"    RTD API error: {e}; retrying")
            time.sleep(30)
            continue
        match = next(
            (b for b in data.get("results", []) if (b.get("commit") or "").startswith(commit[:12])),
            None,
        )
        if match is None:
            line = f"    no RTD build found for {commit[:7]} yet; waiting"
        else:
            state = match["state"]["code"]
            success = match.get("success")
            line = f"    build {match['id']}: state={state} success={success}"
            if state == "finished":
                if success:
                    print(line)
                    return
                raise SystemExit(f"RTD build {match['id']} for {commit[:7]} failed")
        if line != last:
            print(line)
            last = line
        time.sleep(30)
    raise SystemExit(f"timed out after {timeout_s}s waiting for RTD build of {commit[:7]}")


_RTD_PROJECT_API = "https://readthedocs.org/api/v3/projects/secantusdb"


def _ensure_rtd_token() -> str:
    """Pre-flight: require READTHEDOCS_TOKEN so the post-publish RTD admin
    operations (activate version, set default_version) can run.

    Resolution order:
      1. ``READTHEDOCS_TOKEN`` already in the process env (e.g. set in
         the user's shell rc).
      2. ``READTHEDOCS_TOKEN=…`` line in a project-root ``.env`` file
         (gitignored). This is the recommended on-disk store.
    """
    token = os.environ.get("READTHEDOCS_TOKEN")
    if not token:
        token = _read_dotenv_var("READTHEDOCS_TOKEN")
    if not token:
        raise SystemExit(
            "READTHEDOCS_TOKEN is required for the release task — without it,\n"
            "RTD's default version stays pinned to whatever it was before this\n"
            "release. Mint one (read+write) at\n"
            "    https://app.readthedocs.org/accounts/tokens/\n"
            "and either export it in your shell or put `READTHEDOCS_TOKEN=…`\n"
            "into a `.env` file at the repo root (which is gitignored)."
        )
    return token


def _read_dotenv_var(key: str) -> str | None:
    """Tiny ``.env`` parser: ``KEY=VALUE`` lines, optional surrounding
    quotes, ``#`` comments. No interpolation, no exports — that would
    duplicate python-dotenv for one variable."""
    env_path = pathlib.Path(".env")
    if not env_path.is_file():
        return None
    for raw in env_path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        if k.strip() != key:
            continue
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
            v = v[1:-1]
        return v or None
    return None


def _rtd_request(method: str, path: str, token: str, body: dict | None = None) -> dict:
    """Issue a single RTD API v3 request and return the parsed JSON body.

    `path` is appended to the project endpoint (e.g. ``""`` for the
    project itself, ``"/versions/v0.3.0a4/"`` for a version). RTD
    endpoints expect a trailing slash.
    """
    url = _RTD_PROJECT_API + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Token {token}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        payload = resp.read()
    if not payload:
        return {}
    return json.loads(payload)


def _activate_rtd_version(version: str, token: str) -> None:
    """Set the `vX.Y.Z` slug to active so RTD builds it. RTD auto-queues
    a build when a version flips to active=True."""
    path = f"/versions/v{version}/"
    try:
        _rtd_request("PATCH", path, token, body={"active": True, "hidden": False})
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise SystemExit(
            f"failed to activate RTD version v{version}: HTTP {e.code} {e.reason}\n{body}"
        ) from e
    print(f"    activated RTD version v{version}")


def _wait_for_rtd_tag_build(version: str, token: str, *, timeout_s: int = 900) -> None:
    """Poll RTD for the most recent build of the `vX.Y.Z` slug until it
    finishes successfully. Activating a version triggers a build, but the
    api may take a few seconds to register it — first iterations may
    legitimately find nothing."""
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        try:
            data = _rtd_request("GET", f"/versions/v{version}/builds/?limit=1", token)
        except Exception as e:
            print(f"    RTD API error: {e}; retrying")
            time.sleep(30)
            continue
        builds = data.get("results", [])
        if not builds:
            line = f"    no build for v{version} yet; waiting"
        else:
            b = builds[0]
            state = b["state"]["code"]
            success = b.get("success")
            line = f"    v{version} build {b['id']}: state={state} success={success}"
            if state == "finished":
                if success:
                    print(line)
                    return
                raise SystemExit(f"RTD build {b['id']} for v{version} failed")
        if line != last:
            print(line)
            last = line
        time.sleep(30)
    raise SystemExit(f"timed out after {timeout_s}s waiting for RTD build of v{version}")


def _set_rtd_default_version(version: str, token: str) -> None:
    """PATCH the project's `default_version` so the bare RTD URL serves
    `v{version}` rather than the previous default (typically `stable` or
    `latest`)."""
    try:
        _rtd_request("PATCH", "/", token, body={"default_version": f"v{version}"})
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise SystemExit(
            f"failed to set RTD default_version=v{version}: HTTP {e.code} {e.reason}\n{body}"
        ) from e
    print(f"    RTD default_version set to v{version}")
