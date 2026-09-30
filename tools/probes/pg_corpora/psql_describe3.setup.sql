DROP MATERIALIZED VIEW IF EXISTS dpz_m
DROP TABLE IF EXISTS dpz_t CASCADE
DROP FUNCTION IF EXISTS dpz_f(int, text)
DROP DOMAIN IF EXISTS dpz_d
CREATE TABLE dpz_t (id int PRIMARY KEY, name varchar(10) NOT NULL, amt numeric(8,2) DEFAULT 0, tags text[], note text)
COMMENT ON TABLE dpz_t IS 'the table'
COMMENT ON COLUMN dpz_t.note IS 'a note'
CREATE MATERIALIZED VIEW dpz_m AS SELECT id, name FROM dpz_t
CREATE FUNCTION dpz_f(a int, b text DEFAULT 'x') RETURNS text LANGUAGE sql STABLE AS 'select b || a'
CREATE DOMAIN dpz_d AS int CHECK (value > 0)
