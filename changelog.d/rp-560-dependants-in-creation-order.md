### A DROP names what depends on a table in the order it was made

When a DROP is refused because other objects depend on what it drops, or
drops them with CASCADE, PostgreSQL names them in the order `pg_depend`
holds them: the order they were created, by oid. The Rust PostgreSQL server
named a table's dependants by kind instead -- its inheritance children,
then the foreign keys referencing it, then its views -- and named the casts
over its row type after every routine, because a cast's oid was derived
from its types rather than drawn from the oid counter. Foreign keys and
casts now draw their oids from the one counter as PostgreSQL's
`CreateConstraintEntry` and `CastCreate` do, and the walk visits each
object's dependants in oid order, so the DETAIL and the CASCADE notice read
as PostgreSQL's do. DROP SCHEMA walks each member relation the same way, so
it also names a routine in another schema over a member's row type, which
its RESTRICT refusal used to leave out.

#### Fixed

- DROP TABLE's 2BP01 DETAIL and CASCADE notice name the inheritance
  children, the foreign keys referencing the table and its views in
  creation order, and the routines, operators and casts over its row type
  in creation order, each routine followed by the casts it implements. A
  routine over both `t` and `t[]` is named under `t`, its last place.
- DROP FUNCTION names the casts and operators a function implements in
  creation order rather than the casts first.
- DROP SCHEMA names what depends on each member as DROP TABLE or DROP VIEW
  of it would: a routine elsewhere over a member's row type is named after
  that member in the RESTRICT DETAIL as well as the CASCADE notice, and a
  foreign key between two members is no longer named in the notice.
  Fixes RP-560 (tests/backlog/test_rp_found_rp546.py::test_drop_table_dependants_in_creation_order),
  regression test tests/backlog/test_rp_found_rp546.py::test_drop_dependants_by_oid_in_more_shapes

#### Changed

- A foreign key records the oid it drew (`oid` in its catalog entry), and a
  cast's oid is drawn from the counter. A foreign key or cast made before
  this change keeps its old oid and is named after the others.
