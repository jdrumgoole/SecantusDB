### A `$group`'s `$first` and `$last` count the value they hold

On the Rust MongoDB server, a classic-engine `$group`'s `$first` and `$last`
now count the value they keep, as mongod 8.2 does: a 1000-byte string adds
its 1024-byte buffer to the accumulator's own size, a decimal its buffer, an
int or a short string nothing. `explain`'s `maxAccumulatorMemoryUsageBytes`
shows the same number as mongod's, and the `$group` memory budget sees the
same total, so a `$group` holding long first or last values spills and
refuses where mongod does instead of later.

#### Fixed

- `secantus-core` `group::acc_memory`: the classic `$first` / `$last` add
  `Value::getApproximateSize` less the inline `Value` for what they hold.
  Fixes RM-409 (tests/backlog/test_rm_found_2026_11_08_explain.py::test_a_classic_first_counts_its_value);
  regression test tests/mongod_live/test_explain_memory_counters.py::test_a_first_and_a_last_count_the_value_they_hold.
