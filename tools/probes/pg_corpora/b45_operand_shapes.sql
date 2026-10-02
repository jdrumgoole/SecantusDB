# reference-version: 15
# Batch 45: operator resolution for an array beside a scalar (arr = 1
# matched), ANY / ALL over an ARRAY constructor or typed array, BETWEEN
# bounds, current_date & co, json extraction, count(*), HAVING / ORDER BY
# operands, int + float8 arithmetic, a window function in WHERE.
SELECT 1 FROM b45o WHERE b = a
SELECT 1 FROM b45o WHERE (a) = b
SELECT 1 FROM b45o WHERE upper(b) = 1
SELECT 1 FROM b45o WHERE length(b) = 'x'
SELECT 1 FROM b45o WHERE d = 1
SELECT 1 FROM b45o WHERE d + 1 = 'x'
SELECT 1 FROM b45o WHERE ts::date = 1
SELECT 1 FROM b45o WHERE extract(year from d) = 'x'
SELECT 1 FROM b45o WHERE date_part('year', d) = b
SELECT 1 FROM b45o WHERE arr = 1
SELECT 1 FROM b45o WHERE arr[1] = 'x'
SELECT 1 FROM b45o WHERE j = 1
SELECT 1 FROM b45o WHERE j->>'k' = 1
SELECT 1 FROM b45o WHERE j->'k' = 1
SELECT 1 FROM b45o WHERE bo = 1
SELECT 1 FROM b45o WHERE NOT bo = 1
SELECT 1 FROM b45o WHERE f = b
SELECT 1 FROM b45o WHERE n = b
SELECT 1 FROM b45o WHERE abs(a) = b
SELECT 1 FROM b45o WHERE a = ANY(ARRAY['x'])
SELECT 1 FROM b45o WHERE b IN (1, 2)
SELECT 1 FROM b45o WHERE a IN ('x')
SELECT 1 FROM b45o WHERE a BETWEEN 'x' AND 'y'
SELECT 1 FROM b45o WHERE b BETWEEN 1 AND 2
SELECT 1 FROM b45o WHERE b LIKE 1
SELECT 1 FROM b45o WHERE a LIKE 'x'
SELECT 1 FROM b45o WHERE (SELECT b FROM b45o) = 1
SELECT 1 FROM b45o WHERE EXISTS (SELECT 1 FROM b45o i WHERE i.b = b45o.a)
SELECT 1 FROM b45o WHERE row(a, b) = row(1, 2)
SELECT 1 FROM b45o WHERE now() = 1
SELECT 1 FROM b45o WHERE current_date = 'x'
SELECT 1 FROM b45o WHERE a::text = 1
SELECT 1 FROM b45o WHERE (a + f) = 'x'
SELECT 1 FROM b45o WHERE a % 2 = 'x'
SELECT 1 FROM b45o WHERE -a = 'x'
SELECT 1 FROM b45o WHERE b || 'y' = 1
SELECT 1 FROM b45o WHERE substring(b from 1 for 1) = 1
SELECT 1 FROM b45o WHERE trim(b) = 1
SELECT 1 FROM b45o WHERE position('x' in b) = 'q'
SELECT 1 FROM b45o WHERE (CASE WHEN bo THEN a ELSE 0 END) = b
SELECT 1 FROM b45o WHERE greatest(a, 1) = b
SELECT 1 FROM b45o WHERE count(*) OVER () = b
SELECT b = a FROM b45o
SELECT a < b FROM b45o
SELECT 1 FROM b45o GROUP BY a HAVING a = 'x'
SELECT 1 FROM b45o GROUP BY b HAVING count(*) = b
SELECT 1 FROM b45o x JOIN b45o y ON x.a = y.b
SELECT 1 FROM b45o ORDER BY a = b
SELECT 1 FROM generate_series(1,2) g WHERE g = 'x'
SELECT 1 FROM b45o WHERE ts > 5
SELECT 1 FROM b45o WHERE d - d = 'x'
SELECT 1 FROM b45o WHERE age(ts) = 1
SELECT 1 FROM b45o WHERE a = ANY(ARRAY['1'])
SELECT 1 FROM b45o WHERE b = ANY(ARRAY['x', 'y'])
SELECT 1 FROM b45o WHERE a = ANY(ARRAY[1, 2])
SELECT 1 FROM b45o WHERE a = ANY(arr)
SELECT 1 FROM b45o WHERE b = ANY(arr)
SELECT 1 FROM b45o WHERE a = ALL(ARRAY['x'])
SELECT 1 FROM b45o WHERE a NOT BETWEEN 'x' AND 'y'
SELECT 1 FROM b45o WHERE b NOT BETWEEN 1 AND 2
SELECT 1 FROM b45o WHERE a BETWEEN 0 AND 2
SELECT 1 FROM b45o WHERE arr = '{1,2}'
SELECT 1 FROM b45o WHERE arr = ARRAY[1,2]
SELECT 1 FROM b45o WHERE arr <> 1
SELECT 1 FROM b45o WHERE 1 = arr
SELECT 1 FROM b45o WHERE ts < current_timestamp
SELECT 1 FROM b45o WHERE d < current_date
SELECT 1 FROM b45o WHERE d < now()
SELECT 1 FROM b45o WHERE localtimestamp = 'nope'
SELECT 1 FROM b45o WHERE j->>'k' = '1'
SELECT 1 FROM b45o WHERE (j->>'k')::int = 1
SELECT 1 FROM b45o WHERE j->'k' = '1'
SELECT a FROM b45o ORDER BY a + 1
SELECT 1 FROM b45o GROUP BY a HAVING count(*) > 0
SELECT 1 FROM b45o WHERE (a + f) = 1.5
SELECT 1 FROM b45o WHERE (a * n) = 'x'
SELECT a + n FROM b45o
SELECT row_number() OVER () FROM b45o WHERE a = 1
SELECT 1 FROM b45o WHERE a IN (SELECT count(*) FROM b45o)
