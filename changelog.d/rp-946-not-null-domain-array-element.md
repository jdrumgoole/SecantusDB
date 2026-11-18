### A NULL element of a NOT NULL domain's array is refused

`select '{NULL}'::nnint[]`, `nnint` a domain over `int` declared NOT NULL,
is now 23502 `domain nnint does not allow null values`, as on PostgreSQL,
whose `array_in` reads every element, a NULL one too, with the domain's
input function. A NULL element skipped the domain, so the array was
accepted and could be stored.

#### Fixed

- The array literal parser checks the element domain on a NULL element,
  for a cast and for a write to a `nnint[]` column alike.
- Fixes RP-946 (tests/backlog/test_rp_found_rp552.py::test_not_null_domain_array_element)
