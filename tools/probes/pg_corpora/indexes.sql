# --- plain and named
CREATE INDEX ix1_a ON ix1 (a)
CREATE INDEX ON ix1 (b)
CREATE INDEX ON ix1 (b)
CREATE INDEX ON ix1 (a, b)
CREATE INDEX ix1_desc ON ix1 (c DESC)
CREATE INDEX ix1_part ON ix1 (a) WHERE c > 1
CREATE INDEX ix1_part2 ON ix1 (a) WHERE b = 'x'
CREATE INDEX ix1_part3 ON ix1 (a) WHERE b IS NOT NULL AND c < 4
CREATE INDEX ix1_inc ON ix1 (a) INCLUDE (b)
CREATE INDEX ix1_hash ON ix1 USING hash (b)
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'ix1' ORDER BY indexname
# --- the answers do not change
SELECT id FROM ix1 WHERE a = 10 ORDER BY id
SELECT id FROM ix1 WHERE a = 10 AND c > 1 ORDER BY id
SELECT id FROM ix1 WHERE a IS NULL ORDER BY id
SELECT id FROM ix1 WHERE b = 'x' ORDER BY id
SELECT id FROM ix1 ORDER BY c DESC
SELECT id FROM ix1 WHERE a > 5 ORDER BY a, id
# --- collisions
CREATE INDEX ix1_a ON ix1 (b)
CREATE INDEX IF NOT EXISTS ix1_a ON ix1 (b)
CREATE INDEX ix1 ON ix2 (s)
CREATE INDEX ixbad ON ix1 (nosuch)
CREATE INDEX ixbad ON nosuch (a)
CREATE TABLE ix1_a (x int)
# --- unique
CREATE UNIQUE INDEX ix2_s ON ix2 (s)
CREATE UNIQUE INDEX ix1_b_u ON ix1 (b)
CREATE UNIQUE INDEX ix1_c_u ON ix1 (c)
INSERT INTO ix1 VALUES (5, 1, 'z', 1, NULL)
UPDATE ix1 SET c = 2 WHERE id = 1
CREATE UNIQUE INDEX ix1_u_part ON ix1 (a) WHERE c > 2
INSERT INTO ix1 VALUES (6, 10, 'z', 6, NULL)
INSERT INTO ix1 VALUES (7, 10, 'z', 1, NULL)
CREATE UNIQUE INDEX ix2_nulls ON ix2 (id, s)
INSERT INTO ix2 VALUES (3, NULL), (4, NULL)
# --- drop
DROP INDEX ix1_a
DROP INDEX ix1_a
DROP INDEX IF EXISTS ix1_a
DROP INDEX ix1_u_key
DROP INDEX ix1
DROP INDEX ix1_desc, ix1_part
SELECT indexname FROM pg_indexes WHERE tablename = 'ix1' ORDER BY indexname
# --- transactional
BEGIN
CREATE INDEX ix1_txn ON ix1 (c)
ROLLBACK
SELECT count(*) FROM pg_indexes WHERE indexname = 'ix1_txn'
DROP TABLE ix2
SELECT count(*) FROM pg_indexes WHERE tablename = 'ix2'
CREATE INDEX ix1_nulls ON ix1 (a NULLS FIRST)
CREATE INDEX ix1_expr ON ix1 (lower(b))
