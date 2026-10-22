### Rust MongoDB server: constant sub-expressions fail where mongod folds them

mongod optimizes every expression bottom-up before it reads a document, and
evaluates each constant sub-expression it can fold. So one that fails fails
the pipeline, even in a branch that will never run:
- `{$cond: [true, 1, {$divide: [1, 0]}]}`;
- an `$ifNull` or short-circuited `$and`;
- the `in` of a `$map` over `[]`;
- an unused `$let` variable.

The Rust server evaluated lazily and answered. It also checked constant
sub-expressions only in `$project`, `$addFields`, `$set` and `$expr` filters.

It now folds as mongod does (measured 8.2.11, 2026-10-22).

#### Fixed

- Constant sub-expressions are folded bottom-up, every child before its
  parent and left to right, including the children of operators mongod
  never folds itself (`$map`, `$filter`, `$reduce`). `$switch` folds as
  mongod's does: `default` first, then each branch's `case`. After a
  constant truthy case nothing more is visited, and a constant falsy case's
  `then` is skipped.
- Folding covers every stage that holds an expression: `$group` (`_id` and
  accumulator arguments), `$bucket` / `$bucketAuto`, `$setWindowFields`,
  `$redact`, `$replaceRoot` / `$replaceWith` and `$sortByCount`.
- The pipelines of `$facet` and `$unionWith` are folded under their own
  "Failed to optimize pipeline" prefix. A `$lookup`'s pipeline fails as an
  executor error, and only when there are outer documents.
- A `find` projection's computed fields are folded, failing with the bare
  message.

Fixes RM-251
(tests/backlog/test_rm_found_2026_10_21b.py::test_constant_arguments_are_checked_while_optimizing).
In the expression harness, the 7 remaining constant-folding codes are fixed,
leaving 6, all Decimal128 trigonometry (RM-245).
