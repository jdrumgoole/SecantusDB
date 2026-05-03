from __future__ import annotations

from secantus.cli import build_parser


def test_defaults() -> None:
    args = build_parser().parse_args([])
    assert args.host == "127.0.0.1"
    assert args.port == 27117
    assert args.storage_path == ":memory:"
    assert args.log_level == "INFO"


def test_storage_path_overrides_default() -> None:
    args = build_parser().parse_args(["--storage-path", "/var/lib/secantus/cellar"])
    assert args.storage_path == "/var/lib/secantus/cellar"


def test_all_flags_together() -> None:
    args = build_parser().parse_args(
        [
            "--host", "0.0.0.0",
            "--port", "27018",
            "--storage-path", "/tmp/secantus-data",
            "--log-level", "DEBUG",
        ]
    )
    assert args.host == "0.0.0.0"
    assert args.port == 27018
    assert args.storage_path == "/tmp/secantus-data"
    assert args.log_level == "DEBUG"
