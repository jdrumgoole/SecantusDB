### The record_image operators compare money and bit-string fields

The binary-image comparison operators `*=`, `*<` and the rest no longer refuse a record that holds a `money` or bit-string field. A money field compares as the int64 PostgreSQL passes by value, so a negative amount sorts above a positive one exactly as it does there, and a `bit` or `bit varying` field compares by its `VarBit` image, bit count first.

#### Fixed

- Rust PG server: `record_image` comparisons over `money`, `bit` and `varbit` fields, previously 0A000.
  Fixes RP-976 in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_money_and_bit_strings)
