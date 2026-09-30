# Partition keys over expressions, and a multi-column key declared in an
# order different from the table's.
INSERT INTO xpe_l VALUES (1, 'A'), (2, 'b'), (3, 'C')
INSERT INTO xpe_l VALUES (4, 'D')
SELECT tableoid::regclass::text, id, name FROM xpe_l ORDER BY id
INSERT INTO xpe_r VALUES (1, NULL), (7, NULL), (13, NULL), (28, NULL)
SELECT tableoid::regclass::text, id FROM xpe_r ORDER BY id
INSERT INTO xpe_r VALUES (-3, NULL)
INSERT INTO xpe_o VALUES (100, 5), (1, 15), (0, 10)
SELECT tableoid::regclass::text, a, b FROM xpe_o ORDER BY a, b
INSERT INTO xpe_o VALUES (1, 25)
SELECT partattrs::text FROM pg_partitioned_table WHERE partrelid IN ('xpe_l'::regclass, 'xpe_o'::regclass) ORDER BY partrelid
UPDATE xpe_l SET name = 'c' WHERE id = 1
SELECT tableoid::regclass::text, id FROM xpe_l ORDER BY id
SELECT id FROM xpe_l_c ORDER BY id
DROP TABLE xpe_l
DROP TABLE xpe_r
DROP TABLE xpe_o
