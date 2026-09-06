### A catalog to ask about types

psycopg discovers a type by sending one query — `pg_type` joined to
`to_regtype()`, with a `::regtype::text` cast in the select list — and the Rust
PostgreSQL server had none of its five ingredients: no `pg_type`, no
`to_regtype`, no `regtype` cast worth the name, no table aliases and no cast of
a column in a select list. Type discovery failed wholesale, and with it the
whole family of client features built on it — registering an enum, a composite,
a custom range.

`pg_type` is now a virtual table: a definition and rows computed on read from
the same builtin-type catalog that names oids everywhere else, so the two can
never disagree. `to_regtype()` resolves either spelling of a name (`int4` or
`integer`) and answers NULL — not an error — for one it does not know, which is
the whole reason clients prefer it to the `::regtype` cast, which errors.

A `regtype` value itself carries two natures no single scalar holds: it prints
as the type's display name while comparing as its oid. `select
to_regtype('text')` shows `text`; `where t.oid = to_regtype('text')` compares
25. Casting one onward follows the same split — to text as the name, to an
integer as the oid.

The vehicle for the `oid::regtype::text` select item — a chain of casts applied
per column of a table read — works for any casts, not just these, and the
described column type follows the last cast in the chain, so a client decodes
the rows it was promised. `pg_prepared_statements` rides along as an empty
virtual table, which is what psycopg's pipeline tests count rows in.

The `oid` type came with it — psycopg's numeric tests cast to it constantly —
with PostgreSQL's own edges: a negative literal wraps (`(-1)::oid` is
4294967295), a value past 2³²−1 is out of range rather than wrapped, and the
binary format is the 4-byte unsigned form.

#### Added

- The `pg_type` and `pg_prepared_statements` virtual catalog tables.
- The `oid` type: casts from integers and text, both wire formats, unsigned
  wrap-around and range errors as PostgreSQL reports them.
- `to_regtype()`, a real `regtype` (prints as a name, compares as an oid), and
  cast chains over columns in a table select list.
- Table aliases (`FROM pg_type t ... WHERE t.oid`).
