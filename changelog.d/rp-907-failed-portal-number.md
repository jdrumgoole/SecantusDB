### A portal that fails as it runs has still taken its number

PostgreSQL names an unnamed PL/pgSQL portal once its query is planned and before it runs, so a FOR loop or OPEN whose query fails at run time, caught by an exception block, has already spent its `<unnamed portal N>` number. The Rust PostgreSQL server named the portal only after reading its rows, so the next portal reused the number. It now reserves the name before running the query, as `CreateNewPortal` does; a query that fails while planning still takes no number.

#### Fixed

- An unnamed PL/pgSQL portal whose query fails as it runs takes its number, so the next portal is `<unnamed portal 2>` as on PostgreSQL 15.19.
  Fixes RP-907 (tests/backlog/test_rp_found_rp828.py::test_a_portal_that_fails_running_takes_its_number)
