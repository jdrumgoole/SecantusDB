# reference-version: 15
# ORDER BY an expression over a generate_series source (it was ignored: the lazy series path sorted by the series value only).
SELECT x from generate_series(1,6) x order by x % 2
SELECT x from generate_series(1,6) x order by x % 2, x desc
SELECT x * 10 as y from generate_series(1,4) x order by -x
SELECT x from generate_series(1,6) x order by 1 desc
SELECT x from generate_series(1,6) x order by x desc limit 2
SELECT g from generate_series(1,5) g order by g % 3 desc, g
SELECT x * 10 as y from generate_series(1,4) x order by y desc
SELECT x * 10 from generate_series(1,4) x order by 1 desc
SELECT x from generate_series(1,6) x where x > 2 order by x % 2 limit 3
SELECT count(*) from generate_series(1,10) x
SELECT x from generate_series(1,10,3) x order by x desc
