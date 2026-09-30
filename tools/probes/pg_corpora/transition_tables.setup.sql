DROP TABLE IF EXISTS tr_t
DROP TABLE IF EXISTS tr_log
CREATE TABLE tr_t (id int PRIMARY KEY, n int)
CREATE TABLE tr_log (msg text)
CREATE OR REPLACE FUNCTION tr_ins() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO tr_log SELECT 'ins ' || count(*) || ' sum ' || coalesce(sum(n), 0) FROM nt; RETURN NULL; END $$
CREATE OR REPLACE FUNCTION tr_upd() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO tr_log SELECT 'upd ' || o.id || ' ' || o.n || '->' || n.n FROM ot o JOIN nt n ON n.id = o.id ORDER BY o.id; RETURN NULL; END $$
CREATE OR REPLACE FUNCTION tr_del() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO tr_log SELECT 'del ' || string_agg(id::text, ',' ORDER BY id) FROM ot; RETURN NULL; END $$
