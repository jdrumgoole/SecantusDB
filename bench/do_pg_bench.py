#!/usr/bin/env python3
"""Measure `secantusd-pg` against PostgreSQL 16 on the benchmark server droplet.

This is the measurement behind the table on the site's Rust PostgreSQL page:
a RELEASED `secantusd-pg` binary and PostgreSQL 16 at its defaults, on one
DigitalOcean droplet, every commit synced to disk on both, driven by
`bench/pg_concurrency.py` over loopback. It was done by hand until
2026-10-12, which is how it went four releases without being repeated.

The droplet must exist first (`invoke do-up --server-size c-16`); this script
never creates or destroys one, so it never starts or stops spending. On the
server droplet it:

1. installs PostgreSQL 16 and psycopg, and lets local connections in;
2. downloads the `secantusd-pg` release named by `--version`, checks its
   SHA-256 against the release's, and reads the source tree it was built from;
3. runs four sweeps (insert into a table each, insert into one shared table,
   update by primary key, select by primary key), 10 s a trial, three trials;
4. copies the raw trials back to `bench/results/pg-droplet/` and prints the
   table.

Usage::

    python bench/do_pg_bench.py [--version 0.1.0-beta.13] [--host ADDRESS]

`--host` is the server droplet's public address; without it the address is
read from `do-cluster status`. `--render-only DIR` prints the table from
trials already fetched.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import shlex
import statistics
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RESULTS = REPO / "bench" / "results" / "pg-droplet"
DO_CLUSTER = (
    "cargo run --quiet --release --manifest-path crates/Cargo.toml -p secantus-bench "
    "--bin do-cluster --"
)
#: The files `bench/pg_concurrency.py` needs beside itself.
HARNESS = (
    "bench/pg_concurrency.py",
    "bench/__init__.py",
    "tools/__init__.py",
    "tools/provenance.py",
)
#: (workload, mode, client counts, the row label the site uses)
SWEEPS = (
    ("insert", "distinct", "1,4,8", "insert, one table each"),
    ("insert", "shared", "8", "insert, shared table"),
    ("update", "distinct", "1,4,8", "update by primary key"),
    ("select", "distinct", "1,4,8", "select by primary key"),
)


def _latest_version() -> str:
    """The newest `secantusd-pg-v*` tag this checkout knows."""
    out = subprocess.run(
        ["git", "tag", "-l", "secantusd-pg-v*", "--sort=-creatordate"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    if not out:
        raise SystemExit("no secantusd-pg-v* tag here; pass --version")
    return out[0].removeprefix("secantusd-pg-v")


def _server_address() -> str:
    """The server droplet's public address, from `do-cluster status`."""
    out = subprocess.run(
        f"{DO_CLUSTER} status", shell=True, cwd=REPO, capture_output=True, text=True
    ).stdout
    for line in out.splitlines():
        parts = line.split()
        if parts[:1] == ["server"]:
            found = [p for p in parts if re.fullmatch(r"\d+\.\d+\.\d+\.\d+", p)]
            if found:
                return found[0]
    raise SystemExit(
        "no server droplet is up (`do-cluster status` shows none). Create one with "
        "`invoke do-up --server-size c-16`, or pass --host."
    )


def _ssh_options(key: Path) -> list[str]:
    # The harness's own known_hosts (`secantus-bench`'s `remote.rs`), so a
    # droplet `do-cluster` has already met is not a stranger here.
    state = Path(os.environ.get("SECANTUS_BENCH_STATE", REPO / "bench" / ".do-state"))
    state.mkdir(parents=True, exist_ok=True)
    known = state / "known_hosts"
    return [
        "-i",
        str(key),
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        f"UserKnownHostsFile={known}",
        "-o",
        "ConnectTimeout=20",
    ]


def _ssh(host: str, key: Path, script: str, *, check: bool = True) -> str:
    done = subprocess.run(
        ["ssh", *_ssh_options(key), f"root@{host}", script], capture_output=True, text=True
    )
    if check and done.returncode != 0:
        raise SystemExit(f"on {host}: exit {done.returncode}\n{done.stdout}\n{done.stderr}")
    return done.stdout


def _remote_script(version: str, seconds: float, repeat: int) -> str:
    name = f"secantusd-pg-{version}-x86_64-unknown-linux-gnu"
    url = f"https://github.com/jdrumgoole/SecantusDB/releases/download/secantusd-pg-v{version}"
    sweeps = "\n".join(
        f"$P bench/pg_concurrency.py --workload {w} --mode {m} --clients {c} "
        f'--seconds {seconds} --repeat {repeat} --pg-dsn "$DSN" '
        f"--json /root/out/{w}-{m}.json"
        for w, m, c, _ in SWEEPS
    )
    return f"""set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq postgresql-16 python3-venv curl >/dev/null
[ -x /root/venv/bin/python ] || python3 -m venv /root/venv
/root/venv/bin/pip -q install "psycopg[binary]==3.3.4"
sed -i "s|^host *all *all *127.0.0.1/32.*|host all all 127.0.0.1/32 trust|" \\
    /etc/postgresql/16/main/pg_hba.conf
systemctl restart postgresql
cd /root
curl -fsSL -o pg.tgz {url}/{name}.tar.gz
want=$(curl -fsSL {url}/{name}.tar.gz.sha256 | cut -d' ' -f1)
got=$(sha256sum pg.tgz | cut -d' ' -f1)
[ "$want" = "$got" ] || {{ echo "checksum: release says $want, download is $got" >&2; exit 1; }}
tar xzf pg.tgz
rm -rf /root/repo /root/out /root/tmp && mkdir -p /root/repo /root/out /root/tmp
tar xzf /root/harness.tgz -C /root/repo 2>/dev/null
B=/root/{name}/secantusd-pg
DSN="host=127.0.0.1 port=5432 dbname=postgres user=postgres"
{{
  echo "binary_version=$($B --version | head -1)"
  echo "binary_tree=$($B --version | sed -n 's/^tree: //p')"
  echo "postgres=$(psql "$DSN" -Atc 'show server_version')"
  echo "fsync=$(psql "$DSN" -Atc 'show fsync')"
  echo "synchronous_commit=$(psql "$DSN" -Atc 'show synchronous_commit')"
  echo "vcpus=$(nproc)"
  echo "filesystem=$(df -T /root | tail -1 | awk '{{print $2}}')"
  echo "os=$(. /etc/os-release; echo "$PRETTY_NAME")"
}} > /root/out/facts.txt
cd /root/repo
# The droplet has no checkout to compare the binary with; the tree it reports
# is recorded above and compared with the tag by the caller.
export SECANTUSD_PG=$B SECANTUS_ALLOW_STALE_ARTIFACT=1 TMPDIR=/root/tmp
P=/root/venv/bin/python
{sweeps}
"""


def _fetch(host: str, key: Path, into: Path) -> None:
    into.mkdir(parents=True, exist_ok=True)
    names = [f"{w}-{m}.json" for w, m, _, _ in SWEEPS] + ["facts.txt"]
    for name in names:
        subprocess.run(
            ["scp", "-q", *_ssh_options(key), f"root@{host}:/root/out/{name}", str(into / name)],
            check=True,
        )


def _facts(results: Path) -> dict[str, str]:
    path = results / "facts.txt"
    if not path.exists():
        return {}
    return dict(ln.split("=", 1) for ln in path.read_text().splitlines() if "=" in ln)


def render(results: Path) -> str:
    """The comparison as a Markdown table, from the fetched trials."""
    facts = _facts(results)
    lines = [
        "| Workload | Clients | PostgreSQL 16 | secantusd-pg | PostgreSQL is faster by | spread |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for workload, mode, _, label in SWEEPS:
        data = json.loads((results / f"{workload}-{mode}.json").read_text())
        ours = {t["clients"]: t["rates"] for t in data["servers"]["rust"]}
        for trial in data["servers"]["postgres"]:
            clients = trial["clients"]
            pg, rust = statistics.median(trial["rates"]), statistics.median(ours[clients])

            def spread(rates: list[float]) -> float:
                return 100.0 * (max(rates) - min(rates)) / statistics.median(rates)

            lines.append(
                f"| {label} | {clients} | {pg:,.0f} ops/s | {rust:,.0f} ops/s | "
                f"{pg / rust:.2f}x | {spread(trial['rates']):.1f}% / "
                f"{spread(ours[clients]):.1f}% |"
            )
    if facts:
        lines.append("")
        lines.append(
            f"{facts.get('binary_version', '?')} (tree {facts.get('binary_tree', '?')[:12]}), "
            f"PostgreSQL {facts.get('postgres', '?')} (fsync {facts.get('fsync', '?')}, "
            f"synchronous_commit {facts.get('synchronous_commit', '?')}), "
            f"{facts.get('vcpus', '?')} vCPUs, {facts.get('filesystem', '?')}, "
            f"{facts.get('os', '?')}."
        )
    return "\n".join(lines)


def _tag_tree(version: str) -> str | None:
    done = subprocess.run(
        ["git", "rev-parse", f"secantusd-pg-v{version}^{{commit}}:crates"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    return done.stdout.strip() if done.returncode == 0 else None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--version", default=None, help="secantusd-pg release (default: newest tag)")
    ap.add_argument("--host", default=None, help="the server droplet's public address")
    ap.add_argument("--ssh-key", type=Path, default=Path.home() / ".ssh" / "secantus-bench")
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--render-only", type=Path, default=None, help="print a fetched run's table")
    args = ap.parse_args(argv)

    if args.render_only:
        print(render(args.render_only))
        return 0

    version = args.version or _latest_version()
    host = args.host or _server_address()
    print(f"measuring secantusd-pg {version} against PostgreSQL 16 on {host}")
    with tempfile.TemporaryDirectory() as tmp:
        bundle = Path(tmp) / "harness.tgz"
        with tarfile.open(bundle, "w:gz") as tar:
            for name in HARNESS:
                tar.add(REPO / name, arcname=name)
        subprocess.run(
            ["scp", "-q", *_ssh_options(args.ssh_key), str(bundle), f"root@{host}:/root/"],
            check=True,
        )
    minutes = sum(len(c.split(",")) for _, _, c, _ in SWEEPS) * args.repeat * args.seconds * 2 / 60
    print(f"installing, then four sweeps: about {minutes + 3:.0f} minutes")
    sys.stdout.write(_ssh(host, args.ssh_key, _remote_script(version, args.seconds, args.repeat)))

    stamp = datetime.date.today().isoformat()
    into = RESULTS / f"{stamp}-{version}"
    _fetch(host, args.ssh_key, into)
    built_from, tagged = _facts(into).get("binary_tree"), _tag_tree(version)
    if tagged and built_from != tagged:
        raise SystemExit(
            f"the binary reports tree {built_from}; the tag secantusd-pg-v{version} is {tagged}"
        )
    print(f"\nraw trials: {os.path.relpath(into, REPO)}\n")
    print(render(into))
    print(f"\nThe droplets are still running. `{shlex.join(['invoke', 'do-destroy'])}` ends them.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
