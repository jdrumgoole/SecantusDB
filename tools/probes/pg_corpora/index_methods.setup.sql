DROP EXTENSION IF EXISTS btree_gin CASCADE
DROP EXTENSION IF EXISTS btree_gist CASCADE
DROP TABLE IF EXISTS im
CREATE TABLE im (id int PRIMARY KEY, j jsonb, tags text[], tv tsvector, r int4range, t text, n int)
INSERT INTO im VALUES (1, '{"a": 1}', '{x,y}', 'a b', '[1,5)', 'hello', 3), (2, '{"a": 2}', '{y}', 'c', '[2,9)', 'world', 4)
