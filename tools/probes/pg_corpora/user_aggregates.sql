# CREATE AGGREGATE: a state function folded over the group, an initial
# condition, a final function; strict and non-strict state functions.
CREATE FUNCTION add_int(a int, b int) RETURNS int LANGUAGE sql AS 'select coalesce($1, 0) + coalesce($2, 0)'
CREATE AGGREGATE mysum(int) (sfunc = int4pl, stype = int)
SELECT mysum(n) FROM ag
SELECT g, mysum(n) FROM ag GROUP BY g ORDER BY g
SELECT mysum(n) FROM ag WHERE false
SELECT mysum(n) FILTER (WHERE id > 1) FROM ag
SELECT mysum(DISTINCT n) FROM ag
SELECT pg_typeof(mysum(n)) FROM ag
SELECT mysum(n) + 1 AS x FROM ag
SELECT g FROM ag GROUP BY g HAVING mysum(n) > 9 ORDER BY g
SELECT g, mysum(n) FROM ag GROUP BY g ORDER BY mysum(n) DESC
CREATE AGGREGATE mysum2(int) (sfunc = add_int, stype = int, initcond = '100')
SELECT mysum2(n) FROM ag
SELECT mysum2(n) FROM ag WHERE false
CREATE FUNCTION cat_fn(s text, v text) RETURNS text STRICT LANGUAGE plpgsql AS $$ begin return s || ',' || v; end $$
SELECT cat_fn('a', NULL)
CREATE AGGREGATE mycat(text) (sfunc = cat_fn, stype = text, initcond = '')
SELECT mycat(t ORDER BY t) FROM ag
SELECT mycat(t ORDER BY t DESC) FROM ag
CREATE FUNCTION fin(s text) RETURNS int LANGUAGE sql AS 'select length($1)'
CREATE AGGREGATE mylen(text) (sfunc = cat_fn, stype = text, initcond = '', finalfunc = fin)
SELECT mylen(t) FROM ag
SELECT pg_typeof(mylen(t)) FROM ag
CREATE AGGREGATE mymax(int) (sfunc = int4larger, stype = int)
SELECT mymax(n), mysum(n), count(*) FROM ag
CREATE AGGREGATE mysum(int) (sfunc = int4pl, stype = int)
CREATE OR REPLACE AGGREGATE mysum(int) (sfunc = int4pl, stype = int, initcond = '0')
SELECT mysum(n) FROM ag WHERE false
SELECT mysum(t) FROM ag
CREATE AGGREGATE bad(int) (sfunc = no_such, stype = int)
CREATE AGGREGATE bad(int) (stype = int)
CREATE AGGREGATE bad(int) (sfunc = int4pl)
CREATE AGGREGATE oldsum (basetype = int, sfunc = int4pl, stype = int)
SELECT oldsum(n) FROM ag
SELECT proname, prokind FROM pg_proc WHERE proname IN ('mysum', 'add_int') ORDER BY proname
DROP FUNCTION mysum(int)
DROP FUNCTION add_int(int, int)
DROP AGGREGATE mysum(int)
SELECT mysum(n) FROM ag
DROP AGGREGATE mysum(int)
DROP AGGREGATE IF EXISTS mysum(int)
DROP AGGREGATE mysum2(int)
DROP FUNCTION add_int(int, int)
DROP AGGREGATE oldsum(int)
