# What a checkpoint every 60 s costs, and what it saves

`secantusd-pg` built twice on one droplet (16 vCPUs, Xeon Platinum 8280,
12,561 `fdatasync` calls a second) by `build.sh.txt`: `base` is `main`, and
`fix` adds the periodic checkpoint for a logged store. Both without LTO, so
the two differ in that one change. Forty 10 s trials each of eight clients
updating by primary key, from two client droplets, against one server
process (the driver is `../2026-10-13-slow-update-trials/drive.py.txt`).

| | base | fix |
| --- | --- | --- |
| statements a second, median of 40 | 13,815 | 13,864 |
| slowest trial | 13,248 | 11,660 |
| store after the 40 trials | 2.1 GB, 132 log files | 69 MB, 4 log files |
| time to listen after SIGKILL | 36.8 s | 1.4 s |

One trial of the forty with the fix came in 15% under the median; the next
slowest was 13,310. Thirteen checkpoints ran during those trials.

`store-size-every-30s-*.txt` is the store's size in kB and its file count
through each run: `base` climbs without stopping, `fix` moves between 20 MB
and 150 MB.
