INSERT INTO pt VALUES (1, '2023-05-01', 'a'), (2, '2024-02-02', 'b')
SELECT * FROM pt ORDER BY id
SELECT * FROM pt_2023
SELECT * FROM pt_2024
INSERT INTO pt VALUES (3, '2030-01-01', 'c')
INSERT INTO pt_2023 VALUES (4, '2024-06-01', 'd')
INSERT INTO pt_2023 VALUES (5, '2023-06-01', 'e')
SELECT id FROM pt ORDER BY id
UPDATE pt SET d = '2024-03-03' WHERE id = 1
SELECT id FROM pt_2024 ORDER BY id
SELECT id FROM pt_2023 ORDER BY id
DELETE FROM pt WHERE id = 2
SELECT count(*) FROM pt
INSERT INTO pl VALUES (1, 'eu'), (2, 'us'), (3, 'uk')
SELECT id FROM pl_eu ORDER BY id
SELECT id FROM pl_other ORDER BY id
SELECT tableoid::regclass, id FROM pl ORDER BY id
SELECT relname, relkind, relispartition FROM pg_class WHERE relname LIKE 'pt%' ORDER BY 1
SELECT c.relname, pg_get_expr(c.relpartbound, c.oid) FROM pg_class c WHERE c.relispartition AND c.relname LIKE 'p%' ORDER BY 1
SELECT inhrelid::regclass::text, inhparent::regclass::text FROM pg_inherits WHERE inhparent::regclass::text IN ('pt', 'pl') ORDER BY 1
SELECT partstrat, partnatts FROM pg_partitioned_table p JOIN pg_class c ON c.oid = p.partrelid WHERE c.relname = 'pt'
CREATE TABLE pt_bad PARTITION OF pt FOR VALUES FROM ('2023-06-01') TO ('2023-07-01')
TRUNCATE pt
SELECT count(*) FROM pt_2024
ALTER TABLE pt DETACH PARTITION pt_2024
SELECT count(*) FROM pt
CREATE TABLE pt_2025 (id int, d date, v text)
ALTER TABLE pt ATTACH PARTITION pt_2025 FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')
INSERT INTO pt VALUES (9, '2025-05-05', 'z')
SELECT id FROM pt_2025
DROP TABLE pt
SELECT count(*) FROM pg_class WHERE relname = 'pt_2023'
SELECT tableoid::regclass, count(*) FROM pl GROUP BY 1 ORDER BY 1
SELECT count(*) FROM pl WHERE tableoid = 'pl_eu'::regclass
CREATE TABLE plain_t (x int)
INSERT INTO plain_t VALUES (1)
SELECT tableoid::regclass, x FROM plain_t
SELECT p.tableoid::regclass, p.id FROM pl p WHERE p.id = 2
DROP TABLE plain_t
