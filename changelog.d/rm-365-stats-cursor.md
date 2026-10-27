### Rust MongoDB server: reading an operation's storage statistics costs a quarter of what it did

Every operation reads WiredTiger's `statistics:session` for each session it
used, for its `Slow query` line and profile entry (RM-023). Each read opened
and closed a statistics cursor. A session now keeps its cursor and refreshes
it with a reset, which WiredTiger defines as clearing the session's
statistics and copying them afresh at the next step: 1.67 us per read
becomes 0.47 us (release build). Part of RM-365.

#### Changed

- `secantus-wt`: `Session::statistics` reuses one cursor per session
  (`crates/secantus-wt/tests/operation_stats.rs::each_read_of_a_session_counts_from_the_last`).
