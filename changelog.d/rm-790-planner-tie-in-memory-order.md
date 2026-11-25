### A planner tie goes by mongod's in-memory index order

When several indexes cost the same, the query planner takes the first of them
in the collection's in-memory index catalog, as `mongod` 8.2.11 does.

#### Fixed

- An index a `collMod` changed (`prepareUnique` set or cleared, `hidden` set
  or cleared, or `expireAfterSeconds` set, even to the value it had) moves to
  the end of that catalog, so it comes last among equal candidates and wins
  only when no other ties. A `collMod` that changes nothing does not move it,
  `reIndex` builds every index again in build order, and a restart forgets the
  move. This server tried candidates in build order whatever a `collMod` had
  done. `listIndexes` and the durable catalog keep build order, as before.

Fixes RM-790 (tests/backlog/test_rm_index_spec_fidelity.py::test_a_planner_tie_goes_by_the_in_memory_index_order)
