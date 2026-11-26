### A `$group` on `$mod`, or collecting an object, is explained in SBE

`explain` of a `$group` whose `_id` takes `$mod` of a field by a number, or
whose `$push` / `$addToSet` collects an object of fields, now reports what
mongod 8.2.11 does: `explainVersion` "2", no classic `$group` stage, and the
same slot-based plan text, down to how a nested `if` is printed.

#### Fixed

- The slot-based `$group` plan builds `$mod`'s program and a `newObj`
  accumulator argument, where it used to fall back to the classic shape.
  Fixes RM-693 (tests/backlog/test_rm_found_rm435.py::test_a_group_mongod_pushes_into_sbe_is_pushed)
