"""Invoke tasks for the SecantusDB marketing site under ``website/``.

Run from the ``website/`` directory.

* On the **main repo** worktree::

    uv run python -m invoke serve | build | deploy | publish | infra-up

* On the dedicated ``website-dev`` worktree (the one CLAUDE.md tells
  you to use to avoid parallel-release stash collisions on ``main``),
  invoke the venv's python directly so ``uv``'s project-sync doesn't
  try to rebuild the secantusdb wheel from a worktree that doesn't
  have the WiredTiger submodule initialised::

    ../SecantusDB/.venv/bin/python -m invoke serve | build | ...

  All internal subprocess calls in this module run via the SAME
  ``sys.executable`` that's currently running invoke, so once the
  outer command finds a working venv, every step is consistent — no
  more ``uv run`` recursion.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import sys
from pathlib import Path

from invoke.context import Context
from invoke.tasks import task

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent
THEME_IMG = HERE / "themes" / "secantus" / "static" / "img"
BRANDKIT = REPO_ROOT / "brandkit"
OUTPUT = HERE / "output"
STATE_FILE = HERE / "infra" / "aws-state.json"

# Use the running interpreter for every subprocess so the task is
# identical whether invoked under `uv run` (main repo) or via the
# venv's python directly (website worktree, where uv-sync would fail).
PYTHON = sys.executable

PELICAN_DEV_CMD = f"{PYTHON} -m pelican {HERE / 'content'} -o {OUTPUT} -s {HERE / 'pelicanconf.py'}"
PELICAN_PROD_CMD = f"{PYTHON} -m pelican {HERE / 'content'} -o {OUTPUT} -s {HERE / 'publishconf.py'}"
PELICAN_SERVE_CMD = f"{PYTHON} -m pelican --listen --autoreload --port 8000 -s pelicanconf.py -o output content"


def _main_repo_root() -> Path | None:
    """Return the main repo root if we're in a worktree, else None.

    Uses ``git rev-parse --git-common-dir``: in the main repo this is
    the relative ``.git``; in a worktree it's the main repo's
    ``/path/to/main/.git`` (absolute).
    """
    import subprocess
    try:
        common_dir = subprocess.check_output(
            ["git", "-C", str(REPO_ROOT), "rev-parse", "--git-common-dir"],
            text=True,
        ).strip()
    except subprocess.CalledProcessError:
        return None
    common_path = Path(common_dir) if Path(common_dir).is_absolute() else (REPO_ROOT / common_dir)
    main = common_path.parent
    return main if main != REPO_ROOT else None


def _ensure_worktree_setup() -> None:
    """In a worktree, mirror two things from the main repo:

    1. ``.venv`` (symlink) so pelican / boto3 / invoke are available
       without rebuilding the secantusdb wheel.
    2. ``website/infra/aws-state.json`` (symlink) so ``deploy`` can
       read the bucket / distribution / cert IDs without re-running
       ``infra-up`` in every worktree.

    No-op when the worktree IS the main repo (its own ``.venv`` is
    managed by ``uv sync``; its ``aws-state.json`` is the source of
    truth that the worktree symlinks to).
    """
    main_repo = _main_repo_root()
    if main_repo is None:
        return  # main repo or non-git, leave alone

    # 1) .venv symlink
    venv = REPO_ROOT / ".venv"
    if not (venv.is_dir() and (venv / "bin" / "pelican").exists()):
        main_venv = main_repo / ".venv"
        if not (main_venv / "bin" / "pelican").exists():
            raise SystemExit(
                f"main repo venv at {main_venv} is missing pelican.\n"
                f"Run `cd {main_repo} && uv sync --extra dev --extra website` first."
            )
        if venv.exists() or venv.is_symlink():
            if venv.is_symlink():
                venv.unlink()
            else:
                shutil.rmtree(venv)
        venv.symlink_to(main_venv)
        print(f"  symlinked {venv} → {main_venv}")

    # 2) infra/aws-state.json symlink (only if the main repo has it)
    main_state = main_repo / "website" / "infra" / "aws-state.json"
    if main_state.exists() and not STATE_FILE.exists():
        STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
        if STATE_FILE.is_symlink():
            STATE_FILE.unlink()
        STATE_FILE.symlink_to(main_state)
        print(f"  symlinked {STATE_FILE} → {main_state}")


# Backwards-compatible alias kept so any external callers (or my own
# in-flight branches) that still call _ensure_venv() keep working.
_ensure_venv = _ensure_worktree_setup


def _copy_brandkit_assets() -> None:
    """Mirror brandkit images into the theme's static/img/ for serving.

    SVGs are the source of truth; the PNG copies (only ``og-image.png``
    today) exist because Bluesky / LinkedIn / Slack don't render SVG
    Open Graph images, so a rasterised companion is required.
    """
    THEME_IMG.mkdir(parents=True, exist_ok=True)
    if not BRANDKIT.exists():
        raise SystemExit(f"brandkit directory not found at {BRANDKIT}")
    copied = 0
    for pattern in ("*.svg", "*.png"):
        for src in BRANDKIT.glob(pattern):
            shutil.copy2(src, THEME_IMG / src.name)
            copied += 1
    if copied == 0:
        raise SystemExit(f"no SVG/PNG assets found in {BRANDKIT}")
    print(f"  copied {copied} brandkit asset(s) → {THEME_IMG.relative_to(REPO_ROOT)}")


def _load_state() -> dict[str, str]:
    if not STATE_FILE.exists():
        raise SystemExit(
            f"AWS state file not found at {STATE_FILE}. Run `invoke infra-up` first."
        )
    return json.loads(STATE_FILE.read_text())


@task
def assets(c: Context) -> None:
    """Copy brandkit SVGs into the theme's static/img/."""
    _ensure_venv()
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
    _ensure_venv()
    aws_script = HERE / "infra" / "aws.py"
    c.run(
        f"{shlex.quote(PYTHON)} {shlex.quote(str(aws_script))} up"
        f" --domain {shlex.quote(domain)}"
        f" --state-file {shlex.quote(str(STATE_FILE))}",
        pty=True,
    )


@task(name="infra-down")
def infra_down(c: Context, domain: str = "secantusdb.com") -> None:
    """Tear-down stub (manual via console — see infra/aws.py)."""
    _ensure_venv()
    aws_script = HERE / "infra" / "aws.py"
    c.run(
        f"{shlex.quote(PYTHON)} {shlex.quote(str(aws_script))} down"
        f" --domain {shlex.quote(domain)}"
        f" --state-file {shlex.quote(str(STATE_FILE))}",
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

    # Graft the two Sphinx docs trees into the Pelican output so the
    # delete-sweeping S3 sync owns /docs/ and /docs/rust/ (anything the
    # sync doesn't know about it deletes — the docs must ride inside the
    # build, not be uploaded separately). Both trees build via the main
    # repo's invoke tasks (-W, no WiredTiger build needed).
    main_repo = _main_repo_root() or REPO_ROOT
    print("=== Building docs trees (main + rust) ===")
    c.run(f"cd {shlex.quote(str(main_repo))} && uv run --no-sync python -m invoke docs docs-rust", pty=True)
    docs_out = OUTPUT / "docs"
    if docs_out.exists():
        shutil.rmtree(docs_out)
    shutil.copytree(main_repo / "docs" / "_build" / "html", docs_out)
    shutil.copytree(main_repo / "docs-rust" / "_build" / "html", docs_out / "rust")
    print(f"  grafted docs -> {docs_out} and {docs_out / 'rust'}")

    deploy_script = HERE / "infra" / "aws.py"
    print(f"=== Syncing to s3://{bucket}/ ===")
    # shlex.quote every interpolated value — bucket / distribution_id are
    # read from aws-state.json, which is gitignored / on-disk only, but a
    # poisoned state file should not become a code-execution path.
    c.run(
        f"{shlex.quote(PYTHON)} {shlex.quote(str(deploy_script))} sync"
        f" --bucket {shlex.quote(bucket)}"
        f" --source {shlex.quote(str(OUTPUT))}",
        pty=True,
    )

    print(f"=== Invalidating CloudFront distribution {distribution_id} ===")
    c.run(
        f"{shlex.quote(PYTHON)} {shlex.quote(str(deploy_script))} invalidate"
        f" --distribution-id {shlex.quote(distribution_id)}",
        pty=True,
    )
    print(f"\nDone. https://{state.get('domain', 'secantusdb.com')}/")


# Paths allowed in a `publish` commit. Anything else in `git status`
# aborts the shortcut so we never sneak unrelated changes through the
# website-only fast path (which skips the full pytest suite).
# `.gitignore` is included because the worktree convention adds rules
# for the .venv symlink; CLAUDE.md is included because website
# conventions get documented there alongside the marketing site;
# `brandkit/` is included because brand assets (SVG sources + PNG
# companions for OG / social embeds) are referenced by the site and
# legitimately ride along with marketing changes.
_PUBLISH_ALLOWED_PREFIXES: tuple[str, ...] = (
    "website/", "CLAUDE.md", ".gitignore", "brandkit/",
)


def _git(c: Context, args: str, **kw) -> str:
    return c.run(f"git -C {REPO_ROOT} {args}", hide=True, **kw).stdout.rstrip()


@task(
    help={
        "message": "Commit message for the website-only commit.",
        "no_deploy": (
            "Commit and push, but don't deploy. Use on a feature branch; "
            "deploy from main after the PR merges."
        ),
    }
)
def publish(c: Context, message: str = "", no_deploy: bool = False) -> None:
    """Commit, push, and deploy website changes — no pytest, no version bump.

    The shortcut skips the global "run the full test suite before
    committing" rule (the website tree never changes SecantusDB's
    runtime code) and the "bump the version on every push" rule
    (website tree is excluded from sdist/wheel). Only changes under
    ``website/``, ``brandkit/``, the project ``CLAUDE.md`` and
    ``.gitignore`` are allowed; anything else aborts the task so a
    misfire can't fast-path real code through it.

    **The deploy step ships whatever branch you run this from.** The site
    is built from ``main``, so running the full task on a feature branch
    puts content live that isn't on ``main`` yet — and the next deploy
    from ``main`` silently reverts it. On a branch, pass ``--no-deploy``
    to get the guarded commit + push only, then deploy once the PR has
    merged::

        # on the feature branch:
        cd website && uv run python -m invoke publish -m "site: ..." --no-deploy
        # after the merge:
        git checkout main && git pull
        cd website && uv run python -m invoke deploy

    Running the full task on ``main`` (commit + push + deploy in one go)
    stays correct for a trivial edit that doesn't warrant a PR.
    """
    _ensure_venv()

    if not message:
        raise SystemExit(
            "publish needs a commit message: "
            'invoke publish --message "what changed"'
        )

    # `--porcelain=v1 -z` emits NUL-separated entries with no trailing
    # newline. Plain `--porcelain=v1` is line-oriented, which makes a
    # filename containing `\n` split into multiple "lines" and bypasses
    # the path-prefix allowlist. -z is the POSIX-safe form.
    porcelain = _git(c, "status --porcelain=v1 -z")
    if not porcelain:
        raise SystemExit("nothing to publish — working tree is clean")

    bad: list[str] = []
    paths_to_add: list[str] = []
    entries = [e for e in porcelain.split("\0") if e]
    i = 0
    while i < len(entries):
        entry = entries[i]
        i += 1
        if len(entry) < 3:
            continue
        status = entry[:2]
        path = entry[3:]
        # `R<score>` / `C<score>` (rename/copy) is followed by a second
        # entry containing the OLD path — consume and skip it.
        if status[0] in ("R", "C") and i < len(entries):
            i += 1
        # Skip vendor submodule drift markers (' m vendor/...') — the
        # global rule tolerates those.
        if status in (" m", " M") and path.startswith("vendor/"):
            continue
        # Normalize before the prefix check so `website/../src/...` (which
        # lexically passes `startswith("website/")`) doesn't sneak through.
        # `os.path.normpath` collapses `..` segments — `website/../src/x`
        # normalizes to `src/x` which won't match the allowlist.
        norm = os.path.normpath(path)
        if norm.startswith("..") or os.path.isabs(norm):
            # Defense-in-depth: a path that escapes the repo root is
            # never something publish should ship.
            bad.append(path)
            continue
        if not norm.startswith(_PUBLISH_ALLOWED_PREFIXES):
            bad.append(path)
        else:
            paths_to_add.append(path)

    if bad:
        raise SystemExit(
            "publish refuses: changes outside website/ + CLAUDE.md detected:\n  "
            + "\n  ".join(bad)
            + "\nUse a normal commit (with full pytest run) for these."
        )

    if not paths_to_add:
        raise SystemExit("no website/ or CLAUDE.md changes to publish")

    branch = _git(c, "rev-parse --abbrev-ref HEAD")
    print(f"=== Committing {len(paths_to_add)} path(s) on '{branch}' ===")
    add_cmd = "git -C " + shlex.quote(str(REPO_ROOT)) + " add -- " + " ".join(
        shlex.quote(p) for p in paths_to_add
    )
    c.run(add_cmd, pty=True)
    # `shlex.quote` for the messages — using ``{message!r}`` would
    # escape embedded newlines as the literal two-character ``\n``,
    # which the shell hands straight to git verbatim, so multi-line
    # commit bodies came out as one line with literal ``\n`` in the
    # log. shlex.quote preserves real newlines.
    coauthor = "Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
    c.run(
        f"git -C {shlex.quote(str(REPO_ROOT))} commit "
        f"-m {shlex.quote(message)} -m {shlex.quote(coauthor)}",
        pty=True,
    )

    print(f"=== Pushing {branch} ===")
    c.run(f"git -C {shlex.quote(str(REPO_ROOT))} push origin {branch}", pty=True)

    if no_deploy:
        print(
            f"=== Committed and pushed '{branch}'; skipping deploy (--no-deploy) ===\n"
            f"    The site builds from main. Open a PR, and once it's merged:\n"
            f"      git checkout main && git pull\n"
            f"      cd website && uv run python -m invoke deploy"
        )
        return

    print("=== Deploying ===")
    deploy(c)
