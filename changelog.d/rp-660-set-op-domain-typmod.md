### A set operation of a domain column and its base type describes no width

`select a from t union select c from t`, where `a` is of a domain over
`char(3)` and `c` is a `char(3)` column, now describes the result column
with no type modifier (-1), as PostgreSQL does: `select_common_type`
smashes the domain to its base, and `select_common_typmod` then finds a
side that was not of the common type. It described the width, 7. Two sides
of the one domain, or of `char(3)`, keep it, and the values are padded as
before.

#### Fixed

- A set operation's column modifier treats a domain side as of the domain.
  Fixes RP-660 (tests/backlog/test_rp_found_rp557.py::test_set_op_of_a_domain_and_its_base_type)
