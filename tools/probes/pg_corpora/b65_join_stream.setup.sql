DROP TABLE IF EXISTS b65_ja CASCADE
DROP TABLE IF EXISTS b65_jb CASCADE
DROP TABLE IF EXISTS b65_jc CASCADE
CREATE TABLE b65_ja (id int PRIMARY KEY, k int, g int, t text, f float8)
CREATE TABLE b65_jb (id int PRIMARY KEY, name text, k bigint, v numeric)
CREATE TABLE b65_jc (bid int, tag text)
INSERT INTO b65_ja SELECT g, g % 7, g % 3, 'r' || (g % 5), g / 2.0 FROM generate_series(1, 600) g
INSERT INTO b65_ja VALUES (601, NULL, 1, NULL, NULL), (602, NULL, 2, 'x', 1.5)
INSERT INTO b65_jb SELECT g, 'n' || g, g, g * 1.5 FROM generate_series(0, 5) g
INSERT INTO b65_jb VALUES (100, NULL, NULL, NULL), (101, 'dup', 3, 2.25)
INSERT INTO b65_jc VALUES (1, 'a'), (1, 'b'), (3, 'c'), (NULL, 'd'), (101, 'e')
