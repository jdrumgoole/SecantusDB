CREATE EXTENSION IF NOT EXISTS pg_trgm
DROP TABLE IF EXISTS tg
CREATE TABLE tg (id int, w text)
INSERT INTO tg VALUES (1, 'cat'), (2, 'cats'), (3, 'dog'), (4, 'category'), (5, 'catalogue'), (6, NULL)
DROP INDEX IF EXISTS tg_g
