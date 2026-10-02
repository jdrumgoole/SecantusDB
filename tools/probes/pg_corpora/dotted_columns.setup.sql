DROP TABLE IF EXISTS b36_d
DROP TABLE IF EXISTS b36_e
CREATE TABLE b36_d (id int PRIMARY KEY, "dot.s" text, "$x" int, "sp ace" text, "a.b.c" int)
INSERT INTO b36_d VALUES (1, 'p', 10, 's1', 100), (2, 'q', 20, 's2', 200), (3, 'p', 30, 's3', 300)
CREATE TABLE b36_e (k int PRIMARY KEY, "dot.s" text UNIQUE, "$y" int)
INSERT INTO b36_e VALUES (1, 'p', 7), (2, 'q', 8)
