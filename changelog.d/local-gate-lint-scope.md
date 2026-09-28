### `./inv rust-gate` lints the same scope as CI again

When CI's lint step was widened from `src tests` to `.`, `rust_tasks.py`'s gate
was left at the old scope — so the gate would have passed while CI failed, which
is exactly the failure its own comment says it exists to prevent. Found by the
documentation pass at session close rather than by anything automated.

#### Fixed

- `rust-gate` now runs `ruff check .` and `ruff format --check .`, matching CI.

#### Added

- `test_the_local_gate_lints_the_same_scope_as_ci` pins the two together, so
  widening one without the other fails a test instead of a push.
