### `top` reports real timings on Windows

The `top` command measured how long each operation took with a clock whose
resolution on Windows before Python 3.11 is about 15.6 milliseconds. Anything
faster than that measured as zero elapsed time, so the reported per-namespace
timings were almost always `0` on that platform — the counts were right, the
times were not.

Timing now uses the high-resolution performance counter, which is the correct
clock for measuring an interval and is precise on every supported platform.

#### Fixed

- `top` reports non-zero operation times on Windows / Python 3.10, where the
  previous clock's granularity rounded almost every measurement to zero.
