"""Command-line entry point for ``secantus-admin``.

Resolves the token, configures uvicorn, optionally opens a pywebview
window, and runs until shutdown. ``--no-window`` is the headless mode
used by tests and CI.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import secrets
import sys
from pathlib import Path

_DEFAULT_TOKEN_PATH = Path.home() / ".secantus" / "admin-token"


def _resolve_token(*, override: str | None, token_path: Path) -> str:
    """Pick the token to use, in order: ``--token`` flag, persisted file,
    freshly generated (and persisted)."""
    if override:
        return override
    if token_path.exists():
        contents = token_path.read_text(encoding="utf-8").strip()
        if contents:
            return contents
    token = secrets.token_urlsafe(32)
    token_path.parent.mkdir(parents=True, exist_ok=True)
    token_path.write_text(token + "\n", encoding="utf-8")
    with contextlib.suppress(OSError):
        # Best-effort on systems without chmod (Windows). The file is
        # already in the user's home dir.
        os.chmod(token_path, 0o600)
    return token


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="secantus-admin",
        description="Local admin UI for a SecantusDB (or any MongoDB) server.",
    )
    parser.add_argument(
        "--uri",
        default="mongodb://127.0.0.1:27017",
        help="MongoDB URI to administer (default: mongodb://127.0.0.1:27017).",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=0,
        help="Local HTTP port (0 = pick a free one).",
    )
    parser.add_argument(
        "--no-window",
        action="store_true",
        help="Run the FastAPI app without opening a pywebview window. Used in CI.",
    )
    parser.add_argument(
        "--token",
        default=None,
        help="Override the auth token. Default: read/generate ~/.secantus/admin-token.",
    )
    parser.add_argument(
        "--token-path",
        default=str(_DEFAULT_TOKEN_PATH),
        help="Where to read/persist the default token (ignored if --token is set).",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)

    # Override the OS-level process name before pywebview brings up the
    # AppKit window so the menu bar / Dock / Activity Monitor read
    # "SecantusDB admin" rather than "Python".
    from secantus.admin._proc_name import set_process_name

    set_process_name("SecantusDB admin")

    # Fail fast on a URI this console can't drive (most usefully a
    # postgresql:// one aimed at SecantusDB's SQL server), before we
    # generate a token or bring up a window.
    from secantus.admin.client import check_supported_uri

    try:
        check_supported_uri(args.uri)
    except ValueError as exc:
        sys.stderr.write(f"\n{exc}\n\n")
        return 2

    token = _resolve_token(override=args.token, token_path=Path(args.token_path))

    # Lazy imports — the launcher pulls in fastapi / uvicorn / pywebview,
    # all heavyweight. Importing them inside ``main`` keeps ``--help``
    # fast and avoids cost when tests construct ``create_app`` directly.
    # A missing extra is the most common error a user hits, so trade
    # the bare ``ModuleNotFoundError`` for a fix-it hint.
    try:
        from secantus.admin.launcher import run
    except ModuleNotFoundError as exc:
        sys.stderr.write(
            f"\nThe admin web UI requires the 'admin' extra "
            f"(missing dependency: {exc.name}).\n\n"
            f"Install it with one of:\n"
            f"  pip install 'secantusdb[admin]'\n"
            f"  uv sync --extra admin\n"
            f"  uv run --extra admin secantus-admin\n\n"
            f"From a checkout, ``uv run --extra admin invoke admin`` works too.\n"
        )
        return 1

    return run(
        mongo_uri=args.uri,
        port=args.port,
        token=token,
        no_window=args.no_window,
    )


if __name__ == "__main__":
    sys.exit(main())
