from __future__ import annotations

import argparse
import logging
import signal
import sys
from types import FrameType

from secantus.server import SecantusDBServer


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Run a SecantusDB standalone single-node MongoDB server "
            "speaking the pymongo wire protocol."
        ),
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=27017)
    parser.add_argument(
        "--storage-path",
        default="./secantus-data",
        metavar="PATH",
        help=(
            "WiredTiger home directory (default: './secantus-data'). Created "
            "if missing; reopened intact across restarts. Pass ':memory:' "
            "for an ephemeral temp dir cleaned up on shutdown (test mode)."
        ),
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
    )
    parser.add_argument(
        "--auth",
        action="store_true",
        help=(
            "Require SCRAM-SHA-256 authentication for non-handshake commands. "
            "Provision users by connecting once without auth and running "
            "createUser, then restart with --auth. Off by default."
        ),
    )
    parser.add_argument(
        "--noop-heartbeat-seconds",
        type=float,
        default=0.0,
        metavar="SECONDS",
        help=(
            "Emit a periodic ``{op: 'n'}`` oplog heartbeat every N seconds "
            "so quiet change-stream cursors keep their resume token inside "
            "the oplog retention window (mongod's default is 10s). 0 = "
            "disabled (the default; embedded test users typically don't "
            "want the extra writes)."
        ),
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(
        level=args.log_level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    server = SecantusDBServer(
        host=args.host,
        port=args.port,
        storage_path=args.storage_path,
        require_auth=args.auth,
        noop_heartbeat_seconds=args.noop_heartbeat_seconds,
    )

    def handle_signal(signum: int, frame: FrameType | None) -> None:
        server.stop()
        sys.exit(0)

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    server.start()
    server.wait()
    return 0
