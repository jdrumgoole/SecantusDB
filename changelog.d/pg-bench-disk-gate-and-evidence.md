### The PostgreSQL droplet benchmark refuses a slow disk and records why a trial was slow

Two things went wrong in the measurement behind the Rust PostgreSQL page's
table on 2026-10-13, and neither can now happen quietly.

#### Added

- `invoke do-pg-up` creates the three droplets, probes the server's disk, and
  destroys and creates them again while it completes fewer than 5,000
  `fdatasync` calls a second, four times at most. Two of three `c-16`
  droplets that day completed about 1,700, where both servers wait for the
  disk and the write rows come out near 1.0x.
- `invoke do-pg-bench` makes the same probe first and refuses a slow disk.
  `bench/do_pg_bench.py --check-disk` runs the probe alone, and
  `--min-fdatasync 0` measures anywhere.
- Every trial now stores what the server droplet did during it: the disk's
  flushes and the time each took, and the server's CPU a statement. A row
  whose trials differ by more than 20% of their median is printed under the
  table trial by trial with those figures. Three trials of one row had
  fallen by a third with nothing recorded that could say why; fifty-four
  further trials did not repeat it.
