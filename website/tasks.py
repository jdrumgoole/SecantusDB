"""Invoke tasks for the SecantusDB marketing site under ``website/``.

Run from the ``website/`` directory:

* ``uv run python -m invoke serve``      &mdash; local preview at http://localhost:8000
* ``uv run python -m invoke build``      &mdash; production-ready build into ``output/``
* ``uv run python -m invoke clean``      &mdash; wipe build output + copied assets
* ``uv run python -m invoke deploy``     &mdash; build + sync to S3 + CloudFront invalidation
* ``uv run python -m invoke infra-up``   &mdash; provision the AWS infrastructure (one-time)
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

from invoke.context import Context
from invoke.tasks import task

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent
THEME_IMG = HERE / "themes" / "secantus" / "static" / "img"
BRANDKIT = REPO_ROOT / "brandkit"
OUTPUT = HERE / "output"
STATE_FILE = HERE / "infra" / "aws-state.json"

PELICAN_DEV_CMD = f"uv run --extra website pelican {HERE / 'content'} -o {OUTPUT} -s {HERE / 'pelicanconf.py'}"
PELICAN_PROD_CMD = f"uv run --extra website pelican {HERE / 'content'} -o {OUTPUT} -s {HERE / 'publishconf.py'}"
PELICAN_SERVE_CMD = "uv run --extra website pelican --listen --autoreload --port 8000 -s pelicanconf.py -o output content"


def _copy_brandkit_assets() -> None:
    """Mirror brandkit SVGs into the theme's static/img/ for serving."""
    THEME_IMG.mkdir(parents=True, exist_ok=True)
    if not BRANDKIT.exists():
        raise SystemExit(f"brandkit directory not found at {BRANDKIT}")
    copied = 0
    for svg in BRANDKIT.glob("*.svg"):
        shutil.copy2(svg, THEME_IMG / svg.name)
        copied += 1
    if copied == 0:
        raise SystemExit(f"no SVGs found in {BRANDKIT}")
    print(f"  copied {copied} SVG asset(s) → {THEME_IMG.relative_to(REPO_ROOT)}")


def _load_state() -> dict[str, str]:
    if not STATE_FILE.exists():
        raise SystemExit(
            f"AWS state file not found at {STATE_FILE}. Run `invoke infra-up` first."
        )
    return json.loads(STATE_FILE.read_text())


@task
def assets(c: Context) -> None:
    """Copy brandkit SVGs into the theme's static/img/."""
    _copy_brandkit_assets()


@task(pre=[assets])
def build(c: Context, prod: bool = False) -> None:
    """Build the site into ``website/output/``.

    With --prod, uses ``publishconf.py`` (absolute SITEURL, feeds enabled).
    """
    cmd = PELICAN_PROD_CMD if prod else PELICAN_DEV_CMD
    c.run(cmd, pty=True)


@task(pre=[assets])
def serve(c: Context) -> None:
    """Run a local Pelican preview at http://localhost:8000 with autoreload."""
    print("Starting Pelican on http://localhost:8000 — Ctrl-C to stop")
    with c.cd(str(HERE)):
        c.run(PELICAN_SERVE_CMD, pty=True)


@task
def clean(c: Context) -> None:
    """Remove the Pelican output directory and copied brandkit assets."""
    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
        print(f"  removed {OUTPUT.relative_to(REPO_ROOT)}")
    if THEME_IMG.exists():
        shutil.rmtree(THEME_IMG)
        print(f"  removed {THEME_IMG.relative_to(REPO_ROOT)}")


@task(name="infra-up")
def infra_up(c: Context, domain: str = "secantusdb.com") -> None:
    """Provision the AWS infrastructure for ``domain`` (idempotent).

    Reads/writes ``website/infra/aws-state.json``. AWS credentials must
    be available via the standard chain (env / ``~/.aws/credentials`` /
    SSO / instance profile). The Route 53 hosted zone for the domain
    must already exist.
    """
    aws_script = HERE / "infra" / "aws.py"
    c.run(
        f"uv run --extra website python {aws_script} up --domain {domain} --state-file {STATE_FILE}",
        pty=True,
    )


@task(name="infra-down")
def infra_down(c: Context, domain: str = "secantusdb.com") -> None:
    """Tear-down stub (manual via console — see infra/aws.py)."""
    aws_script = HERE / "infra" / "aws.py"
    c.run(
        f"uv run --extra website python {aws_script} down --domain {domain} --state-file {STATE_FILE}",
        pty=True,
    )


@task(pre=[assets])
def deploy(c: Context) -> None:
    """Build (production) and deploy: S3 sync + CloudFront invalidation.

    Reads bucket name and distribution ID from ``website/infra/aws-state.json``.
    """
    state = _load_state()
    bucket = state["bucket"]
    distribution_id = state["distribution_id"]

    print(f"=== Building site (prod) ===")
    c.run(PELICAN_PROD_CMD, pty=True)

    deploy_script = HERE / "infra" / "aws.py"
    print(f"=== Syncing to s3://{bucket}/ ===")
    c.run(
        f"uv run --extra website python {deploy_script} sync"
        f" --bucket {bucket}"
        f" --source {OUTPUT}",
        pty=True,
    )

    print(f"=== Invalidating CloudFront distribution {distribution_id} ===")
    c.run(
        f"uv run --extra website python {deploy_script} invalidate"
        f" --distribution-id {distribution_id}",
        pty=True,
    )
    print(f"\nDone. https://{state.get('domain', 'secantusdb.com')}/")
