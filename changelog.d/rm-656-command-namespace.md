### A run-time pipeline error names the command's namespace

An error raised while a pipeline runs now names the `aggregate` command's
namespace, as mongod 8.2.11 does, wherever the failing stage runs.

#### Fixed

- After `$documents` the prefix named `<db>.` instead of
  `<db>.$cmd.aggregate`, and inside a `$lookup` / `$unionWith` with
  `from` / `coll` it named the foreign collection instead of the outer one.
  Fixes RM-656 (tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_run_time_error_names_the_commands_namespace)
