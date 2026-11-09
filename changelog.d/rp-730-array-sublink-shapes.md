### Rust PG server: an `ARRAY(...)` over an array column refuses ragged, empty or NULL element arrays

An `ARRAY(...)` sublink over an array column gave back arrays PostgreSQL
refuses. `select array(select f from (values (array[1,2]), (array[3])) t(f))`
answered the ragged `{{1,2},{3}}`. A mix with `'{}'` answered `{{1},{}}`. A
mix with `array[[3]]`, or with a NULL array, failed in the binary send as 22P03.
The correlated form did the same. PostgreSQL 15.19 raises 2202E `cannot
accumulate arrays of different dimensionality` and 22004 `cannot accumulate
null arrays`.

PostgreSQL accumulates the rows with `accumArrayResultArr`. It refuses a NULL
array, an empty first array (2202E `cannot accumulate empty arrays`), and a
first array whose extra dimension would pass six (54000). Every later array
must have the first one's lengths and lower bounds. Those checks are now one
function, `secantus_pgplan::arrays::check_accumulable`. The uncorrelated
sublink, the correlated sublink and `array_agg` all call it. `array_agg` had
its own copy of the checks, which lacked the six-dimension limit. It now
raises that 54000 too.

#### Fixed

- Fixes RP-730 (tests/backlog/test_rp_found_rp729.py::test_array_sublink_refuses_ragged_or_null_element_arrays).
  The checks are also pinned by the `secantus-pgplan` unit test
  `arrays::tests::accumulating_arrays_needs_one_shape_and_no_null`.
- Filed RP-731: an accumulated element array whose lower bounds are not 1
  comes back as NULL, where PostgreSQL keeps the bounds
  (tests/backlog/test_rp_found_rp730.py).
- Filed RP-732: with ORDER BY the select list is computed after the sort, so
  a different row's error is raised than on PostgreSQL
  (tests/backlog/test_rp_found_rp730.py).
