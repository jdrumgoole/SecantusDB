### The Rust MongoDB server plans a query's `$expr` as mongod does, and wraps its errors as mongod does

mongod optimises the expression inside a query's `$expr` before it plans,
and derives index-usable predicates from it. So
`{$expr: {$eq: ["$a", 1]}}` reads an index on `a`, and the documents come
back in the index's order. The Rust MongoDB server explained `$expr` as
written and always scanned the collection, so on such a query `find`
returned a different order from mongod's. All of it was measured against
mongod 8.2.11.

#### Fixed

- **`$expr` is optimised.** A constant folds: `{$expr: false}` (or `0`,
  `null`, `{$in: ["$a", []]}`) is `$alwaysFalse` and explains as `EOF`, and
  `{$expr: true}` drops out. Constants print as `{$const: v}`, `$$CURRENT.a`
  as `$a`, and a single argument as a list. A constant `true` leaves an
  `$and`, and a constant `false` leaves an `$or`.
- **`$expr` derives mongod's index predicates.** A comparison of a field to
  a constant becomes `{a: {$_internalExprEq: 1}}` (and `Lt` / `Lte` / `Gt` /
  `Gte`, either way round), an `$in` over constants becomes `{a: {$in:
  [...]}}`, and `$and` / `$or` combine them. Their bounds run to MinKey or
  MaxKey with no type bracketing; a null bound is rechecked; a multikey
  index is not used for them. The FETCH or COLLSCAN filter keeps only the
  `$expr`, printed as mongod prints it.
- **A FETCH stage rechecks its predicates in mongod's order**: by path,
  with `$or`, `$not` and `$expr` first, not in `parsedQuery`'s order. This
  was wrong for every query with two or more residual predicates on
  different paths.
- **An error inside a query's `$expr` carries mongod's wrapper.** That
  means `Executor error during find command: <ns> :: caused by ::`,
  `Executor error during count|distinct|aggregate command on namespace:
  <ns> :: caused by ::`, and `Plan executor error during
  update|delete|findAndModify :: caused by ::`. A constant sub-expression
  that fails is `Failed to optimize expression :: caused by ::` (under
  `Failed to optimize pipeline` in `aggregate`), on an empty collection
  too. `explain` of such a query now answers, and `find` on an empty
  collection no longer fails where mongod returns nothing.
- **`$abs`**: a non-number is 28765 in a query's `$expr` too, and the
  long minimum is 28680. It used to answer the double
  9.223372036854776e18.

Fixes RM-074 in part (tests/backlog/test_rm_057_084.py::test_expr_is_optimised_and_rewritten_for_the_planner,
::test_explain_of_expr_false_is_eof), RM-175 (tests/backlog/test_rm_found_2026_10_16b.py::test_a_fetch_rechecks_its_predicates_by_path),
RM-176 (::test_an_error_inside_a_querys_expr_carries_mongods_wrapper) and
RM-177 (::test_abs_of_a_non_number_and_of_the_long_minimum).
