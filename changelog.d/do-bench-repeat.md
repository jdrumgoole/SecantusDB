### Repeat a benchmark and report the median, not a single roll of the dice

The three-droplet harness could only measure each engine once per run, so
nothing in the report said whether the number was stable. `--repeat N` fixes
that.

The engines **interleave within each pass** rather than each running to
completion in turn — pass 1 measures SecantusDB then MongoDB, pass 2 does the
same, and so on. Thermal drift, a noisy neighbour, or anything else that
changes over the run therefore lands on both engines roughly equally, instead
of penalising whichever happened to go last.

Every figure is then a **median** rather than a mean: one pass disrupted by a
checkpoint stall should not drag the headline, and with a small number of
passes a mean is exactly what an outlier hijacks. Alongside it the report adds
a **spread** column — `(max - min) / median` — which is the number that says
whether the median deserves to be quoted, plus a per-pass table showing the raw
figures in the order they ran.

Three 60-second passes on a `c-4` server measured SecantusDB at a 3.4% spread
and MongoDB at 1.1%, putting the throughput ratio at 0.49x — consistent with
the 0.46x measured earlier on a different cluster at a different duration.

#### Added

- `--repeat N` on `do-cluster run` / `all`, and `--repeat` on `invoke do-bench`
  and `invoke do-run`.
- Median, spread and per-pass reporting in `comparison.md`; per-pass artifacts
  are named `<engine>-pass<N>-…` so every individual measurement is kept.
