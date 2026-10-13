# Looking for the three slow update trials

In the 0.1.0-beta.13 run of 2026-10-13, eight clients updating by primary key
gave 12,418 and 12,209 statements a second and then 8,019, 8,156 and 9,000
(`../2026-10-13-0.1.0-beta.13-remote-clients/update-distinct.first-run.json`).
Nothing recorded at the time could say whether the disk or the server was
slow.

**It did not happen again.** `forty-trials.json` is forty more trials of that
row against one 0.1.0-beta.13 process on a fresh droplet (Xeon Platinum 8358,
12,000 `fdatasync` calls a second), driven by `drive.py.txt` with `iostat`
and `pidstat` running on the server: 12,745 to 13,377 statements a second,
all forty. With the seven trials of the repeat and the seven of 2026-10-12,
that is 54 trials of this release without one slow one. The cause of the three is still not known.

**What changed, so the next one explains itself.** `bench/do_pg_bench.py` now
reads the server droplet's disk and CPU counters before and after every
trial and stores what they say beside the rate. `evidence-sample-0.1.0-beta.14.json`
is a short run with them: a flush took 0.036 to 0.039 ms on both servers,
and an update cost PostgreSQL 62 microseconds of CPU and `secantusd-pg` 158.
A row whose trials differ by more than 20% is printed trial by trial with
those figures.

**What the watching found instead.** `store-size-every-30s.txt` is the
store's size in kB, its file count and a zero, every 30 s of the forty
trials. The server never checkpoints while it runs, so WiredTiger never
removes a log file: 2.0 GB and 125 log files after 12 minutes, for tables
holding a few kilobytes. Killed with SIGKILL at that point, it took 38 s to
listen again. A clean stop left one log file and 19 MB. `tasks/backlog.md`
has the entry.
