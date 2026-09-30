CREATE FUNCTION ov(a int) RETURNS text LANGUAGE sql AS 'SELECT ''int '' || a'
CREATE FUNCTION ov(a text) RETURNS text LANGUAGE sql AS 'SELECT ''text '' || a'
CREATE FUNCTION ov(a int) RETURNS text LANGUAGE sql AS 'SELECT ''again'''
SELECT ov(1), ov('x'), ov('x'::text), ov(2::int)
SELECT ov(1.5)
CREATE OR REPLACE FUNCTION ov(a text) RETURNS text LANGUAGE sql AS 'SELECT ''TEXT '' || a'
SELECT ov('y')
SELECT count(*) FROM pg_proc WHERE proname = 'ov'
DROP FUNCTION ov
DROP FUNCTION ov(text)
SELECT ov('z')
SELECT ov(3)
DROP FUNCTION ov(int)
