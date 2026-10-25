### Rust PostgreSQL server: operator classes of every index method

`CREATE OPERATOR CLASS` and `ALTER OPERATOR FAMILY` take the GiST, GIN,
SP-GiST and BRIN methods, as PostgreSQL 15.19 does, and a class's members
are recorded in `pg_depend`.

#### Fixed

- Operator classes and families of the GiST, GIN, SP-GiST and BRIN methods
  are kept, where they were refused by name with 0A000 (part of RP-169).
  - **Each method's limits.** No fixed strategy count. Support functions
    run 1 to 11 for GiST, 1 to 7 for GIN and SP-GiST, and 1 to 15 for
    BRIN, with each one's options function.
  - **Ordering operators.** ORDER BY members are allowed where the method
    orders by an operator: GiST and SP-GiST.
  - **STORAGE.** A STORAGE type is allowed where the method stores one.
  - **Hard and soft members** follow each method's `amadjustmembers`. A
    GiST, GIN or SP-GiST operator, and an optional support function, is
    the family's. A required support function is the class's, and an
    unknown one is 22023. A BRIN member is the class's.
- `pg_depend` lists a user operator family's and class's rows, where it
  listed none:
  - a family and a class in their schema, and a class in its family and
    on its input type;
  - each member on its class (`i`) or its family (`a`), and on its operator
    or function.

  Fixes part of RP-169 (tests/backlog/test_rp_batch51.py::test_scenario[a GiST operator class],
  tests/backlog/test_rp_batch72.py::test_scenario[operator classes of the GiST GIN SP-GiST and BRIN methods],
  tests/backlog/test_rp_batch72.py::test_scenario[a class's members depend on it or on its family]).
