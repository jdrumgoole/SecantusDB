### Rust PG server: a domain column's whole row is typed by its domain

`select fd = row(1) from fd`, where `fd` is `(a dint)` and `dint` is a domain
over `int`, answered true. PostgreSQL 15.19 refuses it with 42804 `cannot
compare dissimilar column types dint and integer at record column 1`. A
table's row type carries each column's declared type, so a domain column's
field is of the domain and not of its base type, and `record_eq` tells the
two apart. This server recorded the field under the base type, so it
compared the values and gave a confident wrong answer.

#### Fixed

- A table's row type records a domain column under its domain. This
  changes four things:
  - a whole row against a record whose field is the base type is 42804,
    for `=`, `<`, IS DISTINCT FROM, `= ANY`, IN and NULLIF alike. Against a
    field of the domain itself (`fd = row(1::dint)`) it still compares;
  - `pg_typeof((fd).a)` names the domain (`dint`). It answered `integer`;
  - a cast to the table's row type checks the domain's constraints:
    `row(0,'x')::fp` and `'(0,x)'::fp`, with `fp.a` a `pint` (`check (value
    > 0)`), are 23514. Both answered `(0,x)`;
  - a record comparison no longer reads a domain as its base type. A
    composite made by `CREATE TYPE` with a domain field is refused against
    the base type too (`row(1,'x')::ctp = row(1,'x'::text)` is 42804, as
    on PostgreSQL).

  A field whose domain is over `char(n)` is still padded to the width
  (`select fc from fc` is `("ab ",1)`), and nested domains are checked
  innermost first. A table whose row type was recorded before this change
  keeps its base types until an ALTER TABLE rewrites the row type.

Fixes RP-529 (tests/backlog/test_rp_found_rp522.py::test_domain_field_of_a_whole_row),
with the comparison pinned in the crate by
`tests::composite_record_compare_tells_a_domain_from_its_base`. Files RP-551
(a cast to a domain over `char(n)` is not blank-padded), RP-552 (a domain's
CHECK error inside a record literal has no position), RP-553 (a schema's
domain reports its CHECK under the qualified name), RP-554 (a scalar
subquery's array of a row type is described as `text[]`), RP-555 (`ALTER
DOMAIN ... NOT VALID` checks the rows) and RP-556 (`ALTER COLUMN TYPE` to a
domain over the column's type is refused), found while working it.
