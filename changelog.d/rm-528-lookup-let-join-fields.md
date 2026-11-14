### Rust MongoDB server: `$lookup` types its fields and pairs its join fields as mongod does

The Rust MongoDB server now refuses a malformed `$lookup` with mongod 8.2.11's
own errors where it used to run it or answer a generic BadValue (2).

#### Fixed

- A `let` that is not a document, or a `localField`, `foreignField` or `as`
  that is not a string, is refused with 14 "BSON field '$lookup.<field>' is
  the wrong type '<type>', expected type '<object|string>'". Like mongod's
  IDL, the fields are typed in document order, so a wrong type and an
  unknown field (40415) are reported in the order they appear, and both
  come before a missing `as` (40414). A null field counts as absent, so a
  null `as` is now a missing one (40414) rather than 2.
- `localField` without `foreignField`, or the reverse, is refused with 9
  "$lookup requires both or neither of 'localField' and 'foreignField' to
  be specified", with a pipeline or without. Without a pipeline the same 9
  now refuses a `$lookup` that names neither (it used to answer 2), and a
  `let` without a pipeline is refused with 9 "$lookup with a 'let' argument
  must also specify 'pipeline'". An empty join field counts as absent.
- Fixes RM-528 (tests/backlog/test_rm_found_2026_11_10_lookup.py::test_lookup_refuses_a_bad_let_and_a_lone_join_field);
  regression test `argtypes::stage_validation_order_tests::lookup_types_its_fields_in_order_then_pairs_the_join_fields`.
- Filed while working it: RM-645 (stage specs inside a `$facet` or
  `$lookup` pipeline are not checked as the pipeline parses) and RM-646
  (`$lookup` runs a `$`-prefixed join field, an empty `as` and a bad `let`
  variable name).
