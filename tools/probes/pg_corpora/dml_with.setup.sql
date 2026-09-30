DROP TABLE IF EXISTS dw_src
DROP TABLE IF EXISTS dw_arch
DROP TABLE IF EXISTS dw_ids
CREATE TABLE dw_src (id int PRIMARY KEY, v text)
CREATE TABLE dw_arch (id int, v text)
CREATE TABLE dw_ids (id serial PRIMARY KEY, name text)
INSERT INTO dw_src VALUES (1, 'a'), (2, 'b'), (3, 'c')
CREATE OR REPLACE FUNCTION dw_add(n text) RETURNS int LANGUAGE plpgsql AS $$ DECLARE x int; BEGIN INSERT INTO dw_ids (name) VALUES (n) RETURNING id INTO x; RETURN x; END $$
CREATE OR REPLACE FUNCTION dw_upd(k int) RETURNS text LANGUAGE plpgsql AS $$ DECLARE r text; BEGIN UPDATE dw_src SET v = v || '!' WHERE id = k RETURNING v INTO r; RETURN r; END $$
CREATE OR REPLACE FUNCTION dw_strict(k int) RETURNS text LANGUAGE plpgsql AS $$ DECLARE r text; BEGIN DELETE FROM dw_src WHERE id = k RETURNING v INTO STRICT r; RETURN r; END $$
