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
def serve(c: Context, host: str = "127.0.0.1", port: int = 27117) -> None:
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
def clean(c: Context) -> None:
    c.run(
        "rm -rf build dist *.egg-info .pytest_cache .ruff_cache "
        ".coverage htmlcov docs/_build",
        pty=True,
    )
