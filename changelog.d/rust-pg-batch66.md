### Rust PostgreSQL server: extended statistics over ANALYZE's sample, and a checkpointer of its own

A table past ANALYZE's sample has extended statistics, built from the
sample PostgreSQL 15.19 draws. The heaps' checkpoint no longer holds back
a statement's answer.

#### Fixed

- A table of more rows than ANALYZE samples (300 times
  `default_statistics_target`) has extended statistics, where it had none
  (part of RP-099).
  - **The sample** is drawn as `acquire_sample_rows` draws it. Knuth's
    Algorithm S picks the heap's blocks, and a reservoir keeps that many of
    their rows, in heap order.
  - **The estimates** are the sample's, with the table's rows estimated
    from the blocks read. Every block is read while the heap has no more
    blocks than the sample has rows, so the count is then exact.
  - **Randomness.** As in PostgreSQL, the draws are random, so a value
    that depends on which rows were sampled can differ between two
    ANALYZEs.
  - **Still missing.** Such a table's `pg_statistic` rows are not built
    yet (RP-099, open).

  Fixes part of RP-099 (tests/backlog/test_rp_batch50.py::test_scenario[a table past ANALYZE's sample has extended statistics]).
- The heaps' and indexes' checkpoint runs on a thread of its own, as
  PostgreSQL's checkpointer is a process of its own (RP-245). It ran at the
  end of a statement's message cycle, before the answer went out, so its
  durable writes delayed whichever statement came due. That included a
  cancelled one, by 0.56 to 0.72 s on CI. A clean shutdown waits for a
  running checkpoint, then writes what is left.

  Fixes RP-245 (tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_at_once,
  tests/backlog/test_rp_batch53.py::test_a_cancel_during_a_prepared_statements_parse_is_honoured).
