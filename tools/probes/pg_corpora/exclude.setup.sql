CREATE EXTENSION IF NOT EXISTS btree_gist
DROP TABLE IF EXISTS ex_b
DROP TABLE IF EXISTS ex_e
DROP TABLE IF EXISTS ex_r
CREATE TABLE ex_b (id int PRIMARY KEY, room int, during tsrange, EXCLUDE USING gist (room WITH =, during WITH &&))
CREATE TABLE ex_e (id int, EXCLUDE USING btree (id WITH =))
CREATE TABLE ex_r (id int PRIMARY KEY, r int4range, CONSTRAINT no_overlap EXCLUDE USING gist (r WITH &&))
