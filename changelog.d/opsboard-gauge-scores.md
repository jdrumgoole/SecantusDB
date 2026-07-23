### Ops Board: the gauge matrix now shows how each driver scored

The Gauges page previously told you which conformance suites exist and let you
run them; it didn't tell you how any of them did. Each row now carries the
result of the last run — passed-of-ran and the pass rate, coloured green when a
gauge is clean and amber when anything failed, with the report's generation date
beneath it and the full passed/failed/errored/skipped breakdown on hover. A
gauge that has never been run here says so rather than showing a misleading zero.

The scores are read from the reports the gauges themselves generate, so they
can't drift from what was actually measured. Parsing is by column *name*, which
matters more than it sounds: the reports come in three shapes — a per-category
table ending in an Overall row, the same plus an extra Errored column (pymongo),
and a label-less single-row summary (the C++ gauge) — and a parser tuned to one
silently mis-reads the others.

#### Added

- `secantus.opsboard.reports`: validation-report parser + gauge/server filename
  mapping, handling all three report shapes.
- Per-gauge, per-server scores on the `/gauges` matrix.
