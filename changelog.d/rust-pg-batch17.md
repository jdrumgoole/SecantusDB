### Rust PostgreSQL server: join order for shared keys and longer chains, and statements that no longer slow down as the catalog grows

A chain of four tables whose join clauses share a column now comes back in
the order PostgreSQL 15.19 returns it. The shared column makes one
equivalence class, and the join search now plans it as PostgreSQL does:
the class's members in the order the clauses were written, any two
relations holding members joined on the class's derived clause, and a
merge join on the class read unsorted by a merge join above it. The same
search now covers chains of five to eight explicit JOINs, the most
PostgreSQL flattens into one search by default.

A database with many tables, types, functions and views no longer makes
every statement slower. A new connection's first statement read and decoded
the whole catalog about ten times, and an ordinary INSERT re-read the
function catalog, every view definition and every relation on each call.
With 250 tables, a connection's first statement took 620 ms and an INSERT ran
at a fourteenth of its empty-database rate in a debug build; both now stay
close to their empty-database cost. This is the likeliest cause of the psycopg
NOTIFY timing tests that failed on every Linux CI run.

#### Fixed

- `secantus-pg` (`join_order.rs`): a four-table INNER chain whose clauses
  share a column joins on its equivalence class (fixes part of RP-028,
  `tests/backlog/test_rust_postgres_gaps.py::test_four_tables_with_a_shared_key_join_on_its_class`,
  now a regression test). Measured against PostgreSQL 15.19: 135 of 135
  class shapes at 20, 300 and 3,000 rows (72 of them wrong before), the
  108 tree shapes unchanged.
- `secantus-pg`: catalog reads that scaled with the catalog on every
  connection or statement are shared or memoised: `all_table_defs` (the
  committed copy outside a transaction block), `relation_oid`, the session's
  function catalog, `may_call_user_function`, `views()`,
  `any_temp_function`, `index_defs`; `secantus-pgplan`: `session_has_temp`
  and the user type and composite lookups (RP-014, part).
- `tests/test_rust_pgserver_slice.py::test_pg_cancel_and_terminate_backend_signal_a_running_statement`
  no longer holds a cancel to a fixed one-second bound, which measured the
  CI runner: it now asserts that the signal interrupted a 30 s sleep (fixes
  RP-016).

#### Added

- The join order search runs chains of five to eight tables
  (`join_collapse_limit`): 144 of 144 tree shapes of five, six and eight
  tables match PostgreSQL 15.19 (120 of them wrong before). A class over five
  tables matches 8 of 18; the rest is RP-028's new gap test,
  `test_five_tables_read_a_classes_sorted_join_under_a_sorted_outer`.
- `secantus-pgplan` (`joins.rs`): a WHERE conjunct calling an IMMUTABLE
  built-in is applied inside its table's leaf of a general join, where an
  index can serve it (RP-019, part; `joins::tests`, corpus `join_pushdown`).
- `JoinNode::Join::equi_reversed` records the written order of each ON
  equality; `JoinRel::add_paths_on` / `cheapest_keyed_on` keep paths sorted
  on different equivalence classes apart, as PostgreSQL's pathkeys do.

#### Security

- `secantus-pgwire`'s examples read PEM files with
  `rustls_pki_types::pem::PemObject`, and its dev-dependency on the
  unmaintained `rustls-pemfile` (RUSTSEC-2025-0134) is gone. No PostgreSQL
  crate depends on it now; the MongoDB server's `secantus-server` still
  does.
