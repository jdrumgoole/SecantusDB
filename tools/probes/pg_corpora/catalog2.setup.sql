DROP TABLE IF EXISTS cc_child
DROP TABLE IF EXISTS cc_parent
DROP SEQUENCE IF EXISTS cc_seq
CREATE SEQUENCE cc_seq START 5 INCREMENT 2 MINVALUE 1 MAXVALUE 99 CYCLE
CREATE TABLE cc_parent (id int PRIMARY KEY, code text UNIQUE, amount numeric(12,3), label varchar(20), flag bool NOT NULL DEFAULT false, note text)
CREATE TABLE cc_child (cid serial PRIMARY KEY, pid int REFERENCES cc_parent(id), tag text CHECK (tag <> ''))
