# reference-version: 15
# psycopg gauge regressions (batch 49).
# An untyped literal beside a range takes the range's type.
SELECT 'empty' = 'empty'::int4range
SELECT 'empty' = '[1,2)'::int4range
SELECT '[1,3)'::int8range = '[1,3)'
SELECT 'empty'::datemultirange = '{}'
SELECT '[1,5)'::int4range @> '[2,3)'
# A bpchar with no length keeps its trailing blanks; char(n) still pads.
SELECT chr(32)::bpchar, octet_length(chr(32)::bpchar), length(chr(32)::bpchar)
SELECT 'a  '::bpchar, octet_length('a  '::bpchar), 'a  '::bpchar = 'a'
SELECT ARRAY[' ', 'b ']::bpchar[]
SELECT '{" ","b "}'::bpchar[]
SELECT 'ab'::char(4), octet_length('ab'::char(4))
# A built-in over an untyped literal resolves to its overload's type.
SELECT ARRAY[set_byte('x', 0, 1)]
SELECT '{"\\x01"}'::bytea[] = ARRAY[set_byte('x', 0, 1)]
SELECT pg_typeof(ARRAY[set_byte('x', 0, 1)])
# A temp table may REFERENCE itself; its errors name the bare relation.
CREATE TEMP TABLE b49_self (data int PRIMARY KEY, ref int REFERENCES b49_self (data) DEFERRABLE INITIALLY DEFERRED)
INSERT INTO b49_self VALUES (1, 1)
CREATE TEMP TABLE b49_chk (data int CONSTRAINT b49_chk_eq1 CHECK (data = 1))
INSERT INTO b49_chk VALUES (2)
DROP TABLE b49_chk
DROP TABLE b49_self
