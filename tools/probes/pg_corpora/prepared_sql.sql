# SQL-level PREPARE / EXECUTE / DEALLOCATE, measured against PostgreSQL 14.
PREPARE p1(int, text) AS INSERT INTO pp VALUES ($1, $2)
EXECUTE p1(1, 'a')
EXECUTE p1(2, 'b')
PREPARE p2 AS SELECT v FROM pp WHERE id = $1
EXECUTE p2(2)
EXECUTE p2('1')
PREPARE p2 AS SELECT 1
EXECUTE nope
EXECUTE p2
EXECUTE p2(1, 2)
SELECT name, parameter_types::text, from_sql FROM pg_prepared_statements ORDER BY name
DEALLOCATE p1
DEALLOCATE nope
DEALLOCATE ALL
SELECT count(*) FROM pg_prepared_statements
PREPARE p3 AS SELECT $1::int + 1
EXECUTE p3(41)
PREPARE p4(int) AS SELECT $1 || 'x'
EXECUTE p4(5)
PREPARE p5 AS SELECT * FROM nosuch
EXECUTE p3('x')
PREPARE p6(int) AS UPDATE pp SET v = 'z' WHERE id = $1 RETURNING id, v
EXECUTE p6(1)
PREPARE p7 AS SELECT id, v FROM pp ORDER BY id
EXECUTE p7
EXECUTE p3(1 + 2)
DEALLOCATE PREPARE p3
SELECT name FROM pg_prepared_statements ORDER BY name
