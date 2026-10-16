### Rust MongoDB server: `$sum`, `$avg` and `$stdDev` as mongod computes them

The numeric accumulators now follow mongod 8.2's own code (`util/summation`,
`accumulator_sum`, `accumulator_avg` and the removable window functions),
transcribed step by step and checked bit for bit against mongod 8.2.11.

#### Fixed

- **`$sum` and `$avg` over doubles use mongod's compensated summation**, in
  `$group`, in the expression forms and in `$setWindowFields`. Every int,
  long and double goes into one double-double sum, and decimals into a
  decimal total of their own, joined at the end. This server added each
  value to a plain running double, so `$sum` of 0.1, 0.2, 0.3, 1e16, 1.0,
  -1e16 was 0.0 here and is 2.0 on mongod. `{$sum: [0.1, 0.2, 0.3]}` is
  now 0.6, not 0.6000000000000001. A numeric literal (`{$sum: 2.5}`) is
  mongod's plain constant sum, widening int to long to double on overflow.

- **`$stdDevPop` / `$stdDevSamp` over a window that drops documents**
  (`documents: [-2, 0]`, any bounded `range`) are updated incrementally
  as mongod updates them, and match its last digit. Each window used to be
  recomputed from scratch. A window holding a NaN or an infinity is `null`,
  with mongod's quirk that the window forgets those values once its finite
  ones are all gone. A single value under `$stdDevPop` in such a window is
  int 0. In `$group`, a NaN now makes the deviation NaN, as on mongod; it
  used to be dropped.

- **`$sum` / `$avg` over a window that drops documents** are mongod's
  removable sum: a removed value is added negated, NaN and the infinities
  are counted apart, and the answer is narrowed back to the types still in
  the window. They matched about 80% of the time before; 0 of 9,600
  windowed answers differ now, edge values included.

Fixes RM-181 (tests/backlog/test_rm_found_2026_10_16c.py::test_std_dev_over_a_sliding_window,
tests/backlog/test_rm_found_2026_10_16d.py::test_std_dev_over_range_windows_and_non_finite_values),
RM-182 (tests/backlog/test_rm_found_2026_10_16d.py::test_sum_and_avg_accumulators_over_doubles,
tests/backlog/test_rm_found_2026_10_16d.py::test_sum_accumulator_random_doubles,
tests/backlog/test_rm_found_2026_10_16d.py::test_sum_and_avg_expressions_over_doubles),
RM-183 (tests/backlog/test_rm_found_2026_10_16d.py::test_sum_and_avg_over_removable_windows).
