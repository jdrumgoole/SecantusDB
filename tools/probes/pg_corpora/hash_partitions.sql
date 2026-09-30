# reference-version: 15
# PARTITION BY HASH: every row lands in the partition PostgreSQL's hash
# puts it in.
SELECT tableoid::regclass::text, count(*) FROM hp_i GROUP BY 1 ORDER BY 1
SELECT id FROM hp_i0 ORDER BY id
SELECT id FROM hp_i3 ORDER BY id
SELECT tableoid::regclass::text, k FROM hp_t ORDER BY k
SELECT tableoid::regclass::text, count(*) FROM hp_m GROUP BY 1 ORDER BY 1
SELECT a FROM hp_m1 ORDER BY a
SELECT pg_get_expr(relpartbound, oid) FROM pg_class WHERE relname = 'hp_i2'
SELECT partstrat FROM pg_partitioned_table WHERE partrelid = 'hp_i'::regclass
CREATE TABLE hp_i4 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 1)
CREATE TABLE hp_i5 PARTITION OF hp_i FOR VALUES WITH (modulus 3, remainder 1)
CREATE TABLE hp_i6 PARTITION OF hp_i FOR VALUES WITH (modulus 0, remainder 0)
CREATE TABLE hp_i7 PARTITION OF hp_i FOR VALUES WITH (modulus 4, remainder 4)
CREATE TABLE hp_i8 PARTITION OF hp_i DEFAULT
CREATE TABLE hp_i9 PARTITION OF hp_i FOR VALUES IN (1)
UPDATE hp_i SET id = id + 1 WHERE id = 5
SELECT tableoid::regclass::text FROM hp_i WHERE id = 6 ORDER BY 1
DELETE FROM hp_i WHERE id BETWEEN 0 AND 10
SELECT count(*) FROM hp_i
