### A zero-count FETCH re-fetches the row the cursor sits on

A `FETCH 0`, `FETCH FORWARD 0` or `FETCH BACKWARD 0` on the Rust
PostgreSQL server now answers the row the cursor sits on, as PostgreSQL
does. A `MOVE 0` reports whether there is one (`MOVE 1` or `MOVE 0`).
PostgreSQL makes the re-fetch by stepping back one row and forward one
again, so on a `NO SCROLL` cursor sitting on a row the FETCH (`RELATIVE 0`
included) is refused with "cursor can only scan forward". The same step
now holds in PL/pgSQL's `FETCH` / `MOVE` and in `cursor_to_xml(c, 0, ...)`.

#### Fixed

- `FETCH FORWARD 0` / `FETCH 0` / `FETCH BACKWARD 0` answered no row, and
  `MOVE FORWARD 0` / `MOVE 0` reported `MOVE 0` on a row
  (`DoPortalRunFetch`'s zero count). A zero-count FETCH or `FETCH RELATIVE
  0` on a `NO SCROLL` cursor sitting on a row is now 55000. Before the
  first row or past the last it still answers no row.
- PL/pgSQL: `MOVE 0` / `MOVE RELATIVE 0` set FOUND false on a row, and
  `FETCH RELATIVE 0 ... INTO` from a `NO SCROLL` cursor read the row where
  PostgreSQL is 55000.
- `cursor_to_xml(c, 0, ...)` answered no row; it now answers the current
  row (55000 on a `NO SCROLL` cursor), as `SPI_cursor_fetch` does.
- Fixes RP-898 (tests/backlog/test_rp_found_rp744.py::test_a_zero_count_fetch_refetches_the_current_row).
  Regression tests:
  `tests/backlog/test_rp_found_rp744.py::test_a_zero_count_fetch_steps_back_on_a_no_scroll_cursor`,
  `::test_a_plpgsql_zero_count_step_sees_the_current_row`,
  `::test_cursor_to_xml_of_zero_rows_refetches_the_current_row`.
- Filed RP-942: a PL/pgSQL FETCH of more than one row is refused with
  42601 and no position, where PostgreSQL raises 0A000 at the FETCH.
