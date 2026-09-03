### A slow npm registry failed CI where the test harness says it should skip

Every `_ensure_*` helper in the cross-driver smokes is documented as returning
`False` when its toolchain cannot be provisioned — offline, no registry, a cold
cache — so that the smoke needing it **skips**. They only ever checked the exit
code, while `_run` passes `timeout` straight to `subprocess.run`, which
**raises** `TimeoutExpired`. So a registry that merely responded slowly failed
the build, which is not what any of those helpers claim to do.

Four CI shards went red on `npm install` exceeding its 300s budget, across two
test files and both Linux and macOS, on a commit that touched nothing near
them. A rerun cleared three and not the fourth — it is not a coin-flip flake,
the macOS runner is simply slower than the budget.

Provisioning subprocesses now go through `_run_provision`, which reports a
timeout the same way it reports a non-zero exit: no usable toolchain right now.
This is deliberately **not** folded into `_run` — inside a test body a timeout
is a real failure and stays one. All eight call sites are inside `_ensure_*`
helpers; no test body changed.

#### Fixed

- `tests/test_cross_driver_features.py`, `tests/test_geo_cross_driver.py`:
  `_run_provision` returns `None` on timeout, and every provisioning helper
  treats that as "toolchain unavailable" and skips.
