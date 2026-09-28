# --- multiple windows in one select, and repeated use of one
SELECT id, row_number() OVER (ORDER BY id), rank() OVER (ORDER BY v), count(*) OVER () FROM w9 ORDER BY id
SELECT id, sum(v) OVER (PARTITION BY g), sum(v) OVER (ORDER BY id) FROM w9 ORDER BY id
# --- windows beside plain and computed columns
SELECT id, g, upper(g), row_number() OVER (PARTITION BY g ORDER BY id) FROM w9 ORDER BY id
SELECT id * 2 AS d, row_number() OVER (ORDER BY id) FROM w9 ORDER BY d
# --- window over an expression, and PARTITION BY an expression
SELECT id, sum(v * 2) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, count(*) OVER (PARTITION BY v IS NULL) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY v IS NULL, id) FROM w9 ORDER BY id
# --- WHERE runs before the window
SELECT id, count(*) OVER () FROM w9 WHERE v IS NOT NULL ORDER BY id
SELECT id, sum(v) OVER (ORDER BY id) FROM w9 WHERE g = 'b' ORDER BY id
# --- ORDER BY / LIMIT / OFFSET run after it
SELECT id, row_number() OVER (ORDER BY id) AS rn FROM w9 ORDER BY id DESC LIMIT 2
SELECT id, row_number() OVER (ORDER BY id) AS rn FROM w9 ORDER BY rn OFFSET 4
# --- DISTINCT runs after it
SELECT DISTINCT count(*) OVER (PARTITION BY g) FROM w9 ORDER BY 1
# --- lag / lead defaults and offsets
SELECT id, lag(v) OVER (ORDER BY id), lead(v) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, lag(v, 2) OVER (ORDER BY id), lead(v, 2, -7) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, lag(v, 0) OVER (ORDER BY id) FROM w9 ORDER BY id
# --- ntile remainder spreading
SELECT id, ntile(1) OVER (ORDER BY id), ntile(4) OVER (ORDER BY id), ntile(9) OVER (ORDER BY id) FROM w9 ORDER BY id
# --- first/last/nth under the default frame, which is the classic trap
SELECT id, first_value(v) OVER (ORDER BY id), last_value(v) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, nth_value(v, 1) OVER (ORDER BY id), nth_value(v, 9) OVER (ORDER BY id) FROM w9 ORDER BY id
# --- min / max / avg / bool / array / string aggregates as windows
SELECT id, min(v) OVER (ORDER BY id), max(v) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, avg(v) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, count(v) OVER (ORDER BY id), count(*) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, array_agg(v) OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, string_agg(g, '-') OVER (ORDER BY id) FROM w9 ORDER BY id
SELECT id, bool_and(v > 5) OVER (ORDER BY id), bool_or(v > 25) OVER (ORDER BY id) FROM w9 ORDER BY id
# --- EXCLUDE, all three forms
# EXCLUDE is part of the FRAME clause, so PostgreSQL requires one before it --
# `over (order by v exclude current row)` is a syntax error there too. The
# first version of these lines had no frame, so BOTH servers raised the same
# syntax error and the differential counted it as agreement: three scenarios
# that tested nothing at all.
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING EXCLUDE CURRENT ROW) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING EXCLUDE GROUP) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING EXCLUDE TIES) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE CURRENT ROW) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY v GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE GROUP) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING EXCLUDE CURRENT ROW) FROM w9 ORDER BY id
# --- GROUPS frames
SELECT id, sum(v) OVER (ORDER BY v GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v GROUPS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY v GROUPS BETWEEN 1 FOLLOWING AND 2 FOLLOWING) FROM w9 ORDER BY id
# --- RANGE frames with value offsets
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN 10 PRECEDING AND CURRENT ROW) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v RANGE BETWEEN CURRENT ROW AND 10 FOLLOWING) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY v DESC RANGE BETWEEN 5 PRECEDING AND 5 FOLLOWING) FROM w9 ORDER BY id
# --- empty frames
SELECT id, sum(v) OVER (ORDER BY id ROWS BETWEEN 3 PRECEDING AND 2 PRECEDING) FROM w9 ORDER BY id
SELECT id, count(*) OVER (ORDER BY id ROWS BETWEEN 5 FOLLOWING AND 6 FOLLOWING) FROM w9 ORDER BY id
# --- DESC and NULLS ordering
SELECT id, row_number() OVER (ORDER BY v DESC NULLS LAST) FROM w9 ORDER BY id
SELECT id, rank() OVER (ORDER BY v DESC) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v DESC) FROM w9 ORDER BY id
# --- window inside a FROM-subquery and a CTE
SELECT rn FROM (SELECT row_number() OVER (ORDER BY id) AS rn FROM w9) s WHERE rn > 4 ORDER BY rn
WITH c AS (SELECT id, rank() OVER (ORDER BY v) AS r FROM w9) SELECT id, r FROM c WHERE r = 1 ORDER BY id
# --- named windows
SELECT id, sum(v) OVER w, count(*) OVER w FROM w9 WINDOW w AS (PARTITION BY g ORDER BY id) ORDER BY id
SELECT id, sum(v) OVER (w) FROM w9 WINDOW w AS (ORDER BY id) ORDER BY id
# --- an unknown named window is an error, not a silent whole-partition frame
SELECT id, sum(v) OVER nosuchwindow FROM w9
# --- ntile with a bad argument
SELECT id, ntile(0) OVER (ORDER BY id) FROM w9
