# `secantusd-rs`: what a checkpoint every 60 s costs, and what it saves

The measurement of `../../pg-droplet/2026-10-13-periodic-checkpoint/`,
repeated for the Rust MongoDB server. `secantusd-rs` built twice on one
droplet (16 vCPUs, Xeon Platinum 8280, 11,813 `fdatasync` calls a second)
by `build.sh.txt`: `fix` is commit `d111914a`, and `base` is that commit
with the storage crate's `lib.rs` from the commit before it. Both without
LTO, so the two differ in that one change. Sixty 10 s trials each of eight
clients updating by `_id` (pymongo 4.15.3, four processes on each of two
client droplets, `client.py.txt`), against one server process at its
defaults, driven by `drive.py.txt`.

| | base | fix |
| --- | --- | --- |
| updates a second, median of 60 | 15,690 | 15,724 |
| slowest trial | 14,865 | 15,046 |
| store after the 60 trials | 4.1 GB, 2 log files | 2.1 GB, 1 log file |
| time to listen after SIGKILL | 81.2 s | 3.9 s |

All 8,000 documents were there after each restart.

This server's log files are 2 GB each and are allocated in full when they
are created, so the store is never smaller than one of them. `base` keeps
every file it has written: two after 14 minutes, and one more about every
six minutes of this load. `fix` holds a second file for under a minute
after the log rolls over, until the next checkpoint removes the first
(`store-size-every-30s-fix.txt`: one sample of 27 shows two files).

The clients are pymongo processes, so the rate is what eight Python clients
can ask for and is not the server's ceiling. It is the same instrument for
both builds.
