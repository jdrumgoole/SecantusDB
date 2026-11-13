### A refused `listIndexes` or `collStats` of a view is profiled

mongod takes a `listIndexes`'s or a `collStats`'s read lock and namespace
before it finds that the name is a view, and refuses it there
(CommandNotSupportedOnView, 166): its profile entry names the view, with
`ok: 0` and the refusal, and its line and entry count the global lock it
took. This server refused both before profiling them, so it wrote no entry,
and its line had `locks: {}` and no `storage` section.

#### Fixed

- `secantus-commands` (views, profiler): a `listIndexes` or `collStats` of a
  view takes the read's locks before it is refused, and is profiled under
  the view, as mongod 8.2.11's is: one global IS on its line, two in its
  entry, and a `storage` section on each. A refused `findAndModify`,
  `createIndexes`, `dropIndexes` or `validate` of a view is still profiled
  nowhere, as mongod's is.

Fixes RM-488 (tests/backlog/test_rm_view_namespaces.py::test_a_refused_list_indexes_of_a_view_is_profiled,
tests/backlog/test_rm_view_namespaces.py::test_a_refused_list_indexes_of_a_view_counts_its_lock).
Files RM-590, RM-591, RM-592 and RM-593, found while working it.
