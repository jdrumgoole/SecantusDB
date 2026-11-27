### `record_image` operators image a composite with a dropped attribute

When a composite type has a dropped attribute (`ALTER TYPE ... DROP
ATTRIBUTE`, or a table's row type after `ALTER TABLE ... DROP COLUMN`), a
`*=` / `*<` comparison over one of its values now compares the bytes
PostgreSQL compares. The value's header counts the dropped slot, and its
NULL bitmap always marks the slot absent. Before this fix the value was
imaged as though the type had never been altered, so
`row(row(null, 'a')::cx) *< row(row(1, 'a')::cx)` was false where
PostgreSQL 15 says true.

#### Fixed

- `record_image` comparisons over a composite with a dropped attribute
  gave the wrong answer. Fixes RP-976, in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_a_composite_with_a_dropped_attribute)
