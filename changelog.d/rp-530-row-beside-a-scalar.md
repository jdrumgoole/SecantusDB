### Rust PG server: a row compared with a scalar has no operator

`select fs = 1 from fs`, where `fs` is a table, failed with 0A000 `comparing
document with int32 using = is not supported yet`. The server found that
only while it compared a row, and some forms gave an answer instead of an
error. Over no rows (`... where false`), or beside a NULL (`fs =
null::int`), the query returned a result. `fs || 1` concatenated the row
with the number, `fs in (fs, 1)` answered true and `nullif(fs, 1)` returned
the row. PostgreSQL resolves the operator when it analyses the statement. A
row has only `record`'s comparisons, each against another row, so beside a
typed value that is not a row there is no operator. PostgreSQL 15.19 answers
42883 `operator does not exist: fs = integer` at the operator, whether or
not any row is read.

#### Fixed

- A row beside a typed operand that is not a row is now 42883 at the
  operator, with PostgreSQL's no-operator HINT. This covers a table's or a
  view's whole row, a `ROW(...)`, a FROM subquery's whole row and a value of
  a composite type, and these forms:
  - every binary operator (`fs = 1`, `(1,2) < 1::bigint`, `fs @> 1`,
    `fs <@ int4range(1,2)`);
  - IS [NOT] DISTINCT FROM and NULLIF, which use `=`;
  - IN and NOT IN, item by item in the list's order;
  - `= ANY` / `<> ALL` over an array, against its element type;
  - BETWEEN, as its `>=` / `<=` (NOT BETWEEN: `<` / `>`).
- The error names the operand types as written, with a domain under its
  own name (`fs = dint`).
- `||` beside a string still concatenates (`anynonarray || text`), now for
  a composite type's value too: `'a'::text || row(1)::ct` was 42883 and is
  `a(1)`, as on PostgreSQL.

Fixes RP-530 (tests/backlog/test_rp_found_rp522.py::test_record_against_scalar).
The crate regression test is
`secantus-pgplan`'s `tests::a_row_beside_a_scalar_has_no_operator`.

#### Filed

- RP-590: two different composite types are refused a comparison
  (`row(1)::ct = row(1)::ct2` is `t` on PostgreSQL).
- RP-591: an untyped literal beside a row is not read as `record`
  (`fs = 'x'`).
- RP-592: a row beside an `ARRAY[...]` names the row `text`.
- RP-593: an error a PREPARE's query raises carries the inner query's
  offset when a token of the PREPARE text happens to start there.
