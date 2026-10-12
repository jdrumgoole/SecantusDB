"""One client, select by primary key, under chosen CPU placements."""

import os, subprocess, sys, time, statistics, socket, tempfile, shutil
import psycopg

B = sys.argv[1]


def steal():
    f = open("/proc/stat").readline().split()
    return int(f[8]), sum(int(x) for x in f[1:9])


def trial(dsn, cpus, secs=5.0):
    if cpus is not None:
        os.sched_setaffinity(0, cpus)
    else:
        os.sched_setaffinity(0, range(os.cpu_count()))
    with psycopg.connect(dsn, autocommit=True) as c, c.cursor() as cur:
        cur.execute("select v from exp_t where k = %s", (1,))
        cur.fetchone()
        s0 = steal()
        n = 0
        end = time.monotonic() + secs
        while time.monotonic() < end:
            cur.execute("select v from exp_t where k = %s", (1,))
            cur.fetchone()
            n += 1
        s1 = steal()
    return n / secs, 100.0 * (s1[0] - s0[0]) / max(1, s1[1] - s0[1])


def prep(dsn):
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists exp_t")
        c.execute("create table exp_t (k bigint primary key, v bigint)")
        c.execute("insert into exp_t values (1, 0)")


# The guest reports 16 one-thread cores: the hypervisor hides which vCPUs
# share a physical core, so several pairs are tried and compared.
LAYOUTS = (
    [("unpinned", None, None)]
    + [(f"server cpu2, client cpu{k}", {2}, {k}) for k in (3, 4, 6, 10, 15)]
    + [("same cpu2", {2}, {2}), ("unpinned again", None, None)]
)


def pg_pin(cpus):
    pid = open("/var/run/postgresql/16-main.pid").readline().strip()
    mask = ",".join(map(str, sorted(cpus))) if cpus else f"0-{os.cpu_count() - 1}"
    subprocess.run(["taskset", "-acp", mask, pid], capture_output=True)


PG = "host=127.0.0.1 port=5432 dbname=postgres user=postgres"
prep(PG)
for name, srv, cli in LAYOUTS:
    pg_pin(srv)
    r = [trial(PG, cli) for _ in range(7)]
    rates = [x[0] for x in r]
    print(
        f"postgres   {name:26} median {statistics.median(rates):8.0f} spread {100 * (max(rates) - min(rates)) / statistics.median(rates):5.1f}%  steal {max(x[1] for x in r):.2f}%  ",
        " ".join(f"{x:.0f}" for x in rates),
        flush=True,
    )
pg_pin(None)
for name, srv, cli in LAYOUTS:
    store = tempfile.mkdtemp(prefix="exp-")
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    cmd = [B, store, f"127.0.0.1:{port}"]
    if srv:
        cmd = ["taskset", "-c", ",".join(map(str, sorted(srv)))] + cmd
    d = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    dsn = f"host=127.0.0.1 port={port} dbname=postgres user=postgres"
    prep(dsn)
    r = [trial(dsn, cli) for _ in range(7)]
    rates = [x[0] for x in r]
    print(
        f"secantusd  {name:26} median {statistics.median(rates):8.0f} spread {100 * (max(rates) - min(rates)) / statistics.median(rates):5.1f}%  steal {max(x[1] for x in r):.2f}%  ",
        " ".join(f"{x:.0f}" for x in rates),
        flush=True,
    )
    d.terminate()
    d.wait()
    shutil.rmtree(store, ignore_errors=True)
