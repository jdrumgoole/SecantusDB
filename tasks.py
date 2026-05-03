from __future__ import annotations

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
    if clean:
        c.run("rm -rf docs/_build", pty=True)
    c.run(
        f"uv run sphinx-build -W --keep-going -b {builder} docs docs/_build/{builder}",
        pty=True,
    )


@task(name="docs-serve")
def docs_serve(c: Context, port: int = 8000) -> None:
    docs(c)
    c.run(
        f"uv run python -m http.server {port} --directory docs/_build/html",
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
    if not pathlib.Path("vendor/mongo-go-driver/go.mod").exists() or not pathlib.Path(
        "vendor/mongo-go-driver/testdata/specifications/source"
    ).is_dir():
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
    if not pathlib.Path("vendor/mongo-java-driver/gradlew").exists() or not pathlib.Path(
        "vendor/mongo-java-driver/testing/resources/specifications/source"
    ).is_dir():
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


@task
def clean(c: Context) -> None:
    c.run(
        "rm -rf build dist *.egg-info .pytest_cache .ruff_cache "
        ".coverage htmlcov docs/_build",
        pty=True,
    )
