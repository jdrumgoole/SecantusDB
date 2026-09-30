# Partition keys over expressions, and a multi-column key declared in an
# order different from the table's.
INSERT INTO pe_l VALUES (1, 'A'), (2, 'b'), (3, 'C')
INSERT INTO pe_l VALUES (4, 'D')
SELECT tableoid::regclass::text, id, name FROM pe_l ORDER BY id
INSERT INTO pe_r VALUES (1, NULL), (7, NULL), (13, NULL), (28, NULL)
SELECT tableoid::regclass::text, id FROM pe_r ORDER BY id
INSERT INTO pe_r VALUES (-3, NULL)
INSERT INTO pe_o VALUES (100, 5), (1, 15), (0, 10)
SELECT tableoid::regclass::text, a, b FROM pe_o ORDER BY a, b
INSERT INTO pe_o VALUES (1, 25)
SELECT partattrs::text FROM pg_partitioned_table WHERE partrelid IN ('pe_l'::regclass, 'pe_o'::regclass) ORDER BY partrelid
UPDATE pe_l SET name = 'c' WHERE id = 1
SELECT tableoid::regclass::text, id FROM pe_l ORDER BY id
SELECT id FROM pe_l_c ORDER BY id
