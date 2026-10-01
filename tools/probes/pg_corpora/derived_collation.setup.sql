DROP TABLE IF EXISTS dc14_t;
DROP COLLATION IF EXISTS dc14_ci;
CREATE COLLATION dc14_ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
CREATE TABLE dc14_t (k text COLLATE dc14_ci);
INSERT INTO dc14_t VALUES ('a'), ('A'), ('b'), ('B'), ('c');
