DROP TABLE IF EXISTS tnlg_t
DROP FUNCTION IF EXISTS tnlg_f("int")
DROP DOMAIN IF EXISTS tnlg_dd
DROP DOMAIN IF EXISTS "integer"
DROP TYPE IF EXISTS "smallint"
DROP TYPE IF EXISTS "real"
DROP TYPE IF EXISTS "boolean"
DROP TYPE IF EXISTS "double precision"
DROP TYPE IF EXISTS "character varying"
DROP TYPE IF EXISTS "bigint"
DROP TYPE IF EXISTS tnlg_y
DROP TYPE IF EXISTS tnlg_x
DROP TYPE IF EXISTS "int"
CREATE TYPE int AS ENUM ('a', 'b')
CREATE DOMAIN integer AS text CHECK (value <> 'x')
CREATE TYPE smallint AS (x int, y smallint)
CREATE TYPE real AS RANGE (subtype = float8)
CREATE TABLE tnlg_t (a "int", b "integer", c "smallint", d int, e smallint, f "int"[])
INSERT INTO tnlg_t VALUES ('b', 'q', row(1, 2), 5, 6, '{a,b}')
