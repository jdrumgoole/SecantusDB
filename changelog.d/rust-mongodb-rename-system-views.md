### Renaming a database's `system.views` is refused as a write to it

The Rust MongoDB server refused a `renameCollection` of `<db>.system.views`
as a rename of a view (166 "cannot rename view"), and a rename onto it as
a rename onto a view (48 "a view already exists"). mongod 8.2.1 checks the
namespaces first and refuses both as writes to `system.views` (20
IllegalOperation, "error with source namespace: cannot write to
<db>.system.views", and the same for the target). The view check now runs
inside the rename handler after the namespace check, where the time-series
check already ran. A view rename sent to a database other than `admin` now
gets mongod's 13 Unauthorized, not 166.

#### Fixed

- `secantus-commands`: `renameCollection` refuses a view, or a rename onto
  one, in its own handler after its argument, database and namespace
  checks. Before, the view intercept ran ahead of the handler.

Fixes RM-482 (tests/backlog/test_rm_rename_errors.py::test_renaming_system_views_is_a_write_to_it)
