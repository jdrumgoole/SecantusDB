# reference-version: 15
# --- two same-named tables in different schemas
CREATE TABLE sc34_a.sc34_users (user_id serial PRIMARY KEY, name text)
INSERT INTO sc34_a.sc34_users (name) VALUES ('alice'), ('bob'), ('carol')
SELECT count(*) FROM sc34_users
SELECT count(*) FROM sc34_a.sc34_users
SELECT sc34_a.sc34_users.name FROM sc34_a.sc34_users ORDER BY user_id
SELECT u.name FROM sc34_a.sc34_users u WHERE u.user_id = 2
SELECT p.x, a.name FROM sc34_users p JOIN sc34_a.sc34_users a ON a.user_id = p.user_id ORDER BY 1
SELECT name FROM sc34_a.sc34_users WHERE user_id IN (SELECT user_id FROM sc34_users) ORDER BY 1
UPDATE sc34_a.sc34_users SET name = 'BOB' WHERE user_id = 2 RETURNING user_id, name
DELETE FROM sc34_a.sc34_users WHERE user_id = 3 RETURNING name
INSERT INTO sc34_a.sc34_users (user_id, name) VALUES (1, 'x') ON CONFLICT (user_id) DO UPDATE SET name = 'ALICE' RETURNING name
SELECT pg_get_serial_sequence('sc34_a.sc34_users', 'user_id')
SELECT nextval('sc34_a.sc34_users_user_id_seq') > 0
# --- a create into a missing schema, a missing relation
CREATE TABLE sc34_nope.t (a int)
SELECT * FROM sc34_nope.t
SELECT * FROM sc34_a.sc34_missing
SELECT * FROM sc34_missing
# --- search_path resolution
CREATE TABLE sc34_b.sc34_only_b (v int)
INSERT INTO sc34_b.sc34_only_b VALUES (7)
SELECT * FROM sc34_only_b
SET search_path TO sc34_b, public
SHOW search_path
SELECT * FROM sc34_only_b
SELECT count(*) FROM sc34_users
CREATE TABLE sc34_unq (k int)
SELECT schemaname, tablename FROM pg_tables WHERE tablename = 'sc34_unq'
SET search_path TO sc34_a, public
SELECT count(*) FROM sc34_users
SET search_path TO public, sc34_a
SELECT count(*) FROM sc34_users
SET search_path TO "$user", public
# --- FK, index, view, truncate, alter
CREATE TABLE sc34_a.sc34_orders (id int PRIMARY KEY, uid int REFERENCES sc34_a.sc34_users (user_id))
INSERT INTO sc34_a.sc34_orders VALUES (1, 1)
INSERT INTO sc34_a.sc34_orders VALUES (2, 99)
CREATE INDEX sc34_ix ON sc34_a.sc34_orders (uid)
SELECT schemaname, tablename, indexname FROM pg_indexes WHERE indexname = 'sc34_ix'
CREATE VIEW sc34_a.sc34_v AS SELECT name FROM sc34_a.sc34_users
SELECT * FROM sc34_a.sc34_v ORDER BY 1
ALTER TABLE sc34_a.sc34_orders ADD COLUMN note text
SELECT column_name FROM information_schema.columns WHERE table_schema = 'sc34_a' AND table_name = 'sc34_orders' ORDER BY ordinal_position
TRUNCATE sc34_a.sc34_orders
SELECT count(*) FROM sc34_a.sc34_orders
# --- more DDL inside a schema
CREATE TABLE sc34_a.sc34_users (user_id int)
CREATE TABLE sc34_a.sc34_k (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, code text UNIQUE, CHECK (length(code) > 0))
INSERT INTO sc34_a.sc34_k (code) VALUES ('a'), ('b')
INSERT INTO sc34_a.sc34_k (code) VALUES ('a')
INSERT INTO sc34_a.sc34_k (code) VALUES ('')
SELECT sc34_a.sc34_k.* FROM sc34_a.sc34_k ORDER BY id
SELECT c.conname, n.nspname, c.contype FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace WHERE c.conrelid = 'sc34_a.sc34_k'::regclass ORDER BY 1
SELECT c.oid IS NOT NULL, n.nspname, c.relname FROM pg_catalog.pg_class c LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE c.relname OPERATOR(pg_catalog.~) '^(sc34_k)$' AND n.nspname OPERATOR(pg_catalog.~) '^(sc34_a)$'
CREATE SEQUENCE sc34_a.sc34_seq START 5
SELECT nextval('sc34_a.sc34_seq')
SELECT sequence_schema, sequence_name FROM information_schema.sequences WHERE sequence_name LIKE 'sc34%' ORDER BY 1, 2
SELECT schemaname, viewname FROM pg_views WHERE viewname LIKE 'sc34%'
ALTER TABLE sc34_a.sc34_k RENAME TO sc34_k2
SELECT count(*) FROM sc34_a.sc34_k2
SELECT table_schema, table_name FROM information_schema.tables WHERE table_name LIKE 'sc34_k%'
DROP TABLE sc34_a.sc34_nothere
DROP TABLE IF EXISTS sc34_a.sc34_nothere
# --- catalog views
SELECT table_schema, table_name FROM information_schema.tables WHERE table_name LIKE 'sc34%' ORDER BY 1, 2
SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relname = 'sc34_users' ORDER BY 1
SELECT schemaname, tablename FROM pg_tables WHERE tablename LIKE 'sc34%' ORDER BY 1, 2
SELECT 'sc34_a.sc34_users'::regclass::text
SELECT to_regclass('sc34_a.sc34_users') IS NOT NULL, to_regclass('sc34_b.sc34_users') IS NULL
# --- DROP SCHEMA
DROP SCHEMA sc34_b
CREATE SCHEMA IF NOT EXISTS sc34_b
DROP SCHEMA sc34_b CASCADE
SELECT * FROM sc34_b.sc34_only_b
DROP TABLE sc34_a.sc34_orders
DROP VIEW sc34_a.sc34_v
DROP TABLE sc34_a.sc34_users
SELECT count(*) FROM sc34_users
# --- clean up, so the reference server is left as it was found
DROP SCHEMA sc34_a CASCADE
DROP TABLE sc34_users
DROP TABLE sc34_unq
