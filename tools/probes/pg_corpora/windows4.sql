# A window in a WHERE is an error in PostgreSQL, not a filter.
SELECT id FROM w9 WHERE row_number() OVER (ORDER BY id) = 1
# A window on each side of a set operation.
SELECT id, row_number() OVER (ORDER BY id) FROM w9 WHERE g = 'a' UNION ALL SELECT id, row_number() OVER (ORDER BY id) FROM w9 WHERE g = 'b'
# A window over a JOIN and over a generated source: refused, and the refusal
# should name what is missing.
SELECT w9.id, row_number() OVER (ORDER BY w9.id) FROM w9 JOIN w9 AS x ON x.id = w9.id
SELECT n, row_number() OVER (ORDER BY n) FROM generate_series(1,3) AS t(n)
# A window with several ORDER BY keys, and one with a RANGE offset over two
# (which PostgreSQL refuses outright).
SELECT id, row_number() OVER (PARTITION BY g ORDER BY v, id) FROM w9 ORDER BY id
SELECT id, sum(v) OVER (ORDER BY v, id RANGE BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM w9 ORDER BY id
