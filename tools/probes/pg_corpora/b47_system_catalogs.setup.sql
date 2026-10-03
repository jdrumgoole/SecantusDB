DROP FUNCTION IF EXISTS b47f(int)
DROP FUNCTION IF EXISTS b47g()
CREATE FUNCTION b47f(x int) RETURNS int LANGUAGE sql SECURITY DEFINER SET work_mem = '64kB' SET search_path = public, pg_temp AS 'select x'
CREATE FUNCTION b47g() RETURNS text LANGUAGE sql SET work_mem = '128kB' AS 'select current_setting(''work_mem'')'
