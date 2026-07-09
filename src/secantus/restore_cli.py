"""Offline ``secantus-restore-archive`` CLI.

Extracts a SecantusDB backup archive (``.tar.gz`` produced by the
``secantusAdmin.backupArchive`` wire command, or by the admin UI's
"Run native checkpoint backup" button) into a fresh directory the
user can then point a new ``secantusdb`` process at.

This is the offline counterpart to ``secantusAdmin.restoreArchive``
— useful when the source SecantusDB isn't currently running, or
when the operator wants to restore on a different host. No server
process is started.
"""

from __future__ import annotations

import argparse
import sys

from secantus.storage import extract_backup_archive


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="secantus-restore-archive",
        description=(
            "Extract a SecantusDB backup archive into a target "
            "directory. Then start a SecantusDB server pointed at "
            "that directory: 'secantusdb --storage-path <target>'."
        ),
    )
    parser.add_argument(
        "--archive",
        required=True,
        metavar="PATH",
        help="Path to the backup archive (.tar.gz) to extract.",
    )
    parser.add_argument(
        "--target-dir",
        required=True,
        metavar="PATH",
        help=(
            "Destination directory for the restored WiredTiger home. "
            "Must not exist or must be empty (use --allow-existing to "
            "overlay)."
        ),
    )
    parser.add_argument(
        "--allow-existing",
        action="store_true",
        help=(
            "Permit extraction into a non-empty target directory. "
            "Existing files are left in place; archive files overlay. "
            "Default: refuse non-empty targets to avoid mixing two "
            "incompatible WT homes."
        ),
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        result = extract_backup_archive(
            args.archive,
            args.target_dir,
            allow_existing=args.allow_existing,
        )
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(
        f"Extracted {result['fileCount']} file(s) from {result['archive']} "
        f"into {result['targetDir']}"
    )
    print(f"Start the server with: secantusdb --storage-path {result['targetDir']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
