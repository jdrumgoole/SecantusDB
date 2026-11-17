### A multi-row PL/pgSQL FETCH is refused at the FETCH

`FETCH FORWARD 2 FROM c INTO r` in a PL/pgSQL body is refused when the body
is compiled, as PostgreSQL's grammar refuses it: SQLSTATE 0A000, `FETCH
statement cannot return multiple rows`, with the error position at the
FETCH keyword rather than at the token after its INTO target.

#### Fixed

- A compile error `pl_gram.y` raises at a statement's first token now
  points there; the run-time check answers 0A000 instead of 42601.
  Fixes RP-942 (tests/backlog/test_rp_found_rp898.py::test_a_multi_row_plpgsql_fetch_is_feature_not_supported)
