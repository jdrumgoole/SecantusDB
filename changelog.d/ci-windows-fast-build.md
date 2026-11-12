### The weekly Windows build skips PGO and LTO

The `storage-engine` job's Windows cell (weekly cron and manual dispatch)
now builds the wheel in `fast` mode, as Linux PRs do: no PGO, LTO off,
sixteen codegen units. Its full build had reached 61 minutes of a 90-minute
job, and was doubling every three weeks or so as the server grew.

#### Changed

- The Windows `storage-engine` cell builds in `fast` mode. Measured
  2026-11-11 (run 43094814179): the full build spent 48m48s on
  `_secantus_server`, ~41 min of it the crate's single codegen unit under
  thin LTO + PGO (12 min on 2026-10-22), about four times the same step on
  Linux; the three cargo builds run one after another under the Visual
  Studio generator, and the weekly cell's cargo cache does not survive the
  week. Nothing ships the full Windows extension (PyPI retired 2026-10-20;
  the Windows release binary is built without PGO). What is lost: a fault
  that only the LTO / PGO build shows on Windows appears at a release, not
  at the weekly run. macOS keeps the full build weekly.
