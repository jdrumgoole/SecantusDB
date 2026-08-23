from __future__ import annotations

import json
import pathlib
import re
import shlex
import shutil
import subprocess
import time
import urllib.error
import urllib.request

from invoke.context import Context
from invoke.tasks import task

# --- Per-server task modules ("command files that invoke imports") --------
# Python-server dev workflow (sync / test / test-one / perf / lint / fmt / serve
# / docs / docs-serve / clean + py-gate / py-ship) lives in `python_tasks`; the
# rust-* tasks (build / test / parity / gate / ship / bump / stress / per-crate
# checks) live in `rust_tasks`. Each is maintained in one place; `import *`
# brings their Task objects into this root namespace for invoke discovery, so
# `invoke test` / `invoke lint` / `invoke rust-gate` etc. are unchanged. The
# cross-cutting families — driver gauges (`validate-*`), the release pipeline
# (`release-*`), and the bench/chaos harnesses — stay below in this file.
from python_tasks import *  # noqa: E402,F401,F403
from rust_tasks import *  # noqa: E402,F401,F403
from rust_tasks import _rust_env  # noqa: E402  (underscore name: explicit import)


@task(
    name="pr-watch",
    help={
        "pr": "Pull-request number to watch.",
        "repo": "GitHub repo (default: jdrumgoole/SecantusDB).",
        "interval": "Seconds between polls (default: 25).",
        "timeout": "Give up after this many seconds (default: 3600).",
    },
)
def pr_watch(
    c: Context,
    pr: int,
    repo: str = "jdrumgoole/SecantusDB",
    interval: int = 25,
    timeout: int = 3600,
) -> None:
    """Poll a PR's CI checks until they all finish, then print the result.

    Replaces the ad-hoc ``gh pr checks`` shell poll loop used to gate
    "merge when green". Prints one line per check (``name  bucket``) once
    every check has left the pending/queued/in-progress state, and exits
    non-zero if any check did not pass — so it composes in a shell, e.g.
    ``invoke pr-watch 203 && gh pr merge 203 --rebase --delete-branch``.

    A check counts as passing only when its bucket is ``pass`` or
    ``skipping``; ``fail`` and ``cancel`` are reported and cause a
    non-zero exit (a spurious ``cancel`` is worth a human's eyes, not a
    silent merge). While GitHub reports no checks yet, it keeps waiting up
    to ``timeout``.
    """
    from invoke.exceptions import Exit

    pending = {"PENDING", "QUEUED", "IN_PROGRESS"}
    ok_buckets = {"pass", "skipping"}
    deadline = time.monotonic() + timeout
    checks: list[dict] = []
    while True:
        proc = subprocess.run(
            ["gh", "pr", "checks", str(pr), "--repo", repo, "--json", "name,state,bucket"],
            capture_output=True,
            text=True,
        )
        try:
            checks = json.loads(proc.stdout) if proc.stdout.strip() else []
        except json.JSONDecodeError:
            checks = []
        if checks and all(ch.get("state") not in pending for ch in checks):
            break
        if time.monotonic() >= deadline:
            raise Exit(f"pr-watch: timed out after {timeout}s waiting on PR #{pr}", code=2)
        time.sleep(interval)

    checks.sort(key=lambda ch: ch["name"])
    width = max((len(ch["name"]) for ch in checks), default=0)
    failed = []
    for ch in checks:
        result = ch.get("bucket") or ch.get("state") or "?"
        print(f"  {ch['name']:<{width}}  {result}")
        if result not in ok_buckets:
            failed.append(ch["name"])
    if failed:
        raise Exit(
            f"PR #{pr}: {len(failed)} of {len(checks)} check(s) not green: {', '.join(failed)}",
            code=1,
        )
    print(f"PR #{pr}: all {len(checks)} checks passed.")


@task(
    help={
        "uri": "MongoDB URI of the target server (default: mongodb://127.0.0.1:27017/).",
        "db": "Target database (default: harness).",
        "collection": "Target collection (default: inserts_8k).",
        "count": "Number of documents to insert. Omit for continuous mode.",
        "drop": "Drop the target collection before inserting.",
        "progress-every": "Print a progress line every N inserts (default: 1000; 0 to disable).",
    }
)
def load(
    c: Context,
    uri: str = "mongodb://127.0.0.1:27017/",
    db: str = "harness",
    collection: str = "inserts_8k",
    count: int = 0,
    drop: bool = False,
    progress_every: int = 1000,
) -> None:
    """Insert standard 8 KiB documents with a sequence counter.

    Pairs with ``invoke serve``: bring up a server in one terminal and
    point this at it in another. ``--count 0`` (the default) means run
    continuously until Ctrl-C; pass a positive integer for a bounded run.
    Each document carries a monotonic ``n`` field (1, 2, 3, ...) and an
    8192-byte payload, so total BSON size is comfortably ≥ 8 KiB.
    """
    # ``--no-sync`` skips uv's project-rebuild check: invoking the harness
    # shouldn't trigger a multi-minute CMake/WiredTiger rebuild every
    # time. Same pattern the docs / release tasks use.
    cmd = (
        "uv run --no-sync python -m bench.load_writer"
        f" --uri {shlex.quote(uri)}"
        f" --db {shlex.quote(db)}"
        f" --collection {shlex.quote(collection)}"
        f" --progress-every {int(progress_every)}"
    )
    if count > 0:
        cmd += f" --count {int(count)}"
    if drop:
        cmd += " --drop"
    c.run(cmd, pty=True)


@task(
    help={
        "duration": "Total run time in seconds (default: 180).",
        "min-interval": "Minimum seconds between SIGKILLs (default: 5).",
        "max-interval": "Maximum seconds between SIGKILLs (default: 15).",
        "port": "Server port (default: auto-pick a free port).",
        "storage-path": "WiredTiger storage dir (default: tempdir, removed at end).",
        "no-load": "Don't auto-start the load_writer (chaos only).",
        "seed": "RNG seed for kill timing (default: random).",
        "batch-size": "Documents per insert call in the writer (default: 1).",
    }
)
def chaos(
    c: Context,
    duration: float = 180.0,
    min_interval: float = 5.0,
    max_interval: float = 15.0,
    port: int = 0,
    storage_path: str = "",
    no_load: bool = False,
    seed: int = 0,
    batch_size: int = 1,
) -> None:
    """Chaos monkey: random SIGKILL/restart of SecantusDB under live load.

    Spawns SecantusDB on a free port with on-disk WiredTiger storage,
    optionally starts ``bench.load_writer`` against it, then kills and
    restarts the server at random intervals. After ``--duration``
    seconds prints a report: kills, downtime, persisted docs, gaps in
    the writer's ``n`` sequence (gaps == inserts that fell during
    outages or were not durably committed before the kill).
    """
    cmd = (
        "uv run --no-sync python -m bench.chaos"
        f" --duration {float(duration)}"
        f" --min-interval {float(min_interval)}"
        f" --max-interval {float(max_interval)}"
    )
    if port:
        cmd += f" --port {int(port)}"
    if storage_path:
        cmd += f" --storage-path {shlex.quote(storage_path)}"
    if no_load:
        cmd += " --no-load"
    if seed:
        cmd += f" --seed {int(seed)}"
    if batch_size > 1:
        cmd += f" --batch-size {int(batch_size)}"
    c.run(cmd, pty=True)


@task(
    name="compare-servers",
    help={
        "count": "Documents per workload (default: 10000).",
        "reps": "Reps to take the median over (default: 5).",
        "mongo-uri": "Existing mongod URI to compare against (default: spawn a throwaway mongod).",
        "no-mongod": "Skip the mongod comparison (Rust-vs-Python only).",
    },
)
def compare_servers(
    c: Context,
    count: int = 10000,
    reps: int = 5,
    mongo_uri: str = "",
    no_mongod: bool = False,
) -> None:
    """Compare Rust-server, Python-server, and real mongod throughput.

    Runs insert / indexed-range find / full scan / update-many / $group
    aggregate / delete-many against all three over on-disk WiredTiger (via
    pymongo) and prints per-workload medians + how many times slower than
    mongod each server is. mongod is spawned automatically when on PATH (or
    pass --mongo-uri); it's skipped with a note otherwise.

    Requires the Rust server extension, which is NOT in the default wheel —
    build it first:

        SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev
    """
    cmd = f"uv run --no-sync python -m bench.compare_servers --n {int(count)} --reps {int(reps)}"
    if mongo_uri:
        cmd += f" --mongo-uri {shlex.quote(mongo_uri)}"
    if no_mongod:
        cmd += " --no-mongod"
    c.run(cmd, pty=True)


@task(
    name="startup-times",
    help={
        "reps": "Cold-start measurements per server (default: 5).",
        "no-mongod": "Skip the mongod comparison (SecantusDB servers only).",
        "json": "Emit machine-readable JSON instead of the formatted table.",
    },
)
def startup_times(
    c: Context,
    reps: int = 5,
    no_mongod: bool = False,
    json: bool = False,
) -> None:
    """Compare cold-start startup latency of the three standalone servers.

    Spawns mongod, the Python server (``python -m secantus``), and the Rust
    server (the ``secantusdb`` binary) as standalone daemons over a fresh
    on-disk WiredTiger dir, and reports how long each takes from process
    spawn to serving its first ``ping`` (median / min / max / mean over
    ``--reps`` cold starts). mongod is skipped when not on PATH; the Rust
    server uses the standalone binary, so it does NOT need the embedded
    ``_secantus_server`` extension built.
    """
    cmd = f"uv run --no-sync python -m bench.startup_times --reps {int(reps)}"
    if no_mongod:
        cmd += " --no-mongod"
    if json:
        cmd += " --json"
    c.run(cmd, pty=True)


@task(
    help={
        "duration": "Wall-clock seconds per writer count (default: 30).",
        "batch-size": "Documents per insert call (default: 100).",
        "writers": 'Comma-separated writer counts (default: "1,2,4").',
        "shared-collection": "All writers share one collection (max contention).",
    }
)
def concurrency(
    c: Context,
    duration: float = 30.0,
    batch_size: int = 100,
    writers: str = "1,2,4",
    shared_collection: bool = False,
) -> None:
    """N-writer scaling benchmark for the storage layer.

    Phase 0 instrument from ``tasks/wt-concurrency-plan.md``. Spawns
    one server, then runs each writer count back to back; prints
    aggregate throughput + a scaling ratio per row. Today's expected
    number is 0.35x at N=2 on a single collection (Storage._lock
    contention dominates); Phase 2 has to push it above 1.5x.
    """
    cmd = (
        "uv run --no-sync python -m bench.concurrency"
        f" --duration {float(duration)}"
        f" --batch-size {int(batch_size)}"
        f" --writers {shlex.quote(writers)}"
    )
    if shared_collection:
        cmd += " --shared-collection"
    c.run(cmd, pty=True)


@task(
    name="concurrency-refresh",
    help={
        "duration": "Wall-clock seconds per writer count (default: 30).",
        "writers": 'Comma-separated writer counts (default: "1,2,4,8").',
        "runs": "Interleaved sweeps to median over (default: 3).",
        "skip-bench": "Re-render the graphs from the committed results JSON without re-measuring.",
    },
)
def concurrency_refresh(
    c: Context,
    duration: float = 30.0,
    writers: str = "1,2,4,8",
    runs: int = 3,
    skip_bench: bool = False,
) -> None:
    """Re-measure N-writer scaling and refresh the concurrency graphs.

    Runs ``bench.concurrency --server all`` (python, rust, rust-async,
    mongod — needs ``mongod`` on PATH and a built ``secantusd-rs``) with
    interleaved runs, writes the medians to
    ``bench/results/concurrency.json``, then regenerates the
    marker-delimited chart + table blocks in
    ``website/themes/secantus/templates/performance.html`` and
    ``docs/concurrency.md`` via ``bench.concurrency_chart``. The prose
    around both charts is hand-maintained — review it against the
    printed headlines. Part of the per-release website refresh (see the
    secantusdb-website skill); default settings take ~25 min.
    """
    results = "bench/results/concurrency.json"
    if not skip_bench:
        c.run(
            "uv run --no-sync python -m bench.concurrency --server all"
            f" --duration {float(duration)}"
            f" --writers {shlex.quote(writers)}"
            f" --runs {int(runs)}"
            f" --json {results}",
            pty=True,
        )
    c.run(
        f"uv run --no-sync python -m bench.concurrency_chart --results {results}",
        pty=True,
    )


@task(
    name="rw-harness",
    help={
        "workers": "Number of independent reader/writer processes (default: 4).",
        "count": "Documents each worker writes then stops (default: 1000).",
        "duration": "Run each worker N seconds instead of a fixed count (overrides --count).",
        "server": "Server hosting: daemon (default) | embedded | external.",
        "uri": "Server URI when --server external (default: mongodb://127.0.0.1:27018/).",
        "payload-bytes": "Random payload size per document (default: 256).",
        "sync-on-commit": "Start the server with --sync-on-commit (fsync every commit).",
    },
)
def rw_harness(
    c: Context,
    workers: int = 4,
    count: int = 1000,
    duration: float = 0.0,
    server: str = "daemon",
    uri: str = "mongodb://127.0.0.1:27018/",
    payload_bytes: int = 256,
    sync_on_commit: bool = False,
) -> None:
    """Concurrent read/write validation harness.

    Spawns ``--workers`` independent processes that simultaneously read
    and write a shared collection with the highest write/read safety
    (w:majority, j:true, readConcern:majority, retryWrites/Reads). Every
    read is checksum-validated in flight; a final paginated sweep
    re-verifies every document and reconciles per-worker counts. The
    server can be hosted as a daemon subprocess (default), embedded
    in-process, or pointed at an external URI for differential testing.
    """
    cmd = (
        "uv run --no-sync python -m bench.rw_harness"
        f" --workers {int(workers)}"
        f" --server {shlex.quote(server)}"
        f" --payload-bytes {int(payload_bytes)}"
    )
    if duration > 0:
        cmd += f" --duration {float(duration)}"
    else:
        cmd += f" --count {int(count)}"
    if server == "external":
        cmd += f" --uri {shlex.quote(uri)}"
    if sync_on_commit:
        cmd += " --sync-on-commit"
    c.run(cmd, pty=True)


# The harness is Rust (crates/secantus-bench): a `do-cluster` orchestrator and a
# `do-client` load agent. `cargo run` keeps it building from source so a local
# edit is picked up, and --release matters — a debug-build load agent would
# measure the agent, not the server.
_DO_CLUSTER = (
    "cargo run --quiet --release --manifest-path crates/Cargo.toml "
    "-p secantus-bench --bin do-cluster --"
)


@task(
    name="do-bench",
    help={
        "duration": "Timed seconds of load (default: 120).",
        "workers": "Load processes per client droplet (default: 16).",
        "op-mix": "Weighted op mix, e.g. 'insert=100' or 'insert=70,find=20,update=10'.",
        "repeat": "Measurement passes (default 1); >1 interleaves engines and reports medians.",
        "payload": "Document payload: repeat (default, compressible) | random (incompressible).",
        "doc-bytes": "Payload bytes per document (default: 8192).",
        "batch-size": "Documents per insert call (default: 1).",
        "region": "DigitalOcean region (default: lon1).",
        "server-size": "Server droplet size (default: c-4, dedicated CPU).",
        "client-size": "Client droplet size (default: c-2).",
        "build": "Server binary: 'release' (published tarball) or 'source' (build on the droplet).",
        "engine": "Which databases to measure: both (default) | secantus | mongod.",
        "mongod-version": "MongoDB major version to install for the comparison (default: 8.0).",
        "suspend-mode": "After the run: destroy (default) | snapshot | power-off.",
        "no-suspend": "Leave the droplets running afterwards.",
    },
)
def do_bench(
    c: Context,
    duration: float = 120.0,
    workers: int = 16,
    op_mix: str = "insert=70,find=20,update=10",
    repeat: int = 1,
    payload: str = "repeat",
    doc_bytes: int = 8192,
    batch_size: int = 1,
    region: str = "lon1",
    server_size: str = "c-4",
    client_size: str = "c-2",
    build: str = "release",
    engine: str = "both",
    mongod_version: str = "8.0",
    suspend_mode: str = "destroy",
    no_suspend: bool = False,
) -> None:
    """Full three-droplet DigitalOcean benchmark: up -> deploy -> run -> suspend.

    Provisions one server droplet and two client droplets driving load at it
    across a private VPC, so the load generator is not competing with the
    database for the same cores and the network is a real NIC rather than
    loopback. By default it measures **SecantusDB and a real MongoDB
    back-to-back on the same droplets** and prints a side-by-side comparison;
    ``--engine secantus`` or ``--engine mongod`` runs just one. Requires
    ``DIGITALOCEAN_TOKEN``.

    Costs real money for as long as the droplets exist, so the run destroys
    them afterwards by default — a *powered-off* DigitalOcean droplet still
    bills at full price. Use ``--suspend-mode snapshot`` to keep the
    installed software as a cheap image and skip the next redeploy, or
    ``--no-suspend`` to leave the cluster up. ``invoke do-status`` prints the
    live rate for whatever is currently allocated.
    """
    cmd = (
        f"{_DO_CLUSTER} all"
        f" --duration {float(duration)}"
        f" --workers {int(workers)}"
        f" --op-mix {shlex.quote(op_mix)}"
        f" --repeat {int(repeat)}"
        f" --payload {shlex.quote(payload)}"
        f" --doc-bytes {int(doc_bytes)}"
        f" --batch-size {int(batch_size)}"
        f" --region {shlex.quote(region)}"
        f" --server-size {shlex.quote(server_size)}"
        f" --client-size {shlex.quote(client_size)}"
        f" --server-build {shlex.quote(build)}"
        f" --engine {shlex.quote(engine)}"
        f" --mongod-version {shlex.quote(mongod_version)}"
        f" --mode {shlex.quote(suspend_mode)}"
    )
    if no_suspend:
        cmd += " --no-suspend"
    c.run(cmd, pty=True)


@task(
    name="release-benchmark",
    help={
        "duration": "Seconds per engine per pass (default: 90).",
        "workers": "Load processes per client droplet (default: 16).",
        "repeat": "Interleaved measurement passes (default: 3).",
        "keep": "Leave the droplets running afterwards (default: destroy them).",
    },
)
def release_benchmark(
    c: Context,
    duration: float = 90.0,
    workers: int = 16,
    repeat: int = 3,
    keep: bool = False,
) -> None:
    """Re-measure SecantusDB against MongoDB for a release, on real hardware.

    `docs/benchmark.md` publishes a head-to-head throughput and latency
    comparison against a real ``mongod``. It is prose with numbers in it, so it
    goes stale silently: nothing in the test suite fails when the engine gets
    faster, and a release that improves performance ships a page that
    understates it. (Exactly that happened when lz4 replaced zlib as the block
    compressor — the published figures were measured the day before, with the
    old compressor.)

    This task provisions the three droplets, deploys both engines, runs the
    comparison with **release settings** — incompressible payloads and three
    interleaved passes, so the medians are defensible — prints the numbers, and
    destroys the cluster. Requires ``DIGITALOCEAN_TOKEN``; costs roughly $0.25
    and takes about 45 minutes, most of it deployment.

    ``--payload random`` is not optional here. Both engines compress, so the
    default repeated-character payload measures the compressor rather than the
    engine, and it flatters whichever side compresses harder.

    **Cut the Rust binary release first.** This deploys the newest published
    ``secantusdb-v*`` release, so running it before that tag exists measures the
    *previous* build — which is exactly the staleness the task is meant to
    prevent. Pass ``--server-build source`` via ``do-cluster`` instead if you
    need to measure an unreleased ref.
    """
    cmd = (
        f"{_DO_CLUSTER} all"
        f" --duration {float(duration)}"
        f" --workers {int(workers)}"
        f" --repeat {int(repeat)}"
        " --payload random"
        " --engine both"
        " --deploy auto"
    )
    cmd += " --no-suspend" if keep else " --mode destroy"
    print(
        "Release benchmark: 3 interleaved passes per engine on incompressible\n"
        "documents. Copy the comparison table into docs/benchmark.md's\n"
        '"Over a real network, against a real MongoDB" section when it finishes.\n'
    )
    c.run(cmd, pty=True)


@task(
    name="do-perf",
    help={
        "count": "Documents per latency workload (default: 10000).",
        "reps": "Reps to median over per latency workload (default: 5).",
        "duration": "Seconds per writer count in the scaling sweep (default: 30).",
        "writers": 'Writer counts for the scaling sweep (default: "1,2,4,8").',
        "runs": "Interleaved sweeps to median over (default: 3).",
        "keep": "Leave the droplet running instead of destroying it.",
        "git-ref": "Pushed git ref to build and measure (default: HEAD).",
        "size": "Server droplet plan (default: s-8vcpu-16gb — see the docstring).",
    },
)
def do_perf(
    c: Context,
    count: int = 10000,
    reps: int = 5,
    duration: float = 30.0,
    writers: str = "1,2,4,8",
    runs: int = 3,
    keep: bool = False,
    git_ref: str = "",
    size: str = "s-8vcpu-16gb",
) -> None:
    """Measure per-operation latency and writer scaling on a DigitalOcean droplet.

    The droplet counterpart of ``compare-servers`` + ``concurrency-refresh``.
    Those run on whatever machine you happen to be sitting at, and that is
    where the published numbers have gone wrong: a background build or an OS
    indexer moves every column at once and nothing in the output says so. One
    such run made *mongod itself* 2.5x slower than its own baseline, which
    would have published a fabricated regression.

    A droplet is dedicated and idle, and because ``mongod`` is measured in the
    same run it is the control that proves it: if mongod's numbers drift from
    the previous ``bench/results/latency.json``, the machine moved, not the
    engine.

    Only the server droplet is used -- both harnesses spawn all three engines
    and talk to them over loopback, so a client droplet would add nothing but
    a NIC.

    **The default plan is s-8vcpu-16gb, not the cluster default c-4, because
    the scaling sweep needs more cores than it has writers.** At eight writers
    the harness runs eight writer processes *plus* the server; on four vCPUs
    that measures core starvation rather than write scaling. Measured directly:
    mongod -- unchanged code, the control -- scaled 4.19x at eight writers on a
    12-core machine and only 1.78x on a c-4 droplet. Every engine was
    compressed the same way, so the whole sweep was a CPU-count artefact.

    **Keep vCPUs >= the largest writer count.** An earlier note here demanded
    2x; measurement showed that is too conservative. mongod -- the control --
    scales 4.96x at eight writers on this 8-vCPU plan, against 4.65x on a
    12-core workstation and only 1.78x on a 4-vCPU c-4. So 1:1 is fine and 2:1
    oversubscription is what breaks; `s-8vcpu-16gb` measures an eight-writer
    sweep honestly.

    Absolute throughput is much lower than on a fast workstation (the cores
    are slower), but the *scaling ratio* -- what the page reports -- holds.
    Always check mongod against its own previous run before believing a sweep.

    Costs roughly $0.60 and takes about an hour, most of it building WiredTiger
    and the Rust server from source.

    Writes ``bench/results/latency.json`` and ``bench/results/concurrency.json``,
    then regenerate the published charts with ``bench.latency_chart`` and
    ``bench.concurrency_chart``.
    """
    cmd = (
        f"{_DO_CLUSTER} perf"
        f" --server-size {shlex.quote(size)}"
        f" --perf-n {int(count)}"
        f" --perf-reps {int(reps)}"
        f" --duration {float(duration)}"
        f" --perf-writers {shlex.quote(writers)}"
        f" --repeat {int(runs)}"
    )
    if git_ref:
        cmd += f" --server-ref {shlex.quote(git_ref)}"
    cmd += " --no-suspend" if keep else " --mode destroy"
    print(
        "Droplet perf run: per-operation latency + concurrent-writer scaling on\n"
        "dedicated hardware. mongod is measured alongside as the control.\n"
    )
    c.run(cmd, pty=True)


@task(
    name="do-up",
    help={"region": "DigitalOcean region (default: lon1).", "fresh": "Ignore existing snapshots."},
)
def do_up(c: Context, region: str = "lon1", fresh: bool = False) -> None:
    """Create (or wake) the three benchmark droplets and leave them running."""
    cmd = f"{_DO_CLUSTER} up --region {shlex.quote(region)}"
    if fresh:
        cmd += " --fresh"
    c.run(cmd, pty=True)


@task(
    name="do-deploy",
    help={
        "build": "'release' (published tarball, default) or 'source' (build on the droplet).",
        "version": "Release tag for --build release (default: latest secantusdb-v*).",
        "ref": "Git ref for --build source (default: HEAD, which must already be pushed).",
    },
)
def do_deploy(
    c: Context,
    build: str = "release",
    version: str = "latest",
    ref: str = "",
    engine: str = "both",
) -> None:
    """Install the database(s) and the client load agents on the droplets.

    ``--engine both`` (the default) also installs MongoDB Community on the
    server droplet so the comparison run has something to compare against.
    """
    cmd = (
        f"{_DO_CLUSTER} deploy"
        f" --server-build {shlex.quote(build)} --server-version {shlex.quote(version)}"
        f" --engine {shlex.quote(engine)}"
    )
    if ref:
        cmd += f" --server-ref {shlex.quote(ref)}"
    c.run(cmd, pty=True)


@task(
    name="do-run",
    help={
        "duration": "Timed seconds of load (default: 120).",
        "workers": "Load processes per client droplet (default: 16).",
        "op-mix": "Weighted op mix (default: insert=70,find=20,update=10).",
        "engine": "Which databases to measure: both (default) | secantus | mongod.",
        "repeat": "Measurement passes (default 1); >1 interleaves engines and reports medians.",
        "sync-on-commit": "Start the server with --sync-on-commit (fsync every commit).",
    },
)
def do_run(
    c: Context,
    duration: float = 120.0,
    workers: int = 16,
    op_mix: str = "insert=70,find=20,update=10",
    engine: str = "both",
    repeat: int = 1,
    sync_on_commit: bool = False,
) -> None:
    """Run the timed benchmark against already-deployed droplets.

    With the default ``--engine both`` this measures SecantusDB and MongoDB
    back-to-back on the same droplets and prints the comparison.
    """
    cmd = (
        f"{_DO_CLUSTER} run"
        f" --duration {float(duration)} --workers {int(workers)}"
        f" --op-mix {shlex.quote(op_mix)}"
        f" --engine {shlex.quote(engine)}"
        f" --repeat {int(repeat)}"
    )
    if sync_on_commit:
        cmd += " --sync-on-commit"
    c.run(cmd, pty=True)


@task(
    name="do-suspend",
    help={"mode": "destroy (default) | snapshot | power-off — see the module docstring."},
)
def do_suspend(c: Context, mode: str = "destroy") -> None:
    """Park the benchmark droplets until the next test.

    ``destroy`` (default) keeps nothing and bills nothing; ``snapshot``
    destroys the droplets while keeping the installed software as a cheap
    image; ``power-off`` resumes in seconds but keeps billing at full price.
    """
    c.run(
        f"{_DO_CLUSTER} suspend --mode {shlex.quote(mode)}",
        pty=True,
    )


@task(name="do-status")
def do_status(c: Context) -> None:
    """Show which benchmark droplets exist, their state, and the live hourly cost."""
    c.run(f"{_DO_CLUSTER} status", pty=True)


@task(
    help={
        "uri": "MongoDB URI to administer.",
        "port": "Local HTTP port (0 = pick a free one).",
        "no_window": "Run headless (no pywebview window). Useful for CI.",
        "token": "Override the auth token. Default: ~/.secantus/admin-token.",
    }
)
def admin(
    c: Context,
    uri: str = "mongodb://127.0.0.1:27017",
    port: int = 0,
    no_window: bool = False,
    token: str = "",
) -> None:
    """Launch the SecantusDB admin web UI.

    Uses ``--extra admin`` so uv pulls in fastapi / uvicorn / pywebview
    on first run; the base wheel deliberately doesn't ship them so an
    embedded ``SecantusDBServer`` user isn't paying for the GUI stack.
    """
    cmd = [
        "uv",
        "run",
        "--extra",
        "admin",
        "secantus-admin",
        "--uri",
        uri,
        "--port",
        str(port),
    ]
    if no_window:
        cmd.append("--no-window")
    if token:
        cmd.extend(["--token", token])
    c.run(" ".join(cmd), pty=True)


@task(
    name="admin-screenshots",
    help={
        "only": "Capture just this page slug (comma-separated for several).",
        "scale": "Device scale factor: 2 for retina PNGs, 1 for half the bytes.",
        "headed": "Show the browser while it drives the UI.",
        "list_pages": "List the page slugs and exit.",
    },
)
def admin_screenshots(
    c: Context,
    only: str = "",
    scale: int = 2,
    headed: bool = False,
    list_pages: bool = False,
) -> None:
    """Regenerate the admin-UI screenshots in docs/_static/screenshots/.

    Boots a throwaway SecantusDB with a fictional dataset, drives every
    admin page with Playwright, and writes the PNGs the Sphinx docs, the
    README and the marketing site all reference.

    **Run this on every release** — the shots are the only part of the
    docs that goes stale invisibly when the UI changes. The
    ``secantusdb-release`` skill lists it as a pre-flight step, and
    ``tests/test_docs_screenshots.py`` fails if a documented page has no
    image (it can't tell a *stale* image from a fresh one — that's what
    the release step is for).

    Needs the browser once: ``uv run playwright install chromium``.
    """
    cmd = [
        "uv",
        "run",
        "--extra",
        "admin",
        "--extra",
        "screenshots",
        "python",
        "scripts/admin_screenshots.py",
    ]
    if list_pages:
        cmd.append("--list")
    for slug in (s.strip() for s in only.split(",")):
        if slug:
            cmd.extend(["--only", slug])
    cmd.extend(["--scale", str(scale)])
    if headed:
        cmd.append("--headed")
    c.run(" ".join(cmd), pty=True)


@task(
    help={
        "port": "Local HTTP port (0 = pick a free one).",
        "host": "Bind host (default 127.0.0.1).",
        "no_window": "Run headless (no pywebview window). Useful for CI.",
        "token": "Override the auth token. Default: ~/.secantus/opsboard-token.",
        "config": "Config file to read/save (default ~/.secantus/opsboard.json).",
        "save": "Persist the resolved (non-secret) config to the config file, then run.",
    }
)
def opsboard(
    c: Context,
    port: int = 0,
    host: str = "",
    no_window: bool = False,
    token: str = "",
    config: str = "",
    save: bool = False,
) -> None:
    """Launch the SecantusDB Ops Board web UI.

    Drives the build/test/release cycle for all three servers. Uses
    ``--extra opsboard`` so uv pulls in fastapi / uvicorn / pywebview on
    first run; the base wheel deliberately doesn't ship them.

    Settings resolve CLI flag > env var > saved config > default; ``--config``
    picks the config file and ``--save`` persists the resolved (non-secret)
    config to it. For the full flag set, call ``secantus-opsboard`` directly.
    """
    cmd = ["uv", "run", "--extra", "opsboard", "secantus-opsboard", "--port", str(port)]
    if host:
        cmd.extend(["--host", host])
    if no_window:
        cmd.append("--no-window")
    if token:
        cmd.extend(["--token", token])
    if config:
        cmd.extend(["--config", config])
    if save:
        cmd.append("--save")
    c.run(" ".join(cmd), pty=True)


def _run_gauge(
    c: Context,
    *,
    module: str,
    raw: str,
    report: str,
    server: str | None = None,
    hint: str = "",
) -> None:
    """Run a gauge runner, and refuse to report unless it produced fresh results.

    Every ``<driver>_validation.runner`` writes exactly one raw artifact under
    ``.validation/`` — JUnit XML, a ``.trx``, newline-JSON, whatever its test
    tool emits — which its ``generate_report`` then renders into
    ``docs/validation-report-*.md``. Runners are invoked with ``warn=True``
    because a gauge whose tests FAIL must still produce a report; that is the
    deliverable. But the same tolerance used to let a runner that never ran at
    all fall through to ``generate_report``, which happily re-rendered the
    PREVIOUS run's artifact under today's date. A run that did not happen came
    out looking like a run that passed — the one thing a conformance report must
    never do. (Seen for real on the C++ gauge, which refuses to start when
    something already holds port 27017.)

    So: clear the artifact up front, then require it back. Several runners clear
    it themselves, but only *after* their pre-flight checks, and the pre-flight
    bail is exactly the case that leaves a stale file.

    **Keyed on the artifact, not the exit code**, deliberately. Test tools write
    their results file whether the tests pass or fail, and the runners return
    non-zero for both "could not run" (2) and, in some gauges, "tests failed" —
    so an exit-code guard would suppress reports for legitimate failing runs.
    A missing or empty artifact means the gauge never ran; a present one means
    it did, and its numbers — good or bad — are this invocation's.

    ``hint`` is appended to the refusal message: the gauge-specific likely cause
    (a missing toolchain, an occupied port), which is what the reader needs.
    """
    artifact = pathlib.Path(raw)
    artifact.parent.mkdir(parents=True, exist_ok=True)
    if artifact.is_dir():
        shutil.rmtree(artifact, ignore_errors=True)
    else:
        artifact.unlink(missing_ok=True)

    # The SQL-server gauges (psycopg, slt) have no python/rust server split, so
    # they pass server=None and the selector env var is simply not set.
    selector = f"SECANTUS_GAUGE_SERVER={server} " if server is not None else ""
    c.run(
        f"{selector}PYTHONPATH=. uv run --no-sync python -m {module}",
        pty=True,
        warn=True,  # a failing gauge still owes us a report
    )

    if artifact.is_dir():
        produced = any(f.is_file() and f.stat().st_size > 0 for f in artifact.rglob("*"))
    else:
        produced = artifact.is_file() and artifact.stat().st_size > 0
    if not produced:
        raise SystemExit(
            f"{module} produced no results ({artifact} missing or empty); not "
            f"regenerating {report}, which would otherwise restamp the previous "
            f"run's numbers as if they were current. See the runner output above."
            + (f" {hint}" if hint else "")
        )


def _gauge_parallel_flags(jobs: int) -> tuple[str, str]:
    """Return ``(env_prefix, pytest_flags)`` for a pymongo-gauge run of *jobs*.

    ``jobs=1`` (the default, and what the published number is measured with)
    keeps the historical shape: one xdist worker, one controller-owned
    server. ``jobs>1`` gives every worker its own embedded SecantusDB
    (``SECANTUS_GAUGE_PER_WORKER=1``) and distributes whole FILES
    (``--dist loadfile``) so upstream's within-file ordering survives — see
    pymongo_validation/plugin.py for why that combination is safe.

    Keep ``--jobs`` at 4 or below: the change-stream ``awaitData`` tests are
    wall-clock timing-sensitive and start flaking under CPU contention (the
    same ceiling ``validate-all --jobs`` documents).

    One trap worth knowing if you ever hand-roll a gauge invocation: the test
    paths must stay RELATIVE to the rootdir. A path outside it collapses the
    file component of every nodeid to ``""``, ``--dist loadfile`` then sees a
    single group, and the whole suite lands on one worker — a run that looks
    parallel, reports the right numbers, and is no faster at all.
    """
    if jobs < 1:
        raise SystemExit(f"--jobs must be >= 1, got {jobs}")
    if jobs == 1:
        return "", "-n1"
    return "SECANTUS_GAUGE_PER_WORKER=1 ", f"-n{jobs} --dist loadfile"


@task(
    help={
        "server": (
            "Which SecantusDB server the gauge runs against: 'python' (the "
            "pure-Python SecantusDBServer; the headline gauge, default) or "
            "'rust' (the Rust server via the _secantus_server embedded "
            "handle; the R8 conformance gate)."
        ),
        "jobs": (
            "Parallel xdist workers, each with its own embedded server "
            "(default 1 = serial, which is what the published number is "
            "measured with). 4 is the practical ceiling."
        ),
    }
)
def validate(c: Context, server: str = "python", jobs: int = 1) -> None:
    """Run pymongo's vendored test suite against an embedded SecantusDB.

    Generates docs/validation-report.md with a per-category pass / fail /
    skip / pass-rate breakdown — the "MongoDB compatibility" gauge.

    ``--server rust`` runs the same unmodified suite against the Rust
    server instead and writes docs/validation-report-rust-server.md (the
    R8 gate from tasks/rust-server-plan.md). It needs the WT-linking
    ``_secantus_server`` extension importable in the project venv — build
    it into the editable install with::

        SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON \\
            uv sync --extra dev --reinstall-package SecantusDB

    ``--jobs N`` (N > 1) runs the same 1707 tests on N xdist workers, each
    with its OWN embedded server and WT store, distributing whole files.
    Nothing is deselected — coverage is identical — but wall time drops to
    roughly the slowest single file. Use it for the inner loop; leave the
    default serial run for the number that gets published, so the report
    stays comparable release-to-release.
    """
    import pathlib

    from pymongo_validation.include_paths import DESELECT_TESTS, INCLUDE

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")

    if not pathlib.Path("vendor/pymongo-tests/test").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    paths = " ".join(INCLUDE)
    deselect = " ".join(f"--deselect={t}" for t in DESELECT_TESTS)
    suffix = "" if server == "python" else "-rust-server"
    raw_json = f".validation/raw{suffix}.json"
    report = f"docs/validation-report{suffix}.md"
    parallel_env, parallel_flags = _gauge_parallel_flags(jobs)
    # `-p no:cacheprovider`: don't pollute pymongo's tree with .pytest_cache.
    # `-n1 -o addopts=`: pymongo's tests aren't parallel-safe against a SHARED
    #   server (they collide on database / collection names), so the default is
    #   exactly ONE xdist worker — serial semantics, but a pytest-timeout
    #   process kill on a hung test only takes out the worker (xdist records
    #   the crash, restarts the worker, and the json report survives). A bare
    #   no-xdist run would lose the whole report to the first hang.
    #   `--max-worker-restart=200`: don't let repeated hangs end the run.
    #   `--jobs N` swaps this for `-nN --dist loadfile` + a per-worker embedded
    #   server, which removes the sharing rather than the tests — same 1707
    #   tests, ~3x faster. See `_gauge_parallel_flags` and the plugin docstring.
    # `-o timeout=120`: tighter than the project-wide 600s — a gauge test
    #   that blocks >2 min against SecantusDB is a conformance failure worth
    #   recording, and at 600s a handful of hangs would add hours.
    # `-p no:randomly`: pytest-randomly is a project dev dependency and is
    #   active by default, so it SHUFFLES these vendored suites on every run
    #   with a fresh unrecorded seed. Upstream driver suites assume their own
    #   ordering (shared fixtures, collections created by one test and reused
    #   by the next), so shuffling manufactures failures that say nothing about
    #   SecantusDB — measured on identical code, the async gauge produced 6,
    #   9 and 16 failures on three orderings (the reordered runs adding
    #   `CollectionInvalid: collection coll already exists` pileups and an
    #   xdist worker crash in the bulk-insert tests). A conformance number has
    #   to be reproducible and comparable release-to-release, so run these
    #   suites in the order upstream wrote them.
    # `-p pymongo_validation.plugin`: load our embedded-server bootstrap (the
    #   CONTROLLER starts the server pre-conftest; workers inherit the env,
    #   or start their own server when --jobs > 1).
    # `--continue-on-collection-errors`: a collection failure in one file
    #   shouldn't abort the whole run — we want every category measured.
    # `-c pyproject.toml` forces pytest to use OUR config; without it pytest
    # picks up vendor/pymongo-tests/pyproject.toml (closer to the test files)
    # which has options for plugins we don't load (pytest-asyncio etc).
    # `-o addopts= -o testpaths=`: clear the project-wide xdist + tests/ scoping
    # from our pyproject; this run uses positional paths.
    # PYTHONPATH=. so pytest can import our `pymongo_validation` plugin.
    c.run(
        f"{parallel_env}SECANTUS_GAUGE_SERVER={server} "
        "PYTHONPATH=. uv run --no-sync python -m pytest "
        "-c pyproject.toml "
        "-o addopts= -o testpaths= -o timeout=120 "
        f"{parallel_flags} --max-worker-restart=200 "
        "-p no:cacheprovider -p no:randomly -p pymongo_validation.plugin "
        "--continue-on-collection-errors "
        f"--json-report --json-report-file={raw_json} "
        f"--no-header --tb=no -q {deselect} {paths}",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m pymongo_validation.generate_report "
        f"--server {server} {raw_json} {report}",
        pty=True,
    )
    print(f"\nWrote {report}")


@task(
    name="validate-one",
    help={
        "nodeid": (
            "One or more space-separated pytest node ids under "
            "vendor/pymongo-tests/test, e.g. "
            "'vendor/pymongo-tests/test/test_cursor.py::TestCursor::test_tailable'."
        ),
        "server": "'python' (default) or 'rust' (the embedded RustServer).",
    },
)
def validate_one(c: Context, nodeid: str, server: str = "python") -> None:
    """Run one (or a few) pymongo gauge test(s) against an embedded server.

    The targeted inner-loop counterpart to ``validate``: loads the same
    embedded-server plugin so the named test(s) run against the python or rust
    SecantusDB exactly as in the full gauge, but skips the report generation and
    full-suite cost. For ``--server rust`` the embedded ``_secantus_server`` must
    reflect your latest changes — run ``invoke rust-server-build`` first.
    """
    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    ids = " ".join(shlex.quote(n) for n in nodeid.split())
    c.run(
        f"SECANTUS_GAUGE_SERVER={server} PYTHONPATH=. "
        "uv run --no-sync python -m pytest "
        "-c pyproject.toml -o addopts= -o testpaths= -o timeout=120 -n1 "
        "-p no:cacheprovider -p no:randomly -p pymongo_validation.plugin "
        f"{ids}",
        pty=True,
        env=_rust_env(),
    )


@task(name="validate-pymongo-async")
def validate_pymongo_async(c: Context, server: str = "python", jobs: int = 1) -> None:
    """Run pymongo's vendored *async* test suite against an embedded SecantusDB.

    The async sibling of ``validate``: it drives pymongo's native
    ``AsyncMongoClient`` API (the async/await wire path that replaced Motor)
    over the in-scope CRUD / cursor / change-stream / command-monitoring
    surface. Generates docs/validation-report-pymongo-async.md.

    Reuses the same embedded-server plugin (``pymongo_validation.plugin``) as
    the sync gauge — only the test paths
    (``pymongo_async_validation.include_paths``) and the report differ. The
    async tests need ``pytest-asyncio``; this task enables it on the command
    line (``-o asyncio_mode=auto``) so the unmodified vendored config isn't
    required.

    ``--server rust`` runs the same suite against the Rust server and writes
    docs/validation-report-pymongo-async-rust-server.md.

    ``--jobs N`` parallelises exactly as ``validate --jobs N`` does (whole
    files across N workers, one embedded server each); the default stays
    serial so the published number is measured the same way every release.
    """
    import pathlib

    from pymongo_async_validation.include_paths import DESELECT_TESTS, INCLUDE

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")

    if not pathlib.Path("vendor/pymongo-tests/test/asynchronous").exists():
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    paths = " ".join(INCLUDE)
    deselect = " ".join(f"--deselect={t}" for t in DESELECT_TESTS)
    suffix = "" if server == "python" else "-rust-server"
    raw_json = f".validation/pymongo-async-raw{suffix}.json"
    report = f"docs/validation-report-pymongo-async{suffix}.md"
    # Same invocation shape as `validate` (see that task for the `-c
    # pyproject.toml` / `-o addopts=` / `-n1` rationale), plus the
    # pytest-asyncio knobs the async suite needs: `-o asyncio_mode=auto`
    # (pymongo's async tests are bare `async def test_*`, no per-test
    # marker) and the session-scoped default loop pymongo's async fixtures
    # assume. The embedded-server plugin is the SAME one the sync gauge
    # loads — it sets DB_IP/DB_PORT before pymongo's conftest import, which
    # the async `AsyncClientContext` resolves from too.
    parallel_env, parallel_flags = _gauge_parallel_flags(jobs)
    c.run(
        f"{parallel_env}SECANTUS_GAUGE_SERVER={server} "
        "PYTHONPATH=. uv run --no-sync python -m pytest "
        "-c pyproject.toml "
        "-o addopts= -o testpaths= -o timeout=120 "
        "-o asyncio_mode=auto -o asyncio_default_fixture_loop_scope=session "
        f"{parallel_flags} --max-worker-restart=200 "
        "-p no:cacheprovider -p no:randomly -p pytest_asyncio -p pymongo_validation.plugin "
        "--continue-on-collection-errors "
        f"--json-report --json-report-file={raw_json} "
        f"--no-header --tb=no -q {deselect} {paths}",
        pty=True,
        warn=True,
    )
    c.run(
        "uv run --no-sync python -m pymongo_async_validation.generate_report "
        f"--server {server} {raw_json} {report}",
        pty=True,
    )
    print(f"\nWrote {report}")


@task(name="validate-go")
def validate_go(c: Context, server: str = "python") -> None:
    """Run mongo-go-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-go.md with a per-package pass /
    fail / skip / pass-rate breakdown — the Go-driver analogue of the
    pymongo gauge. Requires `go` on PATH (1.21+).

    ``--server rust`` targets the standalone ``secantusdb`` binary instead
    of ``python -m secantus`` and writes docs/validation-report-go-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    # Need both the outer submodule AND its nested `testdata/specifications`
    # submodule (driver-spec test data — without it the bson-corpus tests
    # fail on missing JSON files).
    if (
        not pathlib.Path("vendor/mongo-go-driver/go.mod").exists()
        or not pathlib.Path("vendor/mongo-go-driver/testdata/specifications/source").is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    _run_gauge(
        c,
        module="go_validation.runner",
        raw=f".validation/go-raw{suffix}.ndjson",
        report=f"docs/validation-report-go{suffix}.md",
        server=server,
        hint="A missing `go` toolchain (1.21+) is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m go_validation.generate_report "
        f".validation/go-raw{suffix}.ndjson docs/validation-report-go{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-go{suffix}.md")


@task(name="validate-node")
def validate_node(c: Context, server: str = "python") -> None:
    """Run mongo-node-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-node.md with a per-category pass /
    fail / pending / pass-rate breakdown — the Node-driver analogue of
    the pymongo and Go-driver gauges. Requires Node.js (>=20) and npm
    on PATH. First run does a one-time `npm install` (~1-2 min) inside
    vendor/node-mongodb-native/.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-node-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/node-mongodb-native/package.json").exists():
        c.run("git submodule update --init --recursive", pty=True)

    _run_gauge(
        c,
        module="node_validation.runner",
        raw=f".validation/node-raw{suffix}.json",
        report=f"docs/validation-report-node{suffix}.md",
        server=server,
        hint="Node.js >= 20 on PATH is required.",
    )
    c.run(
        "uv run --no-sync python -m node_validation.generate_report "
        f".validation/node-raw{suffix}.json docs/validation-report-node{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-node{suffix}.md")


@task(name="validate-psycopg")
def validate_psycopg(c: Context) -> None:
    """Run psycopg 3's vendored test suite against a SecantusPGServer daemon.

    The SQL-server conformance gauge (tasks/sql-gauges-plan.md G2): psycopg's
    own tests, unmodified, over a real TCP connection with PSYCOPG_TEST_DSN
    pointing at a daemon SecantusPGServer. Generates
    docs/validation-report-psycopg.md. Python server only — the Rust server
    has no SQL front end.
    """
    import pathlib

    if not pathlib.Path("vendor/psycopg/tests").exists():
        c.run("git submodule update --init vendor/psycopg", pty=True)
    _run_gauge(
        c,
        module="psycopg_validation.runner",
        raw=".validation/psycopg-raw.json",
        report="docs/validation-report-psycopg.md",
        hint="A missing `vendor/psycopg` submodule or PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m psycopg_validation.generate_report "
        ".validation/psycopg-raw.json docs/validation-report-psycopg.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-psycopg.md")


@task(name="validate-sqlalchemy")
def validate_sqlalchemy(c: Context) -> None:
    """Run SQLAlchemy's dialect-compliance suite against a SecantusPGServer daemon.

    The SQL-server ORM gauge (tasks/sql-gauges-plan.md G6): SQLAlchemy's own
    third-party-dialect compliance suite (sqlalchemy.testing.suite — nothing
    vendored, it ships in the sqlalchemy package) over the stock
    postgresql+psycopg dialect, with SecantusDB's capability declarations in
    sqlalchemy_validation/requirements.py. Generates
    docs/validation-report-sqlalchemy.md. Python server only — the Rust
    server has no SQL front end.
    """
    _run_gauge(
        c,
        module="sqlalchemy_validation.runner",
        raw=".validation/sqlalchemy-raw.json",
        report="docs/validation-report-sqlalchemy.md",
        hint="A PG-server startup failure is the usual cause (nothing is vendored).",
    )
    c.run(
        "uv run --no-sync python -m sqlalchemy_validation.generate_report "
        ".validation/sqlalchemy-raw.json docs/validation-report-sqlalchemy.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-sqlalchemy.md")


@task(name="sql-stress")
def sql_stress(c: Context) -> None:
    """Run the pgbench + psql stress/smoke against a SecantusPGServer daemon.

    The SQL-server always-on smoke (tasks/sql-gauges-plan.md G7): unmodified
    pgbench init (DDL + COPY + ALTER ADD PRIMARY KEY) and TPC-B in all three
    protocol modes plus a concurrent select-only lane, then a psql catalog
    smoke. Any error or dropped connection is a bug. Requires pgbench + psql
    on PATH. Generates docs/validation-report-sqlstress.md.
    """
    _run_gauge(
        c,
        module="sqlstress_validation.runner",
        raw=".validation/sqlstress-raw.json",
        report="docs/validation-report-sqlstress.md",
        hint="pgbench/psql missing from PATH or a PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m sqlstress_validation.generate_report "
        ".validation/sqlstress-raw.json docs/validation-report-sqlstress.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-sqlstress.md")


@task(name="validate-pgjdbc")
def validate_pgjdbc(c: Context, shard: str = "") -> None:
    """Run pgjdbc's own test suite against a SecantusPGServer daemon.

    The SQL-server JDBC gauge (tasks/sql-gauges-plan.md G5): the official
    PostgreSQL JDBC driver's suite, unmodified, targeted via pgjdbc's stock
    build.local.properties (gitignored upstream, so the submodule stays
    pristine). Requires a JDK 21 (pgjdbc's Gradle toolchain). Generates
    docs/validation-report-pgjdbc.md. Python server only.

    ``--shard K/N`` runs only the k-th round-robin slice of the class list and
    writes ``.validation/pgjdbc-raw-shard-K.json`` WITHOUT generating a report
    — the CI lane fans the suite across N parallel jobs this way, and
    ``validate-pgjdbc-report`` merges the complete shard set afterwards.
    """
    import pathlib

    if not pathlib.Path("vendor/pgjdbc/gradlew").exists():
        c.run("git submodule update --init vendor/pgjdbc", pty=True)
    if shard:
        k = shard.split("/", 1)[0]
        raw = pathlib.Path(f".validation/pgjdbc-raw-shard-{k}.json")
        raw.unlink(missing_ok=True)  # same freshness discipline as _run_gauge
        c.run(
            f"SECANTUS_PGJDBC_SHARD={shard} uv run --no-sync python -m pgjdbc_validation.runner",
            pty=True,
            warn=True,  # failing tests still produce the raw artifact — the deliverable
        )
        from invoke.exceptions import Exit

        if not raw.exists():
            raise Exit(f"pgjdbc shard {shard} produced no {raw} — the runner never ran")
        print(f"\nWrote {raw} (shard {shard}; merge with validate-pgjdbc-report)")
        # Gradle's exit is deliberately NOT propagated — same semantics as the
        # unsharded task (_run_gauge's warn=True): standing test failures are
        # the report's content, not a job failure, and a red step would skip
        # the artifact-upload steps that ship the raw to the merge job (the
        # first sharded run failed exactly that way: four red shards, zero
        # artifacts, and a merge with nothing to merge). A shard is red only
        # when it produced no raw at all (the Exit above).
        return
    _run_gauge(
        c,
        module="pgjdbc_validation.runner",
        raw=".validation/pgjdbc-raw.json",
        report="docs/validation-report-pgjdbc.md",
        hint="A missing `vendor/pgjdbc` submodule, no JDK 21, or a PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m pgjdbc_validation.generate_report "
        ".validation/pgjdbc-raw.json docs/validation-report-pgjdbc.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-pgjdbc.md")


@task(name="validate-pgjdbc-report")
def validate_pgjdbc_report(c: Context) -> None:
    """Merge a COMPLETE set of pgjdbc shard raws into the conformance report.

    Counterpart of ``validate-pgjdbc --shard K/N``: expects every
    ``.validation/pgjdbc-raw-shard-*.json`` of one run to be present (the CI
    merge job downloads them from the shard jobs' artifacts); the generator
    refuses a missing / duplicate / truncated shard rather than publishing a
    pass rate over part of the suite.

    The shard raws are CONSUMED on a successful merge — this task's
    equivalent of ``_run_gauge``'s freshness guard: a re-run without fresh
    shard artifacts fails on the missing files instead of re-rendering the
    previous run's results under today's date."""
    import glob
    import pathlib

    c.run(
        "uv run --no-sync python -m pgjdbc_validation.generate_report "
        ".validation/pgjdbc-raw-shard-*.json docs/validation-report-pgjdbc.md",
        pty=True,
    )
    for consumed in glob.glob(".validation/pgjdbc-raw-shard-*.json"):
        pathlib.Path(consumed).unlink()
    print("\nWrote docs/validation-report-pgjdbc.md (shard raws consumed)")


@task(name="validate-pgtest")
def validate_pgtest(c: Context) -> None:
    """Run CockroachDB's pgtest wire corpus against a SecantusPGServer daemon.

    The SQL-server wire-protocol gauge (tasks/sql-gauges-plan.md G3): ~54
    datadriven files of raw pgwire exchanges, driven by cockroach's own
    pkg/testutils/pgtest runner verbatim. Corpus + runner are fetched at a
    pinned commit via a sparse blob-filtered clone (cached under
    .validation/) — never vendored. Requires go + network on first run.
    Generates docs/validation-report-pgtest.md. Python server only.
    """
    _run_gauge(
        c,
        module="pgtest_validation.runner",
        raw=".validation/pgtest-raw.json",
        report="docs/validation-report-pgtest.md",
        hint="Missing `go`, no network for the pinned cockroach fetch, or a PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m pgtest_validation.generate_report "
        ".validation/pgtest-raw.json docs/validation-report-pgtest.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-pgtest.md")


@task(name="validate-pgx")
def validate_pgx(c: Context) -> None:
    """Run jackc/pgx's pgconn + pgproto3 tests against a SecantusPGServer daemon.

    The SQL-server low-level Go gauge (tasks/sql-gauges-plan.md G4): the
    strictest hand-rolled pgwire client, run unmodified from vendor/pgx via
    PGX_TEST_DATABASE. Requires the Go toolchain. Generates
    docs/validation-report-pgx.md. Python server only.
    """
    import pathlib

    if not pathlib.Path("vendor/pgx/pgconn").exists():
        c.run("git submodule update --init vendor/pgx", pty=True)
    _run_gauge(
        c,
        module="pgx_validation.runner",
        raw=".validation/pgx-raw.json",
        report="docs/validation-report-pgx.md",
        hint="A missing `vendor/pgx` submodule, missing `go`, or PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m pgx_validation.generate_report "
        ".validation/pgx-raw.json docs/validation-report-pgx.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-pgx.md")


@task(name="validate-slt")
def validate_slt(c: Context) -> None:
    """Run the sqllogictest corpus against a SecantusPGServer daemon.

    The SQL-server correctness gauge (tasks/sql-gauges-plan.md G1): the
    SQLite-originated sqllogictest corpus, vendored unmodified, executed by
    sqllogictest-rs (``cargo install sqllogictest-bin``) over real pgwire —
    one fresh daemon per file. Generates docs/validation-report-slt.md.
    Python server only — the Rust server has no SQL front end.
    """
    import pathlib

    if not pathlib.Path("vendor/sqllogictest/test").exists():
        c.run("git submodule update --init vendor/sqllogictest", pty=True)
    _run_gauge(
        c,
        module="slt_validation.runner",
        raw=".validation/slt-raw.json",
        report="docs/validation-report-slt.md",
        hint="A missing `vendor/sqllogictest` submodule or PG-server startup failure is the usual cause.",
    )
    c.run(
        "uv run --no-sync python -m slt_validation.generate_report "
        ".validation/slt-raw.json docs/validation-report-slt.md",
        pty=True,
    )
    print("\nWrote docs/validation-report-slt.md")


@task(name="validate-ruby")
def validate_ruby(c: Context, server: str = "python") -> None:
    """Run mongo-ruby-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-ruby.md with a per-category pass /
    fail / pending / pass-rate breakdown — the Ruby-driver analogue of
    the pymongo / Go / Node / Java gauges. Requires Ruby (>= 2.7) and
    bundler on PATH (e.g. `brew install ruby` on macOS, then add
    `/opt/homebrew/opt/ruby/bin` to PATH). First run does a one-time
    `bundle install` (~1-2 min) inside vendor/mongo-ruby-driver/.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-ruby-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-ruby-driver/mongo.gemspec").exists():
        c.run("git submodule update --init --recursive", pty=True)

    _run_gauge(
        c,
        module="ruby_validation.runner",
        raw=f".validation/ruby-raw{suffix}.json",
        report=f"docs/validation-report-ruby{suffix}.md",
        server=server,
        hint="Ruby >= 2.7 with `bundler` on PATH is required.",
    )
    c.run(
        "uv run --no-sync python -m ruby_validation.generate_report "
        f".validation/ruby-raw{suffix}.json docs/validation-report-ruby{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-ruby{suffix}.md")


@task(name="validate-java")
def validate_java(c: Context, server: str = "python") -> None:
    """Run mongo-java-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-java.md with a per-module pass /
    fail / skipped / pass-rate breakdown — the Java-driver analogue of
    the pymongo / Go / Node gauges. Requires a JDK (>=8) on PATH; uses
    the gradle wrapper the driver ships, so no system Gradle install
    needed. First run downloads the gradle distribution + dependencies
    (~150 MB) into ~/.gradle/.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-java-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    # The driver pulls in MongoDB driver-spec test data via a nested
    # submodule (testing/resources/specifications) — without it the
    # bson corpus / vector tests fail with `initializationError` on
    # missing JSON files. Same pattern as the go-driver gauge.
    if (
        not pathlib.Path("vendor/mongo-java-driver/gradlew").exists()
        or not pathlib.Path(
            "vendor/mongo-java-driver/testing/resources/specifications/source"
        ).is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    pathlib.Path(".validation").mkdir(exist_ok=True)
    _run_gauge(
        c,
        module="java_validation.runner",
        raw=f".validation/java-results{suffix}",
        report=f"docs/validation-report-java{suffix}.md",
        server=server,
        hint="A JDK 24+ default is the usual cause — Gradle needs 8-23, so install openjdk@17.",
    )
    c.run(
        "uv run --no-sync python -m java_validation.generate_report "
        f".validation/java-results{suffix} docs/validation-report-java{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-java{suffix}.md")


@task(name="validate-kotlin")
def validate_kotlin(c: Context, server: str = "python") -> None:
    """Run mongo-kotlin-driver's integration tests against an embedded SecantusDB.

    Generates docs/validation-report-kotlin.md — the official MongoDB Kotlin
    driver analogue of the Java gauge. The Kotlin driver ships in the
    mongo-java-driver monorepo, so this gauge reuses the same vendored
    submodule and JVM toolchain (a JDK 8-23 on PATH, plus the gradle wrapper
    the driver ships) and targets the ``:driver-kotlin-sync:integrationTest``
    task. First run downloads the gradle distribution + dependencies and
    compiles the Kotlin sources (slow); subsequent runs reuse the caches.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-kotlin-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    # Same nested-submodule requirement as the Java gauge — the unified-spec
    # runners load driver-spec test data from testing/resources/specifications.
    if (
        not pathlib.Path("vendor/mongo-java-driver/gradlew").exists()
        or not pathlib.Path(
            "vendor/mongo-java-driver/testing/resources/specifications/source"
        ).is_dir()
    ):
        c.run("git submodule update --init --recursive", pty=True)

    _run_gauge(
        c,
        module="kotlin_validation.runner",
        raw=f".validation/kotlin-results{suffix}",
        report=f"docs/validation-report-kotlin{suffix}.md",
        server=server,
        hint="A JDK 24+ default is the usual cause — Gradle needs 8-23, so install openjdk@17.",
    )
    c.run(
        "uv run --no-sync python -m kotlin_validation.generate_report "
        f".validation/kotlin-results{suffix} docs/validation-report-kotlin{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-kotlin{suffix}.md")


@task(name="validate-rust")
def validate_rust(c: Context, server: str = "python") -> None:
    """Run mongo-rust-driver's tests against an embedded SecantusDB.

    Generates docs/validation-report-rust.md with a per-module pass /
    fail / ignored / pass-rate breakdown — the Rust-driver analogue of
    the pymongo / Go / Node / Java / Ruby gauges. Requires Rust
    (>= 1.88) on PATH (``brew install rust`` on macOS; ``rustup`` on
    linux). First run does a one-time cargo build (~1-2 min) inside
    vendor/mongo-rust-driver/; subsequent runs reuse ``target/`` and
    complete in seconds for the curated include set.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-rust-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-rust-driver/Cargo.toml").exists():
        c.run("git submodule update --init --recursive", pty=True)

    _run_gauge(
        c,
        module="rust_validation.runner",
        raw=f".validation/rust-raw{suffix}.json",
        report=f"docs/validation-report-rust{suffix}.md",
        server=server,
        hint="A `cargo` toolchain is required.",
    )
    c.run(
        "uv run --no-sync python -m rust_validation.generate_report "
        f".validation/rust-raw{suffix}.json docs/validation-report-rust{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-rust{suffix}.md")


@task(name="validate-php-lib")
def validate_php_lib(c: Context, server: str = "python") -> None:
    """Run mongo-php-library's PHPUnit suite against an embedded SecantusDB.

    Generates docs/validation-report-php-lib.md with a per-category pass /
    fail / skipped / pass-rate breakdown — the high-level PHP-library
    analogue of the pymongo / Go / Node / Java / Ruby / Rust gauges.
    Requires PHP (>= 8.1) with the `mongodb` extension (>= 2.3) loaded and
    `composer` on PATH (`brew install php composer` on macOS). First run does
    a one-time `composer install` inside vendor/mongo-php-library/.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-php-lib-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-php-library/composer.json").exists():
        c.run("git submodule update --init vendor/mongo-php-library", pty=True)

    _run_gauge(
        c,
        module="php_lib_validation.runner",
        raw=f".validation/php-lib-junit{suffix}.xml",
        report=f"docs/validation-report-php-lib{suffix}.md",
        server=server,
        hint="PHP >= 8.1 with the `mongodb` extension plus `composer` are required.",
    )
    c.run(
        "uv run --no-sync python -m php_lib_validation.generate_report "
        f".validation/php-lib-junit{suffix}.xml docs/validation-report-php-lib{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-php-lib{suffix}.md")


@task(name="validate-php-ext")
def validate_php_ext(c: Context, server: str = "python") -> None:
    """Run mongo-php-driver's .phpt suite against an embedded SecantusDB.

    Generates docs/validation-report-php-ext.md with a per-category pass /
    fail / skipped / pass-rate breakdown — the low-level PHP-extension
    analogue of the other gauges, and (with the Go gauge) the strictest
    wire-protocol check. Requires PHP (>= 8.1) with the `mongodb` extension
    loaded; runs against the already-installed extension via PHP's
    `run-tests.php` (no rebuild). The submodule is pinned to the installed
    extension's version (`php --ri mongodb`) to avoid test version skew.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-php-ext-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-php-driver/tests/utils/basic.inc").exists():
        c.run("git submodule update --init vendor/mongo-php-driver", pty=True)

    _run_gauge(
        c,
        module="php_ext_validation.runner",
        raw=f".validation/php-ext-junit{suffix}.xml",
        report=f"docs/validation-report-php-ext{suffix}.md",
        server=server,
        hint="PHP >= 8.1 with the `mongodb` extension (and its `run-tests.php`) is required.",
    )
    c.run(
        "uv run --no-sync python -m php_ext_validation.generate_report "
        f".validation/php-ext-junit{suffix}.xml docs/validation-report-php-ext{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-php-ext{suffix}.md")


@task(name="validate-c")
def validate_c(c: Context, server: str = "python") -> None:
    """Run mongo-c-driver's test-libmongoc suite against an embedded SecantusDB.

    Generates docs/validation-report-c.md with a per-suite pass / fail /
    skipped / pass-rate breakdown — the low-level **C**-driver (libmongoc)
    analogue of the other gauges, and one of the strictest wire-protocol
    checks. Requires `cmake` and a C toolchain; the first run builds the
    vendored driver's `test-libmongoc` binary (~several min, cached under
    vendor/mongo-c-driver/_build), later runs reuse it. On macOS:
    `brew install cmake openssl@3`; on Debian/Ubuntu: `apt-get install -y
    cmake libssl-dev`.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-c-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-c-driver/CMakeLists.txt").exists():
        c.run("git submodule update --init vendor/mongo-c-driver", pty=True)

    _run_gauge(
        c,
        module="c_validation.runner",
        raw=f".validation/c-raw{suffix}.json",
        report=f"docs/validation-report-c{suffix}.md",
        server=server,
        hint="cmake plus a C toolchain and OpenSSL are required; the first build takes ~10 min.",
    )
    c.run(
        "uv run --no-sync python -m c_validation.generate_report "
        f".validation/c-raw{suffix}.json docs/validation-report-c{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-c{suffix}.md")


@task(name="validate-cxx")
def validate_cxx(c: Context, server: str = "python") -> None:
    """Run mongo-cxx-driver's Catch2 suite against an embedded SecantusDB.

    Generates docs/validation-report-cxx.md with a per-group pass / fail /
    skipped / pass-rate breakdown — the **C++**-driver (mongocxx) analogue of
    the other gauges. Requires `cmake`, a C++17 toolchain, and OpenSSL; the
    first run builds the vendored libmongoc (installed to a prefix, since
    mongocxx links it) and the mongocxx `test_driver` binary (~10-15 min,
    cached), later runs reuse them. On macOS: `brew install cmake openssl@3`;
    on Debian/Ubuntu: `apt-get install -y cmake libssl-dev`.

    NOTE: mongocxx's core tests hard-wire `mongodb://localhost:27017`, so this
    gauge binds its SecantusDB daemon on port 27017 and refuses to run if
    something already holds that port (it won't gauge a foreign server).

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-cxx-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path("vendor/mongo-cxx-driver/CMakeLists.txt").exists():
        c.run("git submodule update --init vendor/mongo-cxx-driver", pty=True)
    if not pathlib.Path("vendor/mongo-c-driver/CMakeLists.txt").exists():
        c.run("git submodule update --init vendor/mongo-c-driver", pty=True)

    _run_gauge(
        c,
        module="cxx_validation.runner",
        raw=f".validation/cxx-raw{suffix}.xml",
        report=f"docs/validation-report-cxx{suffix}.md",
        server=server,
        hint="Port 27017 already being in use is the usual cause (mongocxx's tests "
        "hard-wire the driver default port and can't be redirected).",
    )
    c.run(
        "uv run --no-sync python -m cxx_validation.generate_report "
        f".validation/cxx-raw{suffix}.xml docs/validation-report-cxx{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-cxx{suffix}.md")


@task(name="validate-dotnet")
def validate_dotnet(c: Context, server: str = "python") -> None:
    """Run mongo-csharp-driver's xUnit suite against an embedded SecantusDB.

    Generates docs/validation-report-dotnet.md with a per-namespace pass /
    fail / skipped / pass-rate breakdown — the **C# / .NET**-driver analogue of
    the other gauges. Requires the .NET SDK (`brew install dotnet`); the test
    project targets net10.0. The first run restores NuGet packages and builds
    (~several min), later runs reuse the build. The gauge runs the curated CRUD
    specification suite (`MongoDB.Driver.Tests.Specifications.crud`) — see
    dotnet_validation/include_paths.py.

    ``--server rust`` targets the standalone ``secantusdb`` binary and writes
    docs/validation-report-dotnet-rust-server.md.
    """
    import pathlib

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")
    suffix = "" if server == "python" else "-rust-server"

    if not pathlib.Path(
        "vendor/mongo-csharp-driver/tests/MongoDB.Driver.Tests/MongoDB.Driver.Tests.csproj"
    ).exists():
        c.run("git submodule update --init vendor/mongo-csharp-driver", pty=True)

    _run_gauge(
        c,
        module="dotnet_validation.runner",
        raw=f".validation/dotnet-raw{suffix}.trx",
        report=f"docs/validation-report-dotnet{suffix}.md",
        server=server,
        hint="The .NET SDK (and gpg, for the Encryption project's libmongocrypt check) is required.",
    )
    c.run(
        "uv run --no-sync python -m dotnet_validation.generate_report "
        f".validation/dotnet-raw{suffix}.trx docs/validation-report-dotnet{suffix}.md",
        pty=True,
    )
    print(f"\nWrote docs/validation-report-dotnet{suffix}.md")


@task(name="validate-all")
def validate_all(c: Context, server: str = "python", jobs: int = 4) -> None:
    """Run all thirteen driver gauges against the Python (default) or Rust server.

    Local equivalent of the CI ``.github/workflows/validate.yml`` matrix:
    fans out ``validate / validate-pymongo-async / validate-go / validate-node /
    validate-java / validate-kotlin / validate-ruby / validate-rust /
    validate-php-lib / validate-php-ext / validate-c / validate-cxx /
    validate-dotnet`` over a thread pool. Each gauge
    spawns its own SecantusDB daemon and reads back the kernel-assigned port the
    daemon bound (``gauge_common.spawn_daemon``), so concurrent gauges never
    collide on a port. ``validate-cxx`` is the one exception — mongocxx's tests
    hard-wire 27017 — but since every other gauge now binds an ephemeral high
    port, nothing contends with cxx's 27017 either.

    ``--server rust`` runs every gauge against the standalone ``secantusdb``
    binary (reports get the ``-rust-server`` suffix). ``--jobs N`` sets the
    parallelism (default 4); ``--jobs 1`` forces serial.

    Exit code is non-zero if any gauge failed.
    """
    import concurrent.futures
    import subprocess
    import sys
    import threading

    if server not in ("python", "rust"):
        raise SystemExit(f"--server must be 'python' or 'rust', got {server!r}")

    GAUGES = [
        ("pymongo", "validate"),
        ("pymongo-async", "validate-pymongo-async"),
        ("go", "validate-go"),
        ("node", "validate-node"),
        ("java", "validate-java"),
        ("kotlin", "validate-kotlin"),
        ("ruby", "validate-ruby"),
        ("rust", "validate-rust"),
        ("php-lib", "validate-php-lib"),
        ("php-ext", "validate-php-ext"),
        ("c", "validate-c"),
        ("cxx", "validate-cxx"),
        ("dotnet", "validate-dotnet"),
    ]

    # `java` and `kotlin` both drive `./gradlew` inside the SAME vendored
    # monorepo (`vendor/mongo-java-driver` — the Kotlin driver ships in it), so
    # running them concurrently contends on Gradle's project lock and one dies
    # with "Gradle Test Executor … failed to execute tests" +
    # "SmokeTests#initializationError". That is a HARNESS failure that reports
    # as 0 passed / 2 failed — a plausible-looking 0.0% pass rate that would go
    # straight onto the website's driver panel. Observed 2026-08-19 at
    # `--jobs 4`; the same commit measured 294 / 0 / 100.0% when kotlin ran
    # alone. Serialise the pair against each other (they still overlap freely
    # with the other eleven).
    gradle_lock = threading.Lock()
    GRADLE_GAUGES = {"java", "kotlin"}

    def _run(name_task: tuple[str, str]) -> tuple[str, int]:
        name, task_name = name_task
        # Stream stdout/stderr directly so the user gets live progress.
        # We don't capture — interleaving is the price of parallelism.
        cmd = ["uv", "run", "--no-sync", "python", "-m", "invoke", task_name, "--server", server]
        if name in GRADLE_GAUGES:
            with gradle_lock:
                return name, subprocess.run(cmd, check=False).returncode
        return name, subprocess.run(cmd, check=False).returncode

    # Parallel by default. Earlier parallel attempts flaked, but the cause
    # was an ephemeral-port TOCTOU race in the runners — each picked a free
    # port, closed the socket, then spawned a daemon on it, and two gauges
    # starting at once could grab the same just-freed port (one daemon then
    # talked to the wrong server, or died on EADDRINUSE). Ruby was worst hit
    # (its two-phase auth restart doubled the window). Fixed by binding
    # ``--port 0`` and reading the daemon's reported port back
    # (``gauge_common.spawn_daemon``): no window, so gauges parallelise
    # cleanly. Verified with a 6-way cold run (ruby 282/12 in 14s, vs the old
    # 81/213 in 117s under contention).
    max_workers = max(1, jobs)
    mode = "serially" if max_workers == 1 else f"with {max_workers}-way parallelism"
    print(
        f"validate-all: dispatching {len(GAUGES)} gauges {mode} against the {server} server\n",
        flush=True,
    )
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as pool:
        futures = {pool.submit(_run, ng): ng[0] for ng in GAUGES}
        results = {
            future.result()[0]: future.result()[1]
            for future in concurrent.futures.as_completed(futures)
        }

    print("\n=== validate-all summary ===", flush=True)
    failed = []
    for name, _ in GAUGES:
        rc = results.get(name, 1)
        status = "ok" if rc == 0 else f"FAILED (rc={rc})"
        print(f"  {name:<8} {status}", flush=True)
        if rc != 0:
            failed.append(name)
    if failed:
        print(f"\n{len(failed)} gauge(s) failed: {', '.join(failed)}", flush=True)
        sys.exit(1)


@task(name="validate-all-servers")
def validate_all_servers(c: Context, jobs: int = 4) -> None:
    """Run all thirteen driver gauges against BOTH the Python and the Rust server.

    Runs ``validate-all --server python`` then ``validate-all --server rust``
    **sequentially** — never concurrently. The two fleets can't overlap: the C++
    gauge binds 27017 on both runs, and two full gauge fleets at once
    oversubscribe the box and flake the timing-sensitive gauges. Each run writes
    its own reports (the Rust pass adds the ``-rust-server`` suffix).

    ``--jobs N`` is forwarded to each ``validate-all`` (default 4; keep it <= 4
    per CLAUDE.md). Exit code is non-zero if any gauge failed on either server.
    """
    import subprocess
    import sys

    failed: list[str] = []
    for srv in ("python", "rust"):
        print(f"\n===== validate-all against the {srv} server =====\n", flush=True)
        rc = subprocess.run(
            [
                "uv",
                "run",
                "--no-sync",
                "python",
                "-m",
                "invoke",
                "validate-all",
                "--server",
                srv,
                "--jobs",
                str(jobs),
            ],
            check=False,
        ).returncode
        if rc != 0:
            failed.append(srv)

    print("\n=== validate-all-servers summary ===", flush=True)
    for srv in ("python", "rust"):
        print(f"  {srv:<7} {'FAILED' if srv in failed else 'ok'}", flush=True)
    if failed:
        print(f"\ngauges failed on: {', '.join(failed)}", flush=True)
        sys.exit(1)
    print("\nall gauges passed on both servers.", flush=True)


@task(name="validate-summary")
def validate_summary(c: Context) -> None:
    """Generate ``docs/validation-summary.md`` from the five gauges' raw output.

    Each gauge writes its raw artifact to ``.validation/`` (``raw.json``,
    ``go-raw.ndjson``, ``node-raw.json``, ``ruby-raw.json``,
    ``java-results/``). This task reads them all and renders one table
    in ``docs/validation-summary.md`` so the five gauges can be compared
    like for like — every row counts one assertion outcome.

    Gauges that have never been run (no raw artifact) are silently
    omitted from the table. Run ``invoke validate-all`` first if you
    want a complete snapshot.
    """
    c.run("uv run --no-sync python -m validation_summary.generate", pty=True)


@task(name="validate-readme")
def validate_readme(c: Context) -> None:
    """HEAD-check every URL in the published PyPI README.

    PyPI doesn't know our git repo, so any relative URL in `README.md`
    renders as a broken link on the project page. This task fetches
    the description PyPI is actually serving, extracts every link/img
    URL, and reports each one's reachability — a thin wrapper over
    `pytest -m online tests/test_pypi_readme_links.py` so failures
    are easy to read in a terminal.

    Run it after every release. Network-dependent and depends on the
    package being published, so it's deliberately excluded from
    `invoke test` (the `online` marker filters it out by default).
    """
    c.run(
        "uv run --no-sync python -m pytest "
        "-p no:xdist -o addopts= -m online -v "
        "tests/test_pypi_readme_links.py",
        pty=True,
    )


_VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)([ab]\d+|rc\d+)?$")


@task(name="changelog-collate")
def changelog_collate(c: Context) -> None:
    """Fold ``changelog.d/*.md`` fragments into ``docs/changelog.md``.

    Each PR adds a fragment under ``changelog.d/`` instead of editing the shared
    ``docs/changelog.md`` (see ``changelog.d/README.md``). This folds every
    fragment into the ``## [Unreleased]`` section, in filename order, and deletes
    the fragment files. ``release-prepare`` runs this automatically; run it by
    hand to preview the collated changelog before a release.
    """
    from changelog.fragments import collate

    folded = collate()
    if not folded:
        print("no changelog fragments to collate")
        return
    for p in folded:
        print(f"    folded {p}")


@task
def release(c: Context, version: str) -> None:
    """Cut a release: prepare + finalize, end-to-end.

    The canonical one-shot release workflow (see `## Releases` in
    CLAUDE.md). Internally calls ``release-prepare`` (fast,
    foreground-friendly) followed by ``release-finalize`` (long
    polling). When invoked from a sub-agent, prefer running the two
    phases separately so the polling phase can use
    ``run_in_background=true`` and escape the per-Bash 10-minute cap.
    """
    release_prepare(c, version)
    release_finalize(c, version)


@task(name="release-prepare")
def release_prepare(c: Context, version: str) -> None:
    """Phase 1 of the release.

    Pre-flight → tests → perf → bump → commit → tag → push → GitHub
    Release. Fits comfortably in 5–7 min on a quiet machine. Sub-agents can run
    this in the foreground with the harness's default Bash timeout.
    Pushing the tag triggers the `Publish to PyPI` workflow
    asynchronously — wait for it via ``release-finalize``. Docs are
    self-hosted at https://secantusdb.com/docs/ and deploy with the
    post-release website publish (see the secantusdb-release skill).

    Pre-flight requirements (all enforced):
      - On `main` branch.
      - Working tree clean (vendored-submodule drift in either
        lowercase ` m vendor/...` or capital ` M vendor/...` form is
        tolerated; everything else rejects).
      - HEAD == origin/main (no unpushed commits).
      - Tag `vX.Y.Z` not already on origin.

    Pipeline:
      1. Full default test suite (`pytest` parallel, perf-excluded).
      2. Perf regression gates (serial).
      3. Bump pyproject.toml + src/secantus/__init__.py + uv.lock.
      4. Commit, annotate-tag, push commit + tag (combined push).
      5. Create a GitHub Release for `vX.Y.Z` with auto-generated
         notes (marked pre-release for `aN`/`bN`/`rcN` versions).
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    _ensure_main_branch_clean()
    _ensure_in_sync_with_origin()
    _ensure_tag_unused(version)

    print("==> [1/6] Full default test suite")
    c.run("uv run python -m pytest", pty=True)
    print("==> [2/6] Perf regression gates")
    c.run(
        "uv run python -m pytest -p no:xdist -o addopts= -m perf tests/test_perf_regression.py",
        pty=True,
    )

    print("==> [3/6] Collating changelog fragments")
    from changelog.fragments import collate

    folded = collate()
    for p in folded:
        print(f"    folded {p}")

    print(f"==> [4/6] Bumping version files to {version}")
    _bump_version_files(version)
    c.run("uv lock", pty=True)

    print(f"==> [5/6] Committing + tagging v{version}")
    c.run(
        "git add pyproject.toml src/secantus/__init__.py uv.lock docs/changelog.md changelog.d",
        pty=True,
    )
    # If the version is already at ``version`` on HEAD (e.g. because a
    # parallel-session merge bumped it), the ``git add`` stages nothing
    # and ``git commit`` would abort with "nothing to commit". Detect
    # that case and skip the commit — the tag still goes on HEAD which
    # already carries the right version.
    staged = c.run("git diff --cached --quiet", warn=True, hide=True)
    if staged.return_code == 0:
        print(f"    version already at {version} on HEAD; skipping release commit")
    else:
        c.run(f'git commit -m "Release v{version}"', pty=True)
    c.run(f'git tag -a v{version} -m "Release v{version}"', pty=True)
    # Combine the branch and tag pushes into one network round-trip.
    # The publish workflow still fires on the tag ref; nothing else
    # depends on the order of branch-then-tag.
    c.run(f"git push origin main v{version}", pty=True)

    print(f"==> [6/6] Creating GitHub Release v{version}")
    # Pre-release if the version has an `aN` / `bN` / `rcN` suffix.
    is_prerelease = bool(re.search(r"[abc]\d+$|rc\d+$", version))
    cmd = (
        f"gh release create v{version} "
        f"--title 'v{version}' "
        f"--generate-notes "
        f"--target $(git rev-parse HEAD)"
    )
    if is_prerelease:
        cmd += " --prerelease"
    c.run(cmd, pty=True)

    print(
        f"\nv{version} prepared, tag pushed, GitHub Release created.\n"
        f"Run `invoke release-finalize {version}` next to wait for the\n"
        f"publish workflow + PyPI propagation."
    )


@task(name="release-finalize")
def release_finalize(c: Context, version: str) -> None:
    """Phase 2 of the release.

    Poll publish workflow → PyPI.

    Polling can run for 15–25 min in the worst case (the publish
    workflow builds wheels for cp310-cp313 across 4 platforms).
    Sub-agents must call this with ``run_in_background=true`` on the
    Bash invocation to escape the harness's 10-min per-call cap;
    foreground in a developer's shell is fine. Docs are self-hosted
    at https://secantusdb.com/docs/ and ship via the post-release
    website publish, not here.

    Idempotent: every step short-circuits if the desired state is
    already true (publish workflow already concluded, PyPI already
    lists the version). Safe to re-run after any timeout or
    interruption.

    Pre-flight requirements:
      - Tag `vX.Y.Z` exists on origin (the prepare phase pushed it).

    Pipeline:
      7. Wait for GitHub `Publish to PyPI` workflow to succeed.
      8. Wait for PyPI to list the new version.
    """
    if not _VERSION_RE.match(version):
        raise SystemExit(f"version {version!r} doesn't match X.Y.Z[aN|bN|rcN]")
    commit = _resolve_tag_commit(version)

    print(f"==> [7/8] Waiting for GitHub `Publish to PyPI` workflow (commit {commit[:7]})")
    _wait_for_publish_workflow(commit)
    print(f"==> [8/8] Waiting for PyPI to list {version}")
    _wait_for_pypi_version(version)

    print(f"\nv{version} released; GitHub Release and PyPI up to date.")


def _resolve_tag_commit(version: str) -> str:
    """Resolve the commit SHA for ``vX.Y.Z`` on origin.

    Used by ``release-finalize`` to find the release commit when re-run
    later (after any ``main`` HEAD drift). The annotated tag's target
    is the release commit itself, regardless of what's on ``main`` now.
    """
    out = subprocess.run(
        ["git", "rev-parse", f"v{version}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        # Fall back to the remote ref so finalize works even if the
        # local tag was pruned.
        out = subprocess.run(
            ["git", "ls-remote", "origin", f"refs/tags/v{version}^{{}}"],
            capture_output=True,
            text=True,
            check=True,
        )
        line = out.stdout.strip().split("\n", 1)[0]
        if not line:
            raise SystemExit(
                f"tag v{version} not found on origin — "
                f"run `invoke release-prepare {version}` first."
            )
        return line.split()[0]
    return out.stdout.strip()


def _ensure_main_branch_clean() -> None:
    branch = subprocess.run(
        ["git", "branch", "--show-current"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if branch != "main":
        raise SystemExit(f"release must run on main; on {branch!r}")
    status = subprocess.run(
        ["git", "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    # Vendored-submodule drift comes in two flavours, both tolerated:
    #   " m vendor/foo" — modified content inside the submodule (build-time
    #     WiredTiger patching, etc.).
    #   " M vendor/foo" — submodule HEAD shifted because a parallel worktree
    #     pulled or updated the submodule SHA.
    # Neither goes into the release commit (the task only `git add`s
    # pyproject.toml + __init__.py + uv.lock), so they're safe to ignore.
    # Anything else is uncommitted work the release would either include
    # or shadow — reject it.
    bad = [
        line
        for line in status.splitlines()
        if line and not (line.startswith((" m ", " M ")) and "vendor/" in line)
    ]
    if bad:
        raise SystemExit("working tree has uncommitted changes:\n" + "\n".join(bad))


def _ensure_in_sync_with_origin() -> None:
    subprocess.run(["git", "fetch", "origin"], check=True, capture_output=True)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    origin = subprocess.run(
        ["git", "rev-parse", "origin/main"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if head != origin:
        raise SystemExit(
            f"local main ({head[:7]}) is not in sync with origin/main "
            f"({origin[:7]}) — push or pull first."
        )


def _ensure_tag_unused(version: str) -> None:
    out = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"v{version}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if out:
        raise SystemExit(f"tag v{version} already exists on origin — pick a new version.")


def _bump_version_files(version: str) -> None:
    py = pathlib.Path("pyproject.toml")
    init = pathlib.Path("src/secantus/__init__.py")
    py.write_text(
        re.sub(
            r'^version = "[^"]+"',
            f'version = "{version}"',
            py.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )
    init.write_text(
        re.sub(
            r'^__version__ = "[^"]+"',
            f'__version__ = "{version}"',
            init.read_text(),
            count=1,
            flags=re.MULTILINE,
        )
    )


def _wait_for_publish_workflow(commit: str, *, timeout_s: int = 1200) -> None:
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        out = subprocess.run(
            [
                "gh",
                "run",
                "list",
                "--workflow=publish.yml",
                f"--commit={commit}",
                "--json=status,conclusion,databaseId",
                "--limit=1",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        runs = json.loads(out or "[]")
        if not runs:
            line = "    no publish run for this commit yet; waiting"
        else:
            r = runs[0]
            conc = r.get("conclusion") or ""
            line = f"    run {r['databaseId']}: status={r['status']} conclusion={conc}"
            if r["status"] == "completed":
                if r.get("conclusion") == "success":
                    print(line)
                    return
                raise SystemExit(
                    f"publish workflow {r['databaseId']} concluded {r.get('conclusion')!r}"
                )
        if line != last:
            print(line)
            last = line
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for publish workflow")


def _wait_for_pypi_version(version: str, *, timeout_s: int = 600) -> None:
    url = "https://pypi.org/pypi/SecantusDB/json"
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.load(resp)
        except Exception as e:
            print(f"    PyPI API error: {e}; retrying")
            time.sleep(20)
            continue
        latest = data["info"]["version"]
        if version in data.get("releases", {}):
            print(f"    PyPI lists {version} (info.version={latest})")
            return
        print(f"    PyPI does not list {version} yet (info.version={latest}); waiting")
        time.sleep(20)
    raise SystemExit(f"timed out after {timeout_s}s waiting for PyPI to list {version}")
