DROP FUNCTION IF EXISTS fdx(int, text, int, numeric)
DROP FUNCTION IF EXISTS fdp(int)
CREATE FUNCTION fdx(a int, b text DEFAULT 'x', c int DEFAULT 2 + 3, d numeric DEFAULT 1.5) RETURNS text LANGUAGE sql AS 'select b || a || c || d'
CREATE FUNCTION fdp(n int DEFAULT 10) RETURNS int LANGUAGE plpgsql AS $$ begin return n * 2; end $$
