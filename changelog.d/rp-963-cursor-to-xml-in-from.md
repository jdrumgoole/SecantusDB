### cursor_to_xml in FROM reads its cursor once

`select * from cursor_to_xml('c', 1, ...)` answers the cursor's next row
and leaves the cursor one row on, as the same call in a select list does.
The call used to fetch from the cursor once more while the statement was
being typed, so it skipped a row.

#### Fixed

- A cursor FETCH made by a function (`cursor_to_xml`) reads nothing while a
  statement is only typed or planned, as a sequence is not drawn then.
  Fixes RP-963 (tests/backlog/test_rp_found_rp744.py::test_cursor_to_xml_in_from_reads_the_cursor_once)
