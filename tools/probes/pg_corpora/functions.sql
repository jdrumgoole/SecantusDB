CREATE FUNCTION add2(a int, b int) RETURNS int AS 'SELECT a + b' LANGUAGE sql
SELECT add2(2, 3)
SELECT id, add2(id, n) FROM fn_t ORDER BY id
SELECT id FROM fn_t WHERE add2(id, 1) > 2 ORDER BY id
CREATE FUNCTION fact(n int) RETURNS int AS $$ BEGIN IF n <= 1 THEN RETURN 1; END IF; RETURN n * fact(n - 1); END $$ LANGUAGE plpgsql
SELECT fact(5), fact(0)
CREATE FUNCTION grade(s int) RETURNS text AS $$ DECLARE g text; BEGIN CASE WHEN s >= 25 THEN g := 'high'; WHEN s >= 15 THEN g := 'mid'; ELSE g := 'low'; END CASE; RETURN g; END $$ LANGUAGE plpgsql
SELECT name, grade(n) FROM fn_t ORDER BY id
CREATE FUNCTION evens(m int) RETURNS SETOF int AS $$ BEGIN FOR i IN 1..m LOOP IF i % 2 = 0 THEN RETURN NEXT i; END IF; END LOOP; END $$ LANGUAGE plpgsql
SELECT * FROM evens(7)
CREATE FUNCTION names_like(p text) RETURNS SETOF text AS $$ SELECT name FROM fn_t WHERE name LIKE p ORDER BY name $$ LANGUAGE sql
SELECT * FROM names_like('a%')
CREATE FUNCTION total_n() RETURNS bigint AS $$ DECLARE t bigint; BEGIN SELECT sum(n) INTO t FROM fn_t; RETURN t; END $$ LANGUAGE plpgsql
SELECT total_n()
CREATE FUNCTION safe_div(a int, b int) RETURNS int AS $$ BEGIN RETURN a / b; EXCEPTION WHEN division_by_zero THEN RETURN -1; END $$ LANGUAGE plpgsql
SELECT safe_div(10, 2), safe_div(1, 0)
CREATE FUNCTION upsert_n(k int, v int) RETURNS text AS $$ BEGIN UPDATE fn_t SET n = v WHERE id = k; IF NOT FOUND THEN INSERT INTO fn_t VALUES (k, 'new', v); RETURN 'inserted'; END IF; RETURN 'updated'; END $$ LANGUAGE plpgsql
SELECT upsert_n(1, 11), upsert_n(9, 99)
SELECT id, n FROM fn_t ORDER BY id
CREATE FUNCTION tbl(m int) RETURNS TABLE (id int, name text) AS $$ SELECT id, name FROM fn_t WHERE n > m ORDER BY id $$ LANGUAGE sql
SELECT * FROM tbl(15)
SELECT add2(1)
CREATE FUNCTION add2(a int, b int) RETURNS int AS 'SELECT 1' LANGUAGE sql
CREATE OR REPLACE FUNCTION add2(a int, b int) RETURNS int AS 'SELECT a * b' LANGUAGE sql
SELECT add2(3, 4)
CREATE FUNCTION bad() RETURNS int AS $$ BEGIN RETURN nosuchvar + ; END $$ LANGUAGE plpgsql
DO $$ BEGIN RAISE NOTICE 'fact %', fact(4); END $$
CREATE FUNCTION nfact(n int) RETURNS numeric AS $$ BEGIN IF n <= 1 THEN RETURN 1; END IF; RETURN n * nfact(n - 1); END $$ LANGUAGE plpgsql
CREATE FUNCTION nmul(a int, b numeric) RETURNS numeric AS $$ BEGIN RETURN a * b; END $$ LANGUAGE plpgsql
SELECT nfact(20)
SELECT nfact(25)
SELECT nmul(100000, 99999999999)
SELECT pg_typeof(nfact(3))
SELECT 3 * 99999999999::numeric
SELECT nfact(150) > 0
CREATE FUNCTION runaway(n int) RETURNS int AS $$ BEGIN RETURN runaway(n + 1); END $$ LANGUAGE plpgsql
SELECT runaway(1)
SELECT 1
