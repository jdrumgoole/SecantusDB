from __future__ import annotations

import argparse
import datetime
import logging
import signal
import sys
from pathlib import Path
from types import FrameType

from secantus.config import (
    ConfigError,
    SecantusConfig,
    apply_overrides,
    load_config,
)
from secantus.server import SecantusDBServer

log = logging.getLogger(__name__)


def build_parser() -> argparse.ArgumentParser:
    """Argparse parser for the ``secantusdb`` daemon.

    Every value-bearing flag defaults to ``None``. ``None`` is the
    sentinel main() uses to decide "user passed this" vs "user did
    not pass this, fall back to TOML / SecantusConfig default."
    Without it, every argparse default would silently override the
    TOML file.
    """
    parser = argparse.ArgumentParser(
        description=(
            "Run a SecantusDB standalone single-node MongoDB server "
            "speaking the pymongo wire protocol. Flags override values "
            "in secantusd.toml; secantusd.toml overrides built-in "
            "defaults."
        ),
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=None,
        metavar="PATH",
        help=(
            "Path to a secantusd.toml configuration file. When omitted, "
            "the launcher auto-discovers ./secantusd.toml, "
            "~/.secantus/secantusd.toml, /etc/secantus/secantusd.toml "
            "(and the legacy secantusdb.toml at each location) — first "
            "hit wins. Passing this flag disables auto-discovery."
        ),
    )
    parser.add_argument("--host", default=None)
    parser.add_argument("--port", type=int, default=None)
    parser.add_argument(
        "--storage-path",
        default=None,
        metavar="PATH",
        help=(
            "WiredTiger home directory (default: './secantus-data'). Created "
            "if missing; reopened intact across restarts. Pass ':memory:' "
            "for an ephemeral temp dir cleaned up on shutdown (test mode)."
        ),
    )
    parser.add_argument(
        "--log-level",
        default=None,
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
    )
    parser.add_argument(
        "--auth",
        action="store_true",
        default=None,
        help=(
            "Require SCRAM-SHA-256 authentication for non-handshake commands. "
            "Provision users by connecting once without auth and running "
            "createUser, then restart with --auth. Off by default."
        ),
    )
    parser.add_argument(
        "--standalone",
        action="store_true",
        default=None,
        help=(
            "Drop the single-node replica-set advertisement from the "
            "``hello`` reply, so drivers see SecantusDB as a STANDALONE "
            "topology. Default is to advertise as a single-node "
            "``secantus`` replica-set primary so pymongo's change-stream "
            "machinery accepts the topology. Test gauges that need the "
            "driver's single-node code path (e.g. mongo-java-driver's "
            "``ClusterFixture.getSecondary()`` would otherwise loop "
            "forever waiting for a SECONDARY) opt into this."
        ),
    )
    parser.add_argument(
        "--noop-heartbeat-seconds",
        type=float,
        default=None,
        metavar="SECONDS",
        help=(
            "Emit a periodic ``{op: 'n'}`` oplog heartbeat every N seconds "
            "so quiet change-stream cursors keep their resume token inside "
            "the oplog retention window (mongod's default is 10s). 0 = "
            "disabled (the default; embedded test users typically don't "
            "want the extra writes)."
        ),
    )
    parser.add_argument(
        "--cache-size",
        default=None,
        metavar="SIZE",
        help=(
            "WiredTiger cache size — unit-suffixed string like '256M', "
            "'1G', '8G'. Defaults to '1G'. Bigger cache = more working "
            "set in RAM = better hit rates; size to fit the hot doc "
            "subset of the dataset."
        ),
    )
    parser.add_argument(
        "--session-max",
        type=int,
        default=None,
        metavar="N",
        help=(
            "WiredTiger session_max — concurrent WT session cap. Each "
            "client connection + change-stream tailer takes one. "
            "Default 1000."
        ),
    )
    parser.add_argument(
        "--sync-on-commit",
        action="store_true",
        default=None,
        help=(
            "Fsync the WT log on every transaction commit. Closes the "
            "writeConcern j:true durability gap (true power-loss "
            "between commits no longer loses data) at a significant "
            "throughput cost (1-2 orders of magnitude on small-doc "
            "insert workloads). Off by default; matches mongod's "
            "default w:1, j:false."
        ),
    )
    parser.add_argument(
        "--oplog-retention-seconds",
        type=float,
        default=None,
        metavar="SECONDS",
        help=(
            "Oplog wall-clock retention. Entries older than this are "
            "pruned opportunistically. Default 3600 (1 hour). Tune up "
            "for resume tokens that need to survive long idle "
            "stretches; tune down for a tighter disk budget."
        ),
    )
    parser.add_argument(
        "--oplog-max-entries",
        type=int,
        default=None,
        metavar="N",
        help=(
            "Oplog count cap. Whichever bound (this or retention) "
            "hits first prunes the oldest entries. Default 100000."
        ),
    )
    parser.add_argument(
        "--oplog-archive-dir",
        default=None,
        metavar="DIR",
        help=(
            "Enable PITR v2: archive oplog rows to durable segments in DIR before "
            "pruning drops them. Pair with periodic base snapshots "
            "(secantusAdmin.archiveBaseSnapshot) so recovery can reach a time "
            "before the live oplog floor. Off by default."
        ),
    )
    parser.add_argument(
        "--tls-cert-file",
        default=None,
        metavar="PATH",
        help=(
            "PEM-format server certificate chain. When this and "
            "--tls-key-file are both set, accept()ed sockets are TLS-"
            "wrapped before the wire protocol starts; clients connect "
            "with 'mongodb://host:port/?tls=true&tlsCAFile=<ca>'. "
            "Without TLS the daemon stays plaintext (matches the "
            "existing behaviour)."
        ),
    )
    parser.add_argument(
        "--tls-key-file",
        default=None,
        metavar="PATH",
        help="PEM-format private key matching --tls-cert-file.",
    )
    parser.add_argument(
        "--tls-ca-file",
        default=None,
        metavar="PATH",
        help=(
            "PEM-format CA bundle. When set (alongside --tls-cert-file / "
            "--tls-key-file), the daemon asks connecting clients for an "
            "X.509 cert during the TLS handshake and verifies it against "
            "this CA. Required when --tls-require-client-cert is on."
        ),
    )
    parser.add_argument(
        "--tls-require-client-cert",
        action="store_true",
        default=None,
        help=(
            "Reject clients that don't present a valid X.509 cert "
            "(mTLS). Off by default — verifies a cert if one is offered "
            "but accepts clients without one (CERT_OPTIONAL). Requires "
            "--tls-ca-file. mTLS cert-as-username auth (MONGODB-X509) "
            "is a separate follow-on; this flag is the transport-layer "
            "gate only."
        ),
    )
    return parser


def _overrides_from_args(args: argparse.Namespace) -> dict[str, object]:
    """Extract only the flags the user actually passed (non-None) so
    they can be layered on top of the TOML file's values."""
    arg_to_field: dict[str, str] = {
        "host": "host",
        "port": "port",
        "storage_path": "storage_path",
        "log_level": "log_level",
        "auth": "auth",
        "standalone": "standalone",
        "noop_heartbeat_seconds": "noop_heartbeat_seconds",
        "cache_size": "cache_size",
        "session_max": "session_max",
        "sync_on_commit": "sync_on_commit",
        "oplog_retention_seconds": "oplog_retention_seconds",
        "oplog_max_entries": "oplog_max_entries",
        "oplog_archive_dir": "oplog_archive_dir",
        "tls_cert_file": "tls_cert_file",
        "tls_key_file": "tls_key_file",
        "tls_ca_file": "tls_ca_file",
        "tls_require_client_cert": "tls_require_client_cert",
    }
    overrides: dict[str, object] = {}
    for arg_name, field_name in arg_to_field.items():
        value = getattr(args, arg_name)
        if value is not None:
            overrides[field_name] = value
    return overrides


def _build_restore_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="secantusdb restore",
        description=(
            "Point-in-time recovery: rebuild a fresh data directory as the "
            "database was at a target time, by replaying a backup's oplog "
            "forward. The source must be a stopped server's data directory "
            "or a backup .tar.gz (a live data directory can't be opened — "
            "WiredTiger holds a single-writer lock). Start a new server on "
            "--target-dir afterwards."
        ),
    )
    parser.add_argument(
        "--source",
        required=True,
        metavar="PATH",
        help=(
            "Backup .tar.gz archive, a stopped server's data directory, or a "
            "PITR v2 archive directory (base snapshots + oplog segments — lets "
            "recovery reach a time before the live oplog floor)."
        ),
    )
    parser.add_argument(
        "--target-dir",
        required=True,
        metavar="PATH",
        help="Fresh directory to rebuild into (must not be a live server's path).",
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "--to-time",
        metavar="ISO8601",
        help=(
            "Recover to the last write at or before this wall-clock time, e.g. "
            "'2026-06-17T14:30:00Z'. Naive times are treated as UTC."
        ),
    )
    group.add_argument(
        "--to-timestamp",
        metavar="SECS[,ORD]",
        help="Recover to this cluster timestamp (seconds, optional ordinal).",
    )
    parser.add_argument(
        "--preserve-oplog",
        action="store_true",
        help=(
            "Carry the replayed oplog onto the restored directory so a change "
            "stream there can resume from a token minted before the restore "
            "point. Default: start a fresh oplog timeline (like mongorestore)."
        ),
    )
    return parser


def _parse_iso_utc(value: str) -> datetime.datetime:
    text = value[:-1] + "+00:00" if value.endswith("Z") else value
    dt = datetime.datetime.fromisoformat(text)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=datetime.timezone.utc)
    return dt


def _restore_main(argv: list[str]) -> int:
    from bson import Timestamp

    from secantus import oplog_replay

    args = _build_restore_parser().parse_args(argv)
    to_ts = None
    to_wall = None
    if args.to_timestamp is not None:
        secs, _, ordinal = args.to_timestamp.partition(",")
        try:
            to_ts = Timestamp(int(secs), int(ordinal) if ordinal else 0)
        except ValueError:
            print("secantusdb restore: --to-timestamp must be SECS[,ORD]", file=sys.stderr)
            return 2
    if args.to_time is not None:
        try:
            to_wall = _parse_iso_utc(args.to_time)
        except ValueError:
            print(f"secantusdb restore: cannot parse --to-time {args.to_time!r}", file=sys.stderr)
            return 2

    from secantus import pitr_archive

    try:
        if pitr_archive.is_archive_dir(args.source):
            # PITR v2: a directory of base snapshots + oplog segments.
            stats = pitr_archive.restore_from_archive_dir(
                args.source,
                args.target_dir,
                to_ts=to_ts,
                to_wall=to_wall,
                carry_oplog=args.preserve_oplog,
            )
        else:
            fn = (
                oplog_replay.restore_archive_to_timestamp
                if args.source.endswith((".tar.gz", ".tgz"))
                else oplog_replay.restore_to_timestamp
            )
            stats = fn(
                args.source,
                args.target_dir,
                to_ts=to_ts,
                to_wall=to_wall,
                carry_oplog=args.preserve_oplog,
            )
    except (ValueError, RuntimeError) as exc:
        print(f"secantusdb restore: {exc}", file=sys.stderr)
        return 2
    print(
        f"Restored {stats['opsApplied']} operations "
        f"(through oplog seq {stats['lastSeq']}) into {stats['targetDir']}.\n"
        f"Start a server on it: secantusdb --storage-path {stats['targetDir']}"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    raw = sys.argv[1:] if argv is None else argv
    if raw and raw[0] == "restore":
        # ``secantusdb restore ...`` is the only subcommand; everything else
        # is the daemon (bare ``secantusdb --flags`` keeps working unchanged).
        return _restore_main(raw[1:])
    args = build_parser().parse_args(argv)

    try:
        base, source = load_config(args.config)
        cfg: SecantusConfig = apply_overrides(base, _overrides_from_args(args))
    except ConfigError as exc:
        print(f"secantusdb: {exc}", file=sys.stderr)
        return 2

    logging.basicConfig(
        level=cfg.log_level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    if source is not None:
        log.info("loaded config from %s", source)

    server = SecantusDBServer(
        host=cfg.host,
        port=cfg.port,
        storage_path=cfg.storage_path,
        require_auth=cfg.auth,
        noop_heartbeat_seconds=cfg.noop_heartbeat_seconds,
        replica_set_name=None if cfg.standalone else "secantus",
        oplog_retention_seconds=cfg.oplog_retention_seconds,
        oplog_max_entries=cfg.oplog_max_entries,
        oplog_archive_dir=cfg.oplog_archive_dir,
        cache_size=cfg.cache_size,
        session_max=cfg.session_max,
        sync_on_commit=cfg.sync_on_commit,
        tls_cert_file=cfg.tls_cert_file,
        tls_key_file=cfg.tls_key_file,
        tls_ca_file=cfg.tls_ca_file,
        tls_require_client_cert=cfg.tls_require_client_cert,
    )

    def handle_signal(signum: int, frame: FrameType | None) -> None:
        server.stop()
        sys.exit(0)

    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    server.start()
    server.wait()
    return 0
