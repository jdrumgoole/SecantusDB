### A two-dimensional int2vector cannot be printed as text

`array_agg` over `int2vector` or `oidvector` values is typed as the vector but
holds two dimensions, which PostgreSQL's `int2vectorout` / `oidvectorout`
refuse. Casting such a value to text, or printing it with `concat` or
`format`, now raises 42804 `array is not a valid int2vector`, as PostgreSQL
15.19 does, instead of printing `{1,2}`.

#### Fixed

- `array_agg(v)::text` and `concat(array_agg(v))` over vectors raise 42804.
  Fixes RP-602 (tests/backlog/test_rp_found_rp575.py::test_two_dimensional_vector_text)
