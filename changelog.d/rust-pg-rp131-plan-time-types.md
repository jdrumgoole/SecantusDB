### Rust PostgreSQL server: the `information_schema` routine views answer in a fraction of the time

`information_schema.routines`, `parameters`, `data_type_privileges` and
`element_types` read every one of PostgreSQL's 3,244 built-in functions, and
took 0.9, 1.0, 2.1 and 14.8 seconds on an optimised build. They now take
about 0.1, 0.2, 0.5 and 0.9 seconds, with the same rows as before and as
PostgreSQL 15.19. Each change is a mechanism PostgreSQL itself uses (RP-131,
part).

#### Changed

- **A hashed SubPlan.** `(a, b, ...) IN (SELECT ...)` over an uncorrelated
  subquery was an OR over the subquery's rows of AND-ed equalities, evaluated
  in full for every outer row; `element_types` compared each row with
  11,734. When PostgreSQL's `subplan_is_hashable` holds -- equality on
  `text` / `varchar` / `name`, the integers, `oid` or `bool` (or a domain over
  one) on both sides, no user `=` operator, `citext` or nondeterministic
  collation, and the rows fitting in `work_mem` x `hash_mem_multiplier` -- it
  is now one probe a row into the two tables `buildSubPlanHash` builds, with
  `ExecHashSubPlan`'s NULL rules (`findPartialMatch`). Any other comparison is
  evaluated as the OR was, in the same order
  (`crates/secantus-pgplan/src/hashed_subplan.rs`).
- **What parse analysis works out once, once.** A planned row expression keeps
  a memo of its own nodes (`expr_memo.rs`): a cast's target type and typmod,
  a hashed SubPlan's tables, and the value of a constant cast
  (`NULL::int::cardinal_number`, `'NO'::varchar::yes_or_no`), kept from its
  first evaluation as PostgreSQL folds it while planning. A cast now asks its
  operand's type once rather than up to nine times.
- **A view's unread outputs are not computed.** PostgreSQL expands a view
  before `remove_unused_subquery_outputs` runs; here the statement-wide pass
  ran before `plan_select` expanded the views, so `count(*)` over a view
  computed every column of every row. The pass now also runs over the
  subqueries a view expands into, still planning a body alone first so a
  parse-analysis error in an unread output is raised. A STABLE built-in no
  longer keeps its output (only a volatile, set-returning, aggregate or
  window call does, from PostgreSQL 15.19's `pg_proc`), nor does a string
  literal cast to a type that reads it; a removed output keeps its name.
- **A join's rows carry only the columns something reads**
  (`build_joinrel_tlist`): the planner records every joined-row key the query,
  its WHERE and the join's own clauses read (`JoinRows::project`), and the
  materialised join, its hash and merge steps and its LATERAL items copy
  only those.

#### Fixed

- A subquery's field names are worked out once rather than once a row.
- An unread FROM-subquery output with no written name that the subquery's
  own ORDER BY / GROUP BY names (`(select a from t order by a) s`) is kept, as
  PostgreSQL keeps a target entry a sort clause references; one removed keeps
  its name rather than becoming `?column?`
  (`tests/backlog/test_rp_batch71.py::test_scenario[an index scan marks the dead versions it passes]`).
