### Rust PostgreSQL server: a COPY error names its line

A COPY FROM error carries the CONTEXT PostgreSQL 15.19 gives it, and an
integer past its type's range is reported as out of range.

#### Fixed

- A COPY FROM error says where it is (RP-242). It carries PostgreSQL's
  CONTEXT, from `CopyFromErrorCallback`:
  - `COPY t, line 2, column a: "x"` for a value that does not parse;
  - `COPY t, line 2: "7\t1"` for a row a constraint or a BEFORE trigger
    refuses;
  - `COPY t, line 2` for a buffered row a unique index refuses when it is
    inserted.

  Each quotes at most 100 bytes of the text, then `...`, and the header
  line counts as line 1. A row refused by NOT NULL or CHECK is now reported
  before a bad value later in the same CopyData message, as PostgreSQL
  checks each row as it reads it.

  Fixes RP-242 (tests/backlog/test_rp_batch64_copy.py::test_scenario[a COPY error names its line]).
- An integer past its type's range in COPY's input is 22003 `value "..."
  is out of range for type integer`, where it was 22P02 `invalid input
  syntax`. COPY reads integers and floats by their types' input functions,
  as a literal's cast reads them, so `1e400` is out of range for a float
  too. A bigint literal past the range (`'99999999999999999999'::int8`) is
  22003 as well. A float cast to bigint raises `bigint out of range` where
  it saturated (RP-244).

  Fixes RP-244 (tests/backlog/test_rp_batch64_copy.py::test_scenario[COPY FROM reads numbers by their input functions]).
