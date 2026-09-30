# ALTER TABLE ADD COLUMN of a serial column: a sequence, NOT NULL, and every
# existing row numbered.
ALTER TABLE sr ADD COLUMN s serial
SELECT id, s FROM sr ORDER BY id
INSERT INTO sr (id) VALUES (4) RETURNING s
SELECT column_default, is_nullable, data_type FROM information_schema.columns WHERE table_name = 'sr' AND column_name = 's'
SELECT pg_get_serial_sequence('sr', 's')
ALTER TABLE sr ADD COLUMN b bigserial
SELECT id, b, pg_typeof(b) FROM sr ORDER BY id
ALTER TABLE sr ADD COLUMN x smallserial
SELECT pg_get_serial_sequence('sr', 'x')
SELECT id, x FROM sr ORDER BY id
SELECT last_value FROM sr_x_seq
INSERT INTO sr (id, s) VALUES (5, NULL)
ALTER TABLE sr ADD COLUMN IF NOT EXISTS s serial
DROP TABLE sr
SELECT relname FROM pg_class WHERE relname LIKE 'sr\_%' ORDER BY relname
