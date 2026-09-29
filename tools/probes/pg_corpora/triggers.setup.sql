DROP TABLE IF EXISTS tr_t
DROP TABLE IF EXISTS tr_log
DROP FUNCTION IF EXISTS tr_upper() CASCADE
DROP FUNCTION IF EXISTS tr_audit() CASCADE
DROP FUNCTION IF EXISTS tr_skip() CASCADE
DROP FUNCTION IF EXISTS tr_stamp() CASCADE
DROP FUNCTION IF EXISTS tr_guard() CASCADE
DROP FUNCTION IF EXISTS tr_stmt() CASCADE
DROP FUNCTION IF EXISTS tr_notrig() CASCADE
CREATE TABLE tr_t (id int PRIMARY KEY, name text, n int, touched int DEFAULT 0)
CREATE TABLE tr_log (seq serial PRIMARY KEY, op text, tname text, lvl text, whn text, old_id int, new_id int, info text)
CREATE FUNCTION tr_upper() RETURNS trigger AS $$ BEGIN NEW.name := upper(NEW.name); RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_audit() RETURNS trigger AS $$ BEGIN IF TG_OP = 'DELETE' THEN INSERT INTO tr_log(op, tname, lvl, whn, old_id) VALUES (TG_OP, TG_TABLE_NAME, TG_LEVEL, TG_WHEN, OLD.id); RETURN OLD; ELSIF TG_OP = 'UPDATE' THEN INSERT INTO tr_log(op, tname, lvl, whn, old_id, new_id, info) VALUES (TG_OP, TG_TABLE_NAME, TG_LEVEL, TG_WHEN, OLD.id, NEW.id, OLD.name || '->' || NEW.name); RETURN NEW; ELSE INSERT INTO tr_log(op, tname, lvl, whn, new_id) VALUES (TG_OP, TG_TABLE_NAME, TG_LEVEL, TG_WHEN, NEW.id); RETURN NEW; END IF; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_skip() RETURNS trigger AS $$ BEGIN IF NEW.n < 0 THEN RETURN NULL; END IF; RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_stamp() RETURNS trigger AS $$ BEGIN NEW.touched := OLD.touched + 1; RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_guard() RETURNS trigger AS $$ BEGIN IF OLD.n > 100 THEN RAISE EXCEPTION 'row % is protected', OLD.id; END IF; RETURN OLD; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_stmt() RETURNS trigger AS $$ BEGIN INSERT INTO tr_log(op, tname, lvl, whn) VALUES (TG_OP, TG_TABLE_NAME, TG_LEVEL, TG_WHEN); RETURN NULL; END $$ LANGUAGE plpgsql
CREATE FUNCTION tr_notrig() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql
