#!/usr/bin/env python3
"""Measure `secantusd-pg` against PostgreSQL 16 on the benchmark server droplet.

This is the measurement behind the table on the site's Rust PostgreSQL page:
a RELEASED `secantusd-pg` binary and PostgreSQL 16 at its defaults, on one
DigitalOcean droplet, every commit synced to disk on both, driven by
`bench/pg_concurrency.py` over loopback. It was done by hand until
2026-10-12, which is how it went four releases without being repeated.

The droplets must exist first (`invoke do-up --server-size c-16
--client-size c-8`); this script never creates or destroys one, so it never
starts or stops spending. It:

1. installs PostgreSQL 16 on the server droplet, and psycopg where the
   clients run;
2. downloads the `secantusd-pg` release named by `--version`, checks its
   SHA-256 against the release's, and reads the source tree it was built from;
3. runs four sweeps (insert into a table each, insert into one shared table,
   update by primary key, select by primary key), 10 s a trial, three trials;
4. copies the raw trials back to `bench/results/pg-droplet/` and prints the
   table.

**The clients run on the two client droplets by default** (`--clients-on
droplets`), half on each, over the cluster's private network, every client
starting its window at the same instant. The two servers take turns on the
one port the cluster's firewall opens. `--clients-on server` runs them beside
the server over loopback, which is how this was first measured: there a lone
client and the server share CPUs, and a single trial of one or four clients
moved by up to 21% (`bench/results/pg-droplet/2026-10-12-low-client-noise/`).

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
    "bench/pg_remote_client.py",
    "bench/__init__.py",
    "tools/__init__.py",
    "tools/provenance.py",
)
#: The one port the cluster's firewall opens between its droplets
#: (`secantus-bench`'s `SERVER_PORT`). Both servers take turns on it.
WIRE_PORT = 27017
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


def _addresses() -> dict[str, str]:
    """Each droplet's public address by role, from `do-cluster status`."""
    out = subprocess.run(
        f"{DO_CLUSTER} status", shell=True, cwd=REPO, capture_output=True, text=True
    ).stdout
    found: dict[str, str] = {}
    for line in out.splitlines():
        parts = line.split()
        if parts[:1] and parts[0] in ("server", "client-1", "client-2"):
            ips = [p for p in parts if re.fullmatch(r"\d+\.\d+\.\d+\.\d+", p)]
            if ips:
                found[parts[0]] = ips[0]
    return found


def _server_address() -> str:
    """The server droplet's public address."""
    found = _addresses()
    if "server" not in found:
        raise SystemExit(
            "no server droplet is up (`do-cluster status` shows none). Create one with "
            "`invoke do-up --server-size c-16`, or pass --host."
        )
    return found["server"]


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


_INSTALL_CLIENT = """set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq python3-venv >/dev/null
[ -x /root/venv/bin/python ] || python3 -m venv /root/venv
/root/venv/bin/pip -q install "psycopg[binary]==3.3.4"
rm -rf /root/repo && mkdir -p /root/repo && tar xzf /root/harness.tgz -C /root/repo 2>/dev/null
"""


def _install_server(version: str) -> str:
    """PostgreSQL 16 and the release binary on the server droplet, both set to
    answer the cluster's other droplets on `WIRE_PORT`, neither left running."""
    name = f"secantusd-pg-{version}-x86_64-unknown-linux-gnu"
    url = f"https://github.com/jdrumgoole/SecantusDB/releases/download/secantusd-pg-v{version}"
    conf = "/etc/postgresql/16/main"
    return f"""set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq postgresql-16 curl >/dev/null
systemctl stop postgresql
sed -i -e "s|^#*listen_addresses.*|listen_addresses = '*'|" \\
    -e "s|^port = .*|port = {WIRE_PORT}|" {conf}/postgresql.conf
sed -i "s|^host *all *all *127.0.0.1/32.*|host all all 127.0.0.1/32 trust|" {conf}/pg_hba.conf
grep -q "10.0.0.0/8" {conf}/pg_hba.conf \\
    || echo "host all all 10.0.0.0/8 trust" >> {conf}/pg_hba.conf
cd /root
curl -fsSL -o pg.tgz {url}/{name}.tar.gz
want=$(curl -fsSL {url}/{name}.tar.gz.sha256 | cut -d' ' -f1)
got=$(sha256sum pg.tgz | cut -d' ' -f1)
[ "$want" = "$got" ] || {{ echo "checksum: release says $want, download is $got" >&2; exit 1; }}
tar xzf pg.tgz
ln -sf /root/{name}/secantusd-pg /root/secantusd-pg
mkdir -p /root/out
systemctl start postgresql
sleep 2
DSN="host=127.0.0.1 port={WIRE_PORT} dbname=postgres user=postgres"
{{
  echo "binary_version=$(/root/secantusd-pg --version | head -1)"
  echo "binary_tree=$(/root/secantusd-pg --version | sed -n 's/^tree: //p')"
  echo "postgres=$(psql "$DSN" -Atc 'show server_version')"
  echo "fsync=$(psql "$DSN" -Atc 'show fsync')"
  echo "synchronous_commit=$(psql "$DSN" -Atc 'show synchronous_commit')"
  echo "vcpus=$(nproc)"
  echo "filesystem=$(df -T /root | tail -1 | awk '{{print $2}}')"
  echo "os=$(. /etc/os-release; echo "$PRETTY_NAME")"
  echo "private=$(hostname -I | tr ' ' '\\n' | grep '^10\\.' | tail -1)"
}} > /root/out/facts.txt
systemctl stop postgresql
cat /root/out/facts.txt
"""


#: Start one server on the wire port with the other stopped. PostgreSQL keeps
#: its cluster between sweeps; this server starts on an empty store each time.
_START = {
    "postgres": (
        "pkill -x secantusd-pg; sleep 1; systemctl start postgresql; sleep 2; "
        f"ss -ltn | grep -q ':{WIRE_PORT} '"
    ),
    "rust": (
        "systemctl stop postgresql; pkill -x secantusd-pg; sleep 1; rm -rf /root/store; "
        f"nohup /root/secantusd-pg /root/store 0.0.0.0:{WIRE_PORT} "
        ">/root/secantusd.log 2>&1 </dev/null & "
        f"sleep 3; ss -ltn | grep -q ':{WIRE_PORT} '"
    ),
}


def _split(clients: int) -> tuple[int, int]:
    """How many clients each of the two client droplets runs."""
    return (clients + 1) // 2, clients // 2


def _remote_trial(
    hosts: tuple[str, str],
    key: Path,
    dsn: str,
    tables: list[str],
    workload: str,
    clients: int,
    seconds: float,
) -> tuple[float, float]:
    """One trial with the clients on the client droplets: (ops/s, the busier
    client droplet's CPU use in percent)."""
    from concurrent.futures import ThreadPoolExecutor

    first, second = _split(clients)
    # The window opens at a time on the DROPLETS' clock, far enough ahead for
    # both commands to have arrived and their processes to have started.
    start_at = float(_ssh(hosts[0], key, "date +%s.%N").strip()) + 6.0
    shares = [(hosts[0], first, 0)] + ([(hosts[1], second, first)] if second else [])

    def one(share: tuple[str, int, int]) -> dict:
        host, count, base = share
        mine = tables[base : base + count] if len(tables) > 1 else tables
        out = _ssh(
            host,
            key,
            "cd /root/repo && /root/venv/bin/python bench/pg_remote_client.py "
            f"--dsn {shlex.quote(dsn)} --tables {','.join(mine)} --workload {workload} "
            f"--clients {count} --seconds {seconds} --start-at {start_at} --slot-base {base}",
            check=False,
        )
        try:
            return json.loads(out.strip().splitlines()[-1])
        except (IndexError, ValueError):
            raise SystemExit(f"{host} reported no result for a trial:\n{out}") from None

    with ThreadPoolExecutor(len(shares)) as pool:
        results = list(pool.map(one, shares))
    errors = [e for r in results for e in r["errors"]]
    if errors:
        raise SystemExit(f"{len(errors)} client(s) failed, first: {errors[0]}")
    committed = sum(r["committed"] for r in results)
    return committed / seconds, max(r["cpu_busy_pct"] for r in results)


def _measure_remote(
    server: str, clients: tuple[str, str], key: Path, version: str, seconds: float, repeat: int
) -> Path:
    """The four sweeps with the clients on the two client droplets."""
    for host in clients:
        _ssh(host, key, _INSTALL_CLIENT)
    sys.stdout.write(_ssh(server, key, _install_server(version)))
    sys.stdout.flush()
    # A directory of its own: the same release measured with the clients
    # beside the server is a different measurement, and is kept beside it.
    into = RESULTS / f"{datetime.date.today().isoformat()}-{version}-remote-clients"
    into.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["scp", "-q", *_ssh_options(key), f"root@{server}:/root/out/facts.txt", str(into)],
        check=True,
    )
    private = _facts(into)["private"]
    dsn = f"host={private} port={WIRE_PORT} dbname=postgres user=postgres"
    prepare = (
        "cd /root/repo && /root/venv/bin/python -c "
        + shlex.quote(
            "import sys; from bench.pg_concurrency import _prepare; "
            "_prepare(sys.argv[1], sys.argv[2].split(','), sys.argv[3])"
        )
        + f" {shlex.quote(dsn)} "
    )
    for workload, mode, counts, label in SWEEPS:
        collected: dict[str, list[dict]] = {}
        for name in ("postgres", "rust"):
            _ssh(server, key, _START[name])
            collected[name] = []
            for count in (int(c) for c in counts.split(",")):
                tables = (
                    [f"bench_{i}" for i in range(count)] if mode == "distinct" else ["bench_shared"]
                )
                rates, busy = [], []
                for _ in range(repeat):
                    # Fresh tables for every trial, as the loopback harness has.
                    _ssh(clients[0], key, prepare + f"{','.join(tables)} {workload}")
                    rate, cpu = _remote_trial(clients, key, dsn, tables, workload, count, seconds)
                    rates.append(rate)
                    busy.append(cpu)
                collected[name].append({"clients": count, "rates": rates, "client_cpu_busy": busy})
                print(
                    f"{label:24} {name:8} {count} clients: "
                    f"{statistics.median(rates):>9,.0f} ops/s  "
                    f"(clients' CPUs {max(busy):.0f}% busy at most)",
                    flush=True,
                )
        (into / f"{workload}-{mode}.json").write_text(
            json.dumps(
                {
                    "workload": workload,
                    "mode": mode,
                    "seconds": seconds,
                    "repeat": repeat,
                    "clients_on": "two client droplets",
                    "servers": collected,
                },
                indent=2,
            )
            + "\n"
        )
    _ssh(server, key, "pkill -x secantusd-pg; systemctl stop postgresql; true", check=False)
    return into


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
    placement = json.loads((results / "insert-distinct.json").read_text()).get("clients_on")
    if placement:
        busiest = max(
            cpu
            for workload, mode, _, _ in SWEEPS
            for server in json.loads((results / f"{workload}-{mode}.json").read_text())[
                "servers"
            ].values()
            for trial in server
            for cpu in trial.get("client_cpu_busy", [0.0])
        )
        lines.append("")
        lines.append(
            f"Clients on {placement}, over the private network; the busiest a client "
            f"droplet's CPUs got in any trial was {busiest:.0f}%."
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


def _main_remote(args: argparse.Namespace, version: str) -> int:
    found = _addresses()
    missing = [r for r in ("server", "client-1", "client-2") if r not in found]
    if missing:
        raise SystemExit(
            f"not up: {', '.join(missing)} (`do-cluster status`). Create them with "
            "`invoke do-up --server-size c-16 --client-size c-8`."
        )
    print(
        f"measuring secantusd-pg {version} against PostgreSQL 16 on {found['server']}, "
        f"clients on {found['client-1']} and {found['client-2']}",
        flush=True,
    )
    with tempfile.TemporaryDirectory() as tmp:
        bundle = Path(tmp) / "harness.tgz"
        with tarfile.open(bundle, "w:gz") as tar:
            for name in HARNESS:
                tar.add(REPO / name, arcname=name)
        for role in ("client-1", "client-2"):
            subprocess.run(
                ["scp", "-q", *_ssh_options(args.ssh_key), str(bundle)]
                + [f"root@{found[role]}:/root/"],
                check=True,
            )
    into = _measure_remote(
        found["server"],
        (found["client-1"], found["client-2"]),
        args.ssh_key,
        version,
        args.seconds,
        args.repeat,
    )
    built_from, tagged = _facts(into).get("binary_tree"), _tag_tree(version)
    if tagged and built_from != tagged:
        raise SystemExit(
            f"the binary reports tree {built_from}; the tag secantusd-pg-v{version} is {tagged}"
        )
    print(f"\nraw trials: {os.path.relpath(into, REPO)}\n")
    print(render(into))
    print("\nThe droplets are still running. `invoke do-destroy` ends them.")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--version", default=None, help="secantusd-pg release (default: newest tag)")
    ap.add_argument("--host", default=None, help="the server droplet's public address")
    ap.add_argument("--ssh-key", type=Path, default=Path.home() / ".ssh" / "secantus-bench")
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--render-only", type=Path, default=None, help="print a fetched run's table")
    ap.add_argument(
        "--clients-on",
        choices=["droplets", "server"],
        default="droplets",
        help="where the clients run: the two client droplets (default), or beside the server",
    )
    args = ap.parse_args(argv)

    if args.render_only:
        print(render(args.render_only))
        return 0

    version = args.version or _latest_version()
    if args.clients_on == "droplets":
        return _main_remote(args, version)
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
