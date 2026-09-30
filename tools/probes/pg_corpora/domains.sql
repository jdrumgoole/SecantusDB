CREATE DOMAIN dm_pos AS int CHECK (VALUE > 0)
CREATE DOMAIN dm_nn AS text NOT NULL DEFAULT 'x' CONSTRAINT dm_nn_len CHECK (length(VALUE) < 5)
CREATE DOMAIN dm_email AS varchar(50) CHECK (VALUE LIKE '%@%')
CREATE DOMAIN dm_pos AS int
CREATE DOMAIN dm_bad AS nosuchtype
CREATE TABLE dm_t (id int PRIMARY KEY, q dm_pos, s dm_nn, e dm_email)
INSERT INTO dm_t VALUES (1, -1, 'a', 'a@b')
INSERT INTO dm_t (id, q) VALUES (2, 3)
INSERT INTO dm_t VALUES (3, 3, NULL, 'a@b')
INSERT INTO dm_t VALUES (4, 3, 'toolong', 'a@b')
INSERT INTO dm_t VALUES (5, 3, 'ok', 'nope')
INSERT INTO dm_t VALUES (6, NULL, 'ok', NULL)
SELECT (-5)::dm_pos
SELECT 5::dm_pos, NULL::dm_pos IS NULL
SELECT 'abc'::dm_email
UPDATE dm_t SET q = 0
UPDATE dm_t SET q = q + 1 WHERE id = 2
SELECT id, q, s, e FROM dm_t ORDER BY id
ALTER DOMAIN dm_pos ADD CONSTRAINT big CHECK (VALUE < 100)
INSERT INTO dm_t VALUES (7, 200, 'a', 'a@b')
ALTER DOMAIN dm_pos ADD CONSTRAINT tiny CHECK (VALUE < 2)
ALTER DOMAIN dm_pos DROP CONSTRAINT big
ALTER DOMAIN dm_pos DROP CONSTRAINT nope
ALTER DOMAIN dm_pos DROP CONSTRAINT IF EXISTS nope
INSERT INTO dm_t VALUES (8, 200, 'a', 'a@b')
ALTER DOMAIN dm_pos SET NOT NULL
ALTER DOMAIN dm_nn SET DEFAULT 'y'
INSERT INTO dm_t (id, q) VALUES (9, 1)
ALTER DOMAIN dm_nn DROP DEFAULT
DROP DOMAIN dm_pos
DROP DOMAIN nosuch
DROP DOMAIN IF EXISTS nosuch
CREATE TABLE dm_u (id int PRIMARY KEY, x dm_pos DEFAULT 7)
INSERT INTO dm_u (id) VALUES (1)
SELECT * FROM dm_u
DROP DOMAIN dm_pos CASCADE
SELECT * FROM dm_t ORDER BY id
SELECT * FROM dm_u
CREATE DOMAIN dm_pos AS int CHECK (VALUE > 0)
CREATE TABLE dm_t (id int PRIMARY KEY, q dm_pos)
SELECT domain_name, data_type FROM information_schema.domains WHERE domain_name LIKE 'dm_%' ORDER BY 1
SELECT column_name, data_type, domain_name FROM information_schema.columns WHERE table_name = 'dm_t' ORDER BY ordinal_position
SELECT typname, typtype, typbasetype = 'int4'::regtype::oid, typnotnull FROM pg_type WHERE typname = 'dm_pos'
SELECT 'dm_pos'::regtype::text, pg_typeof(5::dm_pos)::text
SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'dm_t'::regclass AND attname = 'q'
