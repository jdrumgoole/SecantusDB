### `record_image` operators compare composite fields

A record holding a named composite field can now be compared with `*=`,
`*<` and the other `record_image` operators. The field is compared by the
same bytes PostgreSQL compares: the tuple header `heap_form_tuple` builds,
then its fields laid out as `heap_fill_tuple` lays them, short varlenas
included, so `row(256, 'a')::ca` sorts before `row(1, 'b')::ca` exactly as
on PostgreSQL 15.

#### Fixed

- `record_image` comparisons over a named composite field were refused
  with 0A000. Fixes RP-976, in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_composite_fields)
