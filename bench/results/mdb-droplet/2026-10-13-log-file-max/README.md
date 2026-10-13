# `secantusd-rs`: what the size of a log file costs

The released `secantusd-rs` 0.5.3-beta.191 binary on one droplet (16 vCPUs,
Xeon Platinum 8280, 12,223 `fdatasync` calls a second), started with
`--log-file-max` at 2GB (the default then), 512MB, 128MB and 32MB. The
load is the project's Rust agent (`do-client`, built by
`build-client.sh.txt`) on two client droplets over the private network:
half inserts and half updates of 1 KiB documents with a random payload, one
at a time, for 90 s, on a fresh store each time. Three passes, the order of
the sizes rotated each pass, at 16 writers and then at 64. `drive.py.txt`
runs it. Medians of three:

| writers | log file | operations a second | update p99 | store, no writes | store at the end | restart after SIGKILL |
| --- | --- | --- | --- | --- | --- | --- |
| 16 | 2GB | 22,701 | 1.66 ms | 2,048 MB | 3,158 MB | 20.2 s |
| 16 | 512MB | 22,379 | 1.69 ms | 512 MB | 3,142 MB | 17.8 s |
| 16 | 128MB | 22,422 | 1.69 ms | 128 MB | 2,866 MB | 17.6 s |
| 16 | 32MB | 22,204 | 1.74 ms | 32 MB | 2,707 MB | 17.3 s |
| 64 | 2GB | 16,010 | 10.69 ms | 2,048 MB | 4,975 MB | 17.6 s |
| 64 | 512MB | 17,088 | 9.92 ms | 512 MB | 2,468 MB | 15.5 s |
| 64 | 128MB | 16,102 | 10.69 ms | 128 MB | 2,176 MB | 14.1 s |
| 64 | 32MB | 16,090 | 10.69 ms | 32 MB | 2,123 MB | 14.5 s |

What it shows:

- **The rate does not depend on the size.** At 16 writers the four medians
  are within 2.2% of each other, and the three trials of one size differ
  by up to 2.8%. At 64 writers the rate fell through the run whatever the
  size: in the order they ran, the first five trials are between 17,088
  and 17,234 and the last seven between 15,031 and 16,102. No size is
  ahead in every pass.
- **A log file is allocated in full**, so a store that has taken no write
  is the size of one.
- **Under this load the checkpoint interval sets the store's size, not the
  file size.** A checkpoint runs every 60 s, so a 90 s run ends holding
  about the last 30 s of log whatever the file size. What 2GB adds is the
  unused part of its last file.
- **64 writers got less than 16 did.** The clients' CPUs were under 5%
  busy in both, so that is the server. It is not about the log file and
  was not looked into here.

The earlier figure of 13% to 19% more throughput from 2GB at 4 to 8 writers
(`tasks/rust-perf-findings.md`) came from a sweep with the clients on the
server's machine. It was not repeated here.
