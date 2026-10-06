DROP TABLE IF EXISTS b65_r CASCADE
CREATE TABLE b65_r (v int, t text, a int, b int, d date, j jsonb, n numeric)
INSERT INTO b65_r VALUES (1, 'a', 1, 0, '2020-01-01', '{}', 1.5), (2, 'b', 2, 0, '2020-01-02', '[]', 2.5)
DROP TABLE IF EXISTS b65_r0 CASCADE
CREATE TABLE b65_r0 (v int, t text, a int, b int)
DROP SEQUENCE IF EXISTS b65_seq
CREATE SEQUENCE b65_seq
