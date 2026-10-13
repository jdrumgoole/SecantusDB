"""`bench/do_pg_bench.py`: the droplet measurement behind the site's Rust
PostgreSQL table. The parts that need no droplet: what it runs there, and
the table it prints from what comes back."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def _module():
    spec = importlib.util.spec_from_file_location("do_pg_bench", REPO / "bench/do_pg_bench.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _trials(directory: Path, workload: str, mode: str, pg: dict, rust: dict) -> None:
    servers = {
        name: [{"clients": c, "rates": rates} for c, rates in data.items()]
        for name, data in (("postgres", pg), ("rust", rust))
    }
    (directory / f"{workload}-{mode}.json").write_text(json.dumps({"servers": servers}))


def test_the_table_reports_medians_and_how_much_faster_postgresql_is(tmp_path: Path) -> None:
    bench = _module()
    for workload, mode, counts, _ in bench.SWEEPS:
        clients = [int(c) for c in counts.split(",")]
        _trials(
            tmp_path,
            workload,
            mode,
            {c: [2000.0 * c, 2100.0 * c, 1900.0 * c] for c in clients},
            {c: [1000.0 * c, 1000.0 * c, 1100.0 * c] for c in clients},
        )
    (tmp_path / "facts.txt").write_text(
        "binary_version=secantusd-pg 0.1.0-beta.13\nbinary_tree=abcdef0123456789\n"
        "postgres=16.15\nfsync=on\nsynchronous_commit=on\nvcpus=16\nfilesystem=ext4\n"
        "os=Ubuntu 24.04.5 LTS\n"
    )
    table = bench.render(tmp_path)
    rows = [ln for ln in table.splitlines() if ln.startswith("| ") and "ops/s" in ln]
    # One row per client count per sweep: 3 + 1 + 3 + 3.
    assert len(rows) == 10
    # The MEDIAN of each side, and their ratio; the spread of each.
    assert rows[0] == (
        "| insert, one table each | 1 | 2,000 ops/s | 1,000 ops/s | 2.00x | 10.0% / 10.0% |"
    )
    assert "| insert, shared table | 8 | 16,000 ops/s | 8,000 ops/s | 2.00x |" in table
    assert "secantusd-pg 0.1.0-beta.13 (tree abcdef012345)" in table
    assert "PostgreSQL 16.15 (fsync on, synchronous_commit on), 16 vCPUs, ext4" in table


def test_the_droplet_runs_the_named_release_and_checks_its_checksum() -> None:
    bench = _module()
    script = bench._remote_script("0.1.0-beta.13", 10.0, 3)
    release = "releases/download/secantusd-pg-v0.1.0-beta.13"
    assert f"{release}/secantusd-pg-0.1.0-beta.13-x86_64-unknown-linux-gnu.tar.gz" in script
    # A download that is not the release's is an error, not a measurement.
    assert 'want" = "$got" ] ||' in script and "exit 1" in script
    assert script.startswith("set -e")
    # Every sweep, each into a file of its own.
    for workload, mode, counts, _ in bench.SWEEPS:
        assert f"--workload {workload} --mode {mode} --clients {counts} " in script
        assert f"--json /root/out/{workload}-{mode}.json" in script
    assert "--seconds 10.0 --repeat 3" in script
    # It never creates or destroys a droplet.
    assert "do-cluster" not in script and "doctl" not in script


def test_the_harness_bundle_names_files_that_exist() -> None:
    bench = _module()
    for name in bench.HARNESS:
        assert (REPO / name).exists(), name


def test_remote_clients_are_split_between_the_two_droplets() -> None:
    bench = _module()
    assert bench._split(1) == (1, 0)
    assert bench._split(4) == (2, 2)
    assert bench._split(8) == (4, 4)
    assert bench._split(5) == (3, 2)


def test_the_two_servers_take_turns_on_the_one_port_the_firewall_opens() -> None:
    """The cluster's firewall lets its droplets reach the server on one port,
    so each server is started there with the other stopped."""
    bench = _module()
    assert bench.WIRE_PORT == 27017
    assert "pkill -x secantusd-pg" in bench._START["postgres"]
    assert "systemctl start postgresql" in bench._START["postgres"]
    assert "systemctl stop postgresql" in bench._START["rust"]
    # An empty store each time, and listening beyond loopback.
    assert "rm -rf /root/store" in bench._START["rust"]
    assert "0.0.0.0:27017" in bench._START["rust"]
    for start in bench._START.values():
        assert start.endswith("grep -q ':27017 '")
    install = bench._install_server("0.1.0-beta.13")
    assert "port = 27017" in install and "listen_addresses = '*'" in install
    assert "host all all 10.0.0.0/8 trust" in install
    assert 'want" = "$got" ] ||' in install


def test_a_remote_run_says_where_the_clients_were_and_how_busy(tmp_path: Path) -> None:
    bench = _module()
    for workload, mode, counts, _ in bench.SWEEPS:
        servers = {
            name: [
                {"clients": int(c), "rates": [100.0, 110.0, 105.0], "client_cpu_busy": [9.0, 17.5]}
                for c in counts.split(",")
            ]
            for name in ("postgres", "rust")
        }
        (tmp_path / f"{workload}-{mode}.json").write_text(
            json.dumps({"clients_on": "two client droplets", "servers": servers})
        )
    table = bench.render(tmp_path)
    assert "Clients on two client droplets, over the private network" in table
    assert "was 18%." in table


def test_neither_server_is_measured_over_tls() -> None:
    """Ubuntu's PostgreSQL offers TLS and psycopg takes it unless told not to,
    so until 2026-10-13 PostgreSQL was measured encrypted and `secantusd-pg`
    in the clear. Every DSN says `sslmode=disable`, and the run records what
    PostgreSQL saw."""
    mod = _module()
    for script in (
        mod._install_server("0.1.0-beta.13"),
        mod._remote_script("0.1.0-beta.13", 10.0, 3),
    ):
        dsns = [ln for ln in script.splitlines() if ln.startswith("DSN=")]
        assert dsns and all("sslmode=disable" in ln for ln in dsns)
        assert "pg_stat_ssl" in script


def test_the_footnote_names_the_processor_the_disk_and_the_absence_of_tls(tmp_path: Path) -> None:
    for workload, mode, counts, _ in _module().SWEEPS:
        rates = {int(c): [100.0, 110.0, 120.0] for c in counts.split(",")}
        _trials(tmp_path, workload, mode, rates, rates)
    (tmp_path / "facts.txt").write_text(
        "binary_version=secantusd-pg 0.1.0-beta.13\nbinary_tree=abc\npostgres=16.15\n"
        "fsync=on\nsynchronous_commit=on\ntls=f\nvcpus=16\n"
        "cpu=Intel(R) Xeon(R) Platinum 8168 CPU @ 2.70GHz\nfdatasync_per_s=1653\n"
        "filesystem=ext4\nos=Ubuntu 24.04.5 LTS\n"
    )
    footnote = _module().render(tmp_path).splitlines()[-1]
    assert "no TLS on either server" in footnote
    assert "16 vCPUs (Intel(R) Xeon(R) Platinum 8168 CPU @ 2.70GHz)" in footnote
    assert "ext4 (1,653 fdatasync calls a second by pg_test_fsync)" in footnote
