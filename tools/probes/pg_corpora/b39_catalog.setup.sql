DROP TABLE IF EXISTS b39_fk
DROP TABLE IF EXISTS b39_pk
DROP TABLE IF EXISTS b39_ix
DROP TABLE IF EXISTS b39_pki
DROP TABLE IF EXISTS b39_err
DROP DOMAIN IF EXISTS b39_dom
DROP TYPE IF EXISTS b39_s.b39_ct
DROP SCHEMA IF EXISTS b39_s
CREATE TABLE b39_pk (a int, b int, PRIMARY KEY (a, b))
CREATE TABLE b39_fk (c int, d int, CONSTRAINT b39_fk_pk FOREIGN KEY (c, d) REFERENCES b39_pk (b, a))
CREATE TABLE b39_ix (id int, name text, colour text, quest text)
CREATE INDEX b39_ix_id ON b39_ix (id)
CREATE INDEX b39_ix_fsingle ON b39_ix (upper(colour))
CREATE UNIQUE INDEX b39_ix_un ON b39_ix (id)
CREATE INDEX b39_ix_fmulti ON b39_ix (upper(colour), upper(quest))
CREATE INDEX b39_ix_fmixed ON b39_ix (colour, upper(quest))
CREATE INDEX b39_ix_partial ON b39_ix (name) WHERE id > 5
CREATE TABLE b39_pki (a int, b int, c int, d int)
CREATE UNIQUE INDEX b39_pki_pkey ON b39_pki (b, d) INCLUDE (a)
CREATE DOMAIN b39_dom AS int8
CREATE SCHEMA b39_s
CREATE TYPE b39_s.b39_ct AS (i int8)
CREATE TABLE b39_err (id int NOT NULL, v int)
DROP FUNCTION IF EXISTS b39_rec(int)
CREATE FUNCTION b39_rec(IN a int, OUT b int, OUT c text) AS 'BEGIN b := a; c := ''x''; END' LANGUAGE plpgsql
