"""Command-line entry point for ``secantus-opsboard``.

Settings resolve CLI flag > env var > saved config file > default (see
``secantus.opsboard.config``). ``--save`` persists the resolved non-secret
config; the auth token is a secret kept in its own file / env / flag.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import secrets
import sys
from pathlib import Path

from secantus.opsboard.config import (
    DEFAULT_CONFIG_PATH,
    ENV_TOKEN,
    OpsboardConfig,
)

_DEFAULT_TOKEN_PATH = Path.home() / ".secantus" / "opsboard-token"


def _resolve_token(*, override: str | None, token_path: Path) -> str:
    """Token precedence: --token > $SECANTUS_OPSBOARD_TOKEN > persisted file >
    freshly generated (and persisted)."""
    if override:
        return override
    env_token = os.environ.get(ENV_TOKEN)
    if env_token:
        return env_token
    if token_path.exists():
        contents = token_path.read_text(encoding="utf-8").strip()
        if contents:
            return contents
    token = secrets.token_urlsafe(32)
    token_path.parent.mkdir(parents=True, exist_ok=True)
    token_path.write_text(token + "\n", encoding="utf-8")
    with contextlib.suppress(OSError):
        os.chmod(token_path, 0o600)
    return token


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="secantus-opsboard",
        description=(
            "Local web app to drive the SecantusDB build/test/release cycle. "
            "Settings resolve: CLI flag > env var > saved config > default."
        ),
    )
    # Defaults are None so we can tell "unset" from an explicit value and let
    # env / saved-config fill in (see OpsboardConfig.resolve).
    parser.add_argument("--host", default=None, help="Bind host (default 127.0.0.1).")
    parser.add_argument("--port", type=int, default=None, help="Local HTTP port (0 = pick free).")
    parser.add_argument(
        "--repo-root",
        default=None,
        help="Repo the board drives (default: the checkout it ships from).",
    )
    win = parser.add_mutually_exclusive_group()
    win.add_argument(
        "--no-window",
        dest="no_window",
        action="store_const",
        const=True,
        default=None,
        help="Headless (no pywebview window). Useful for CI.",
    )
    win.add_argument(
        "--window",
        dest="no_window",
        action="store_const",
        const=False,
        help="Force the pywebview window (override a saved/env no-window).",
    )
    parser.add_argument("--db-path", default=None, help="Job journal sqlite path.")
    parser.add_argument("--log-dir", default=None, help="Per-job logfile directory.")
    parser.add_argument("--token", default=None, help="Override the auth token (secret).")
    parser.add_argument(
        "--token-path",
        default=str(_DEFAULT_TOKEN_PATH),
        help="Where to read/persist the default token (ignored if --token is set).",
    )
    parser.add_argument(
        "--config",
        default=None,
        help=f"Config file to read/save (default {DEFAULT_CONFIG_PATH}).",
    )
    parser.add_argument(
        "--save",
        action="store_true",
        help="Persist the resolved (non-secret) config to the config file, then run.",
    )
    parser.add_argument(
        "--print-config",
        action="store_true",
        help="Print the resolved config as JSON and exit (no server).",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)

    config_path = Path(args.config) if args.config else None
    cfg = OpsboardConfig.resolve(
        cli={
            "host": args.host,
            "port": args.port,
            "repo_root": args.repo_root,
            "no_window": args.no_window,
            "db_path": args.db_path,
            "log_dir": args.log_dir,
        },
        config_path=config_path,
    )

    if args.save:
        saved = cfg.save(config_path or DEFAULT_CONFIG_PATH)
        sys.stderr.write(f"saved config to {saved}\n")

    if args.print_config:
        sys.stdout.write(json.dumps(cfg.to_dict(), indent=2) + "\n")
        return 0

    # Make the jobkit locations visible to the web app's Journal AND to any
    # spawned ``./inv`` child, so all three agree on where the journal lives.
    cfg.export_env()

    from secantus.opsboard._proc_name import set_process_name

    set_process_name("SecantusDB Ops Board")

    token = _resolve_token(override=args.token, token_path=Path(args.token_path))

    try:
        from secantus.opsboard.launcher import run
    except ModuleNotFoundError as exc:
        sys.stderr.write(
            f"\nThe Ops Board requires the 'opsboard' extra "
            f"(missing dependency: {exc.name}).\n\n"
            f"Install it with one of:\n"
            f"  uv sync --extra opsboard\n"
            f"  uv run --extra opsboard secantus-opsboard\n\n"
            f"From a checkout, ``uv run --extra opsboard invoke opsboard`` works too.\n"
        )
        return 1

    return run(
        host=cfg.host,
        port=cfg.port,
        repo_root=cfg.repo_root,
        token=token,
        no_window=cfg.no_window,
        journal_path=cfg.db_path,
    )


if __name__ == "__main__":
    sys.exit(main())
