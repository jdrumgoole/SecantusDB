SELECT * FROM cpk ORDER BY a, b
SELECT v FROM cpk WHERE a = 1 AND b = 'y'
SELECT a, b FROM cpk WHERE a = 1 ORDER BY b
INSERT INTO cpk VALUES (1, 'x', 99)
INSERT INTO cpk (b, a, v) VALUES ('x', 1, 99)
INSERT INTO cpk (b, a, v) VALUES ('z', 3, 40) RETURNING a, b, v
INSERT INTO cpk VALUES (NULL, 'q', 1)
UPDATE cpk SET v = v + 1 WHERE a = 1 RETURNING a, b, v
DELETE FROM cpk WHERE a = 2 AND b = 'x' RETURNING *
SELECT count(*) FROM cpk
INSERT INTO cpk VALUES (1, 'x', 5) ON CONFLICT (a, b) DO UPDATE SET v = excluded.v RETURNING *
INSERT INTO cpk VALUES (1, 'x', 6) ON CONFLICT DO NOTHING
SELECT a, b, v FROM cpk ORDER BY a DESC, b DESC LIMIT 2
SELECT c.a, count(*) FROM cpk c GROUP BY c.a ORDER BY 1
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'cpk'
SELECT constraint_name, constraint_type FROM information_schema.table_constraints WHERE table_name = 'cpk' AND constraint_type = 'PRIMARY KEY'
SELECT column_name, ordinal_position FROM information_schema.key_column_usage WHERE table_name = 'cpk' ORDER BY ordinal_position
CREATE TABLE cpk_child (id int PRIMARY KEY, a int REFERENCES cpk (a))
