### The stale-artifact check reaches the gauges and benchmarks, and the lint gate reaches the whole repo

Two gaps left open by the previous change, both of the same shape: a guard that
existed but only covered part of what it was meant to.

#### Changed

- **Every launcher checks provenance, not just pytest and the probes.** Six more
  sites: `gauge_common.rust_binary()` (all thirteen MongoDB gauges),
  `psycopg_validation/runner.py`, and the `bench/` harnesses behind the published
  concurrency chart, the latency-vs-mongod table and the two PostgreSQL cost
  benchmarks. These are the paths that PUBLISH a number — on 2026-09-18 the
  psycopg gauge was measured against a checkout 116 crate-commits behind, reported
  73.8% where the truth was ~99.98%, and that figure reached the live website.
- **`SECANTUS_ALLOW_STALE_ARTIFACT=1` overrides the check, loudly.** Measuring an
  old build on purpose — a bisect, a before/after against a previous release — is
  legitimate, and without a supported way to say so the person who needs it
  comments the check out, which removes it for everybody. The override still
  prints the mismatch, so a stale run cannot look like a clean one in a log.
- **CI lints `.` instead of `src tests`.** That scope left `tools/`, `bench/`, all
  nineteen gauge runners, the invoke tasks and `website/` unchecked. A named path
  list was the first fix and was wrong the same way — an enumeration grows a hole
  the moment a directory is added, and the test written against it immediately
  found 27 locations missing. ruff already skips gitignored trees and the vendored
  submodules, so `.` reaches our Python and nothing else.

#### Fixed

- **A dict key written twice in `pgtest_validation/include_paths.py`**, silently
  discarding one session's findings — Python keeps only the last. Two sessions had
  each documented the `procedure` stanza; only one was visible. Both are now
  merged into a single entry, including the detail they disagree on (the offending
  line is cited as `procedure:66` by one and `procedure:68` by the other), rather
  than one being picked. This was invisible until ruff was pointed at that
  directory.
- 44 further lint findings across the newly-covered directories: unused imports,
  import ordering, long lines, a `zip()` without `strict=`, a `try`/`except`/`pass`.

#### Added

- Twelve tests: one per launcher asserting it calls the check, two for the
  override's behaviour, and two pinning the lint gate's scope.
