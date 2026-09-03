DROP TABLE IF EXISTS cat10
CREATE TABLE cat10 (id int PRIMARY KEY, n int NOT NULL, s text DEFAULT 'x', c numeric(10,2))
CREATE INDEX cat10_n ON cat10 (n)
INSERT INTO cat10 VALUES (1, 1, 'a', 1.5)
