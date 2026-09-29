DROP TABLE IF EXISTS tg_a
DROP TABLE IF EXISTS tg_b
DROP TABLE IF EXISTS tg_log
DROP FUNCTION IF EXISTS tg_args() CASCADE
DROP FUNCTION IF EXISTS tg_fail() CASCADE
DROP FUNCTION IF EXISTS tg_ts() CASCADE
DROP FUNCTION IF EXISTS tg_cnt() CASCADE
DROP FUNCTION IF EXISTS tg_cascade() CASCADE
CREATE TABLE tg_a (id int PRIMARY KEY, v text, amt numeric(10,2), at timestamp, flag boolean)
CREATE TABLE tg_b (id int PRIMARY KEY, a_id int, note text)
CREATE TABLE tg_log (seq serial PRIMARY KEY, msg text)
CREATE FUNCTION tg_args() RETURNS trigger AS $$ BEGIN INSERT INTO tg_log(msg) VALUES (TG_NAME || ':' || TG_NARGS || ':' || coalesce(TG_ARGV[0], '-') || ':' || coalesce(TG_ARGV[1], '-')); RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tg_fail() RETURNS trigger AS $$ BEGIN IF NEW.v = 'bad' THEN RAISE EXCEPTION 'bad value for id %', NEW.id USING ERRCODE = '22023'; END IF; RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tg_ts() RETURNS trigger AS $$ BEGIN NEW.at := '2026-01-02 03:04:05.123456'::timestamp; NEW.amt := NEW.amt * 2; NEW.flag := NEW.amt > 10; RETURN NEW; END $$ LANGUAGE plpgsql
CREATE FUNCTION tg_cnt() RETURNS trigger AS $$ DECLARE c int; BEGIN SELECT count(*) INTO c FROM tg_a; INSERT INTO tg_log(msg) VALUES (TG_OP || ' ' || TG_WHEN || ' ' || TG_LEVEL || ' count=' || c); RETURN NULL; END $$ LANGUAGE plpgsql
CREATE FUNCTION tg_cascade() RETURNS trigger AS $$ BEGIN DELETE FROM tg_b WHERE a_id = OLD.id; RETURN OLD; END $$ LANGUAGE plpgsql
