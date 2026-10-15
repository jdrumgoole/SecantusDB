# reference-version: 15
# A user type named like a grammar spelling (`int`, `integer`, `smallint`, `double precision`, ...): none is a pg_type row, so PostgreSQL 15.19 creates `public."int"` beside int4; a bare `int` stays int4 and only the quoted spelling reaches the user type (RP-038).
CREATE TYPE int AS ENUM ('z')
CREATE TYPE public.int AS ENUM ('z')
CREATE TYPE "int" AS ENUM ('z')
CREATE DOMAIN int AS int
SELECT 'a'::"int", pg_typeof('a'::"int")::text
SELECT 'a'::public."int", cast('b' AS "int")
SELECT 'zz'::"int"
SELECT null::"int" IS NULL
SELECT 1::int, 2::integer, 3::smallint, 1.5::real, pg_typeof(1::int)::text, pg_typeof(1.5::real)::text
SELECT 'b'::"int" > 'a'::"int", enum_range(null::"int")::text
SELECT array['a', 'b']::"int"[], pg_typeof(array['a']::"int"[])::text
SELECT format_type('"int"'::regtype, null), '"int"'::regtype::text, 'public.int'::regtype::text, 'int'::regtype::text
SELECT '"int"[]'::regtype::text, '_int'::regtype::text, to_regtype('integer')::text
SELECT typname, typtype FROM pg_type WHERE typname IN ('int', '_int', 'integer', 'smallint', '_smallint', 'real') AND typnamespace = 'public'::regnamespace ORDER BY 1
SELECT 'y'::"integer", pg_typeof('y'::"integer")::text, pg_typeof(3::integer)::text
SELECT 'x'::"integer"
SELECT row(1, 2)::"smallint", pg_typeof(row(1, 2)::"smallint")::text, (row(1, 2)::"smallint").y
SELECT '[1,2)'::"real", pg_typeof('[1,2)'::"real")::text
SELECT a, b, c::text, d, e, f::text FROM tnlg_t
SELECT pg_typeof(a)::text, pg_typeof(b)::text, pg_typeof(c)::text, pg_typeof(d)::text, pg_typeof(e)::text, pg_typeof(f)::text FROM tnlg_t
SELECT attname, format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'tnlg_t'::regclass AND attnum > 0 ORDER BY attnum
SELECT column_name, data_type, udt_name FROM information_schema.columns WHERE table_name = 'tnlg_t' ORDER BY ordinal_position
INSERT INTO tnlg_t (a) VALUES ('zz')
SELECT a::text || 'x', a = 'b' FROM tnlg_t
ALTER TYPE "int" ADD VALUE 'c'
SELECT enum_range(null::"int")::text
ALTER TYPE "int" RENAME VALUE 'c' TO 'cc'
CREATE FUNCTION tnlg_f(x "int") RETURNS text LANGUAGE sql AS $$ SELECT x::text $$
SELECT tnlg_f('a'), tnlg_f('b'::"int")
DROP FUNCTION tnlg_f("int")
CREATE DOMAIN tnlg_dd AS "int"
SELECT 'a'::tnlg_dd::text, pg_typeof('a'::tnlg_dd)::text
DROP DOMAIN tnlg_dd
CREATE TYPE tnlg_x AS ENUM ('q')
ALTER TYPE tnlg_x RENAME TO bigint
SELECT 'q'::"bigint", pg_typeof('q'::"bigint")::text, 7::bigint, pg_typeof(7::bigint)::text
ALTER TYPE "bigint" RENAME TO tnlg_y
SELECT 'q'::tnlg_y, pg_typeof('q'::tnlg_y)::text
DROP TYPE tnlg_y
CREATE TYPE boolean AS ENUM ('t')
SELECT true::boolean, 't'::"boolean", pg_typeof(true::boolean)::text, pg_typeof('t'::"boolean")::text
DROP TYPE public.boolean
CREATE TYPE "double precision" AS ENUM ('x')
SELECT 'x'::"double precision", 2::double precision, pg_typeof('x'::"double precision")::text
DROP TYPE "double precision"
CREATE TYPE "character varying" AS ENUM ('x')
SELECT 'x'::"character varying", 'q'::character varying(3), pg_typeof('x'::"character varying")::text
DROP TYPE "character varying"
SELECT 'a'::pg_catalog."int"
DROP TABLE tnlg_t
DROP DOMAIN "integer"
DROP TYPE "smallint"
DROP TYPE "real"
DROP TYPE "int"
DROP TYPE IF EXISTS "int"
DROP TYPE "int"
SELECT 'a'::"int"
CREATE TABLE tnlg_t (c "int")
SELECT pg_typeof(1::int)::text
