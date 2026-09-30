DROP VIEW IF EXISTS io_v
DROP TABLE IF EXISTS io_t
DROP TABLE IF EXISTS io_log
CREATE TABLE io_t (id int PRIMARY KEY, name text)
CREATE TABLE io_log (msg text)
INSERT INTO io_t VALUES (1, 'a'), (2, 'b')
CREATE VIEW io_v AS SELECT id, upper(name) AS uname FROM io_t
CREATE OR REPLACE FUNCTION io_f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF TG_OP = 'INSERT' THEN INSERT INTO io_t VALUES (NEW.id, lower(NEW.uname)); INSERT INTO io_log VALUES ('ins ' || NEW.id); RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE io_t SET name = lower(NEW.uname) WHERE id = OLD.id; INSERT INTO io_log VALUES ('upd ' || OLD.id || ' ' || NEW.uname); RETURN NEW; ELSE IF OLD.id = 99 THEN RETURN NULL; END IF; DELETE FROM io_t WHERE id = OLD.id; INSERT INTO io_log VALUES ('del ' || OLD.id); RETURN OLD; END IF; END $$
