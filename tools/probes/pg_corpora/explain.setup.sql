DROP TABLE IF EXISTS exq
CREATE TABLE exq (id int PRIMARY KEY, n int, g text)
CREATE INDEX exq_n_idx ON exq (n)
INSERT INTO exq VALUES (1, 1, 'a'), (2, 2, 'b')
