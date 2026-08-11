### The pgjdbc lane runs sharded — CI wall clock drops from ~70 to ~20 minutes

The pgjdbc conformance gauge runs ~5,500 JUnit tests over a real wire and
took the better part of an hour as a single CI job. The lane now fans out
as four parallel jobs, each running a deterministic round-robin quarter of
the class list (the vendored suite stays byte-for-byte unmodified — only
Gradle's `--tests` selection differs per shard), and a merge job combines
the shards' JUnit results into the same single conformance report.

The merge enforces the same publish discipline as the truncation guard: a
missing, duplicate, or truncated shard refuses the report outright rather
than rendering a pass rate measured over part of the suite. `only=pgjdbc`
dispatches select all four shards; a single shard is addressable as
`only=pgjdbc-1`. Locally, `invoke validate-pgjdbc` is unchanged (one full
run), with `--shard K/N` + `validate-pgjdbc-report` available for the
split flow.

#### Changed

- `.github/validate-lanes.json` gains lane `group`s; the plan job's filter
  matches groups as well as names.
- `pgjdbc_validation.runner` honours `SECANTUS_PGJDBC_SHARD=K/N`;
  `generate_report` merges a complete shard set (refusing anything less);
  shard-math and merge-guard tests in
  `tests/test_pgjdbc_gauge_truncation.py`.
