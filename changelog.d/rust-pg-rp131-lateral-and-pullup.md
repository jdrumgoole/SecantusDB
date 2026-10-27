### Rust PostgreSQL server: the `information_schema` routine views in half the time again

`information_schema.routines`, `parameters`, `data_type_privileges` and
`element_types` now take about 0.05, 0.11, 0.25 and 0.42 seconds on an
optimised build without LTO, from 0.09, 0.21, 0.49 and 0.85 (measured side
by side on one machine). Every row is the same as before, byte for byte, and
the same as PostgreSQL 15.19's. Each change follows what PostgreSQL itself
does (RP-131, part).

`nameconcatoid`, behind every `specific_name`, now cuts the name rather than
the oid suffix to fit `NAMEDATALEN`, as PostgreSQL does.

#### Changed

- **A LATERAL function scan is planned once.** `FROM t, f(t.a)` -- what
  `_pg_expandarray` in a select list becomes -- planned its SQL again for
  every distinct left row (668 times for `parameters`). A built-in
  set-returning function, with or without `WITH ORDINALITY`, is now a
  function scan planned once whose rescan only runs the function over the
  new values (`ExecReScanFunctionScan`); anything else still plans its SQL
  (`joins::FunctionScan`).
- **What parse analysis settles once, once.** A planned expression now keeps
  a call's resolution (`function_absent_in_reference`, which also evaluated
  every argument a second time), an operator's operand check
  (`array_operand_mismatch`), each node's type as typed without a value, and
  the parameter types it declares, rather than working them out for every
  row (`expr_memo.rs`).
- **An unread output is not kept.** A FROM-subquery's outputs that
  `remove_unused_subquery_outputs` removed (and NULL ones nothing reads) are
  left out of the rows the executor materialises, and a join leaf's rows
  hold only the columns a joined row carries. PostgreSQL's own views
  (`pg_catalog`, `information_schema`) are no longer planned alone before
  their outputs are pruned: `initdb` analysed them, and parse analysis never
  runs on a view's stored query again.
- Lookups no longer done for every row or every catalog read: the PG 15
  catalog column order is read once, and the user function, base type and
  composite type registries are not searched when they are empty.

#### Fixed

- `nameconcatoid(name, oid)` kept a long name whole and overran 63 bytes;
  it now cuts the name at a character boundary so `_<oid>` always fits, as
  `name.c` does. Fixes RP-305
  (`tests/backlog/test_rp131_names.py::test_nameconcatoid_cuts_the_name_not_the_suffix`).
