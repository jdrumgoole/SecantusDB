### Date operators honour their `timezone`

The backlog carried this as one Rust-only gap in `$dateFromString` that "genuinely needs a timezone database — a dependency decision, not an afternoon's port". Measuring it found all three parts wrong: `chrono-tz` is **already** a dependency with the IANA database bundled (which is why `$hour` with a named zone already worked), it is `$dateFromParts` too, and the sweep turned up **three silent wrong answers on BOTH servers** that the entry never mentioned.

New standing probe `tools/probes/date_timezones.py` — 409 shapes across the whole date family, every zone kind, and both DST directions.

| | Before | After |
| --- | --- | --- |
| Python server | 142 divergent | **0** |
| Rust server | 191 divergent | **0** |

#### Fixed — silent wrong answers

- **`$dateTrunc` ignored `timezone` entirely**, bucketing on UTC boundaries. A daily rollup for `America/New_York` bucketed at 00:00Z instead of 04:00Z, quietly attributing four hours of every day to the wrong bucket. It also binned from year 1 rather than mongod's **2000-01-01** reference, and defaulted weeks to Monday where mongod uses **Sunday**.
- **`$dateDiff` ignored it too**, and computed "whole units elapsed" where mongod counts **boundary crossings**: 02:00Z→23:00Z is 1 day in New York and 0 in UTC. Both operators now share one bin-index function so they cannot drift apart.
- **`$dateAdd` / `$dateSubtract` ignored it as well.** A calendar shift moves the *local wall clock*: noon Eastern plus one day is noon Eastern — 23 real hours across a spring-forward — while `+24 hour` is 24. Every calendar shift across a DST boundary was an hour out (30 minutes for `Australia/Lord_Howe`).

Two arithmetics, both mongod's, probed across the 2026-03-08 spring-forward: **calendar** units land on a local wall-clock boundary; **sub-day** units bin by real elapsed time, so `binSize: 5` hours stays 5 real hours apart rather than re-aligning.

#### Fixed — Rust capability

- `$dateFromString` and `$dateFromParts` accept **named IANA zones**. Only the instant→wall-clock direction had been wired up; these two need the reverse, which is now `tz_instant_from_local_ms`.
- An unusable zone answers mongod's `40485` instead of deferring — which on the standalone server reported the *operator* as unsupported for what is a bad *argument*.

#### Fixed — error surface

- Zone names are **case-sensitive**. `zoneinfo` resolves through the filesystem, so `America/new_york` loaded on macOS and failed on Linux — the answer depended on the host.
- A literal `timezone` is validated at pipeline **optimization** time, which is where mongod reports it, and `$dateTrunc` / `$dateDiff` name the parameter in the message as mongod does.
