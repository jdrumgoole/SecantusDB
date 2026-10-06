DROP TABLE IF EXISTS b66_u CASCADE
CREATE TABLE b66_u (a int, b int, g int, t text)
INSERT INTO b66_u VALUES (1, 0, 1, 'x'), (2, 0, 1, 'y'), (3, 0, 2, 'z')
DROP SEQUENCE IF EXISTS b66_useq
CREATE SEQUENCE b66_useq
