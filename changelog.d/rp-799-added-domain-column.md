### Rust PG server: a column `ALTER TABLE ... ADD COLUMN` adds of a domain is one of the domain

`secantusd-pg` now types a column that `ADD COLUMN` adds of a domain the
way it types one `CREATE TABLE` declares: stored as the domain's innermost
base type at that base's width, and tagged with the domain. Before, the
column was typed by the domain's name and nothing else, so the domain had
no effect on it.

#### Fixed

- An added domain column now works with the base type's functions and
  aggregates. `string_agg` over a domain over `text` used to fail with
  42883.
- The domain's `NOT NULL` and `CHECK` constraints are now enforced on every
  write. They are also checked against the rows already in the table, so
  adding a `NOT NULL` domain column whose value would be NULL fails with
  23502, as on PostgreSQL 15.19.
- Existing and new rows now get the domain's `DEFAULT` unless the column
  declares its own.
- A column of a domain over `char(n)` is now padded to its width.
- `information_schema.columns` now reports the column's base type and names
  the domain.

Fixes RP-799 (tests/backlog/test_rp_found_rp782.py::test_an_added_domain_column_reaches_its_base_types_aggregates,
tests/backlog/test_rp_found_rp799.py::test_an_added_domain_column_is_one_of_the_domain)

Filed while working it: RP-824 (a column retyped off its domain still
enforces the domain), RP-825 (a domain's DEFAULT is listed as its column's
default), RP-826 (a domain over a domain does not take the inner domain's
DEFAULT), RP-827 (`information_schema.columns.data_type` of a domain over a
domain).
