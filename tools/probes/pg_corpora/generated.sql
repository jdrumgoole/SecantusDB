CREATE TABLE gc (id int PRIMARY KEY, a int, b int GENERATED ALWAYS AS (a * 2) STORED, c text GENERATED ALWAYS AS (upper('x' || a::text)) STORED)
INSERT INTO gc (id, a) VALUES (1, 3)
INSERT INTO gc VALUES (2, 4, 5)
INSERT INTO gc VALUES (2, 4, DEFAULT)
INSERT INTO gc (id, a, c) VALUES (3, 1, 'no')
UPDATE gc SET a = 10 WHERE id = 1
UPDATE gc SET b = 1
UPDATE gc SET b = DEFAULT, a = a + 1 WHERE id = 2
SELECT * FROM gc ORDER BY id
INSERT INTO gc (id, a) VALUES (4, NULL)
SELECT b, c FROM gc WHERE id = 4
SELECT attname, attgenerated FROM pg_attribute WHERE attrelid = 'gc'::regclass AND attnum > 0 ORDER BY attnum
SELECT column_name, is_generated, generation_expression FROM information_schema.columns WHERE table_name = 'gc' ORDER BY ordinal_position
SELECT pg_get_expr(adbin, adrelid) FROM pg_attrdef WHERE adrelid = 'gc'::regclass ORDER BY adnum
INSERT INTO gc (id, a) SELECT 5, 7
SELECT b FROM gc WHERE id = 5
