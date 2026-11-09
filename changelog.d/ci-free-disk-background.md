### CI jobs no longer wait for the disk clean-up

#### Changed

- Every Linux CI job started by removing toolchains it does not use, and
  waited 1.6-2.8 minutes for the removal on the PR run's critical path
  (measured 2026-11-09). The runners start with 88 GB free of 145 GB, and
  the most a job was seen using is about 49 GB (`pg-oracle (backlog)`).
- `.github/actions/free-disk` now runs the removal in the background when at
  least 70 GB are free at the start (the new `wait-below-gb` input), so the
  job goes straight on. A smaller runner still waits for it and still warns
  under 40 GB free (RP-312).
- The RP-312 disk watchdog now records the lowest free space it sees, and
  its report step prints it, so each job's real peak disk use is measured
  rather than read off a few `df` lines.
