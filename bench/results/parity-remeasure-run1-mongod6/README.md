# Parity re-measure, run 1 — Rust vs mongod 6.0.16 (2026-08-20)

The first clean write re-measure at `cfd5b352`, and the source of the "same
measurement against 6.0.16 gives 2.27× → 3.36×" line in the retracted VERDICT
section of `tasks/rust-parity-forward-plan.md`. Run 2 (`../parity-remeasure/`)
added the 8.3.4 arm; this one is kept because the 6.0.16 claim rests on it.

| writers | Rust | mongod 6.0.16 standalone | mongod 6.0.16 `replset-w1` | like-for-like gap |
|---|---|---|---|---|
| 1 | 36,542/s | 112,088/s | 82,791/s | 2.27× |
| 2 | 56,207/s | 205,501/s | 147,895/s | 2.63× |
| 4 | 75,407/s | 384,407/s | 245,679/s | 3.26× |
| 8 | 88,654/s | 497,825/s | 297,471/s | 3.36× |

## Why `artifact.json` says `"trusted": false`

**A bug in the runner's guard, not a problem with the data.** The post-run box
check sampled the load average the instant the last writer exited — and load
average is a ~1-minute decaying mean, so it was reading *this run's own* writers
winding down (7.22) rather than any outside contamination. The guard now waits for
that decay before judging, and records both readings; see `check_box(settle_s=...)`
in `bench/parity_remeasure.py`.

The flag is left as it was recorded rather than edited after the fact, because a
result artifact that gets tidied up retroactively is worth nothing.

**Corroboration:** run 2 measured the same arms under a guard that passed cleanly
and agreed to within ~2% — 37,308 vs 36,542 (Rust, 1 writer) and 112,472 vs
112,088 (mongod 6.0.16 standalone). The rep-to-rep spread inside each run is under
2% as well. Two independent runs agreeing that closely is the reason these numbers
are quotable despite the flag.

Everything here was produced by `bench/parity_remeasure.py` on a pinned detached
worktree; `artifact.json` records the SHA before and after, the mongod version, and
both box checks.
