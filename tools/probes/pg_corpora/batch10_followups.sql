# Batch 10 follow-ups, each a silent wrong answer before.
# ON UPDATE CASCADE checks the child's OTHER foreign keys; two unnamed
# foreign keys over one column are numbered apart.
UPDATE fu_p1 SET k = 3 WHERE k = 1
SELECT * FROM fu_ch
SELECT conname FROM pg_constraint WHERE conrelid = 'fu_ch'::regclass ORDER BY 1
ALTER TABLE fu_ch ADD FOREIGN KEY (a) REFERENCES fu_p2(k)
ALTER TABLE fu_ch ADD CONSTRAINT fu_ch_a_fkey FOREIGN KEY (a) REFERENCES fu_p2(k)
SELECT conname FROM pg_constraint WHERE conrelid = 'fu_ch'::regclass ORDER BY 1
# A RANGE frame over int8 beyond 2^53 compares exactly.
SELECT k, count(*) OVER (ORDER BY k RANGE BETWEEN 0 PRECEDING AND 0 FOLLOWING) FROM fu_w ORDER BY k
SELECT k, count(*) OVER (ORDER BY k RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM fu_w ORDER BY k
# A timestamp(p) column stores the value rounded to p digits.
SELECT t::text, z::text FROM fu_m
UPDATE fu_m SET t = '2021-06-01 10:00:00.9996'
SELECT t::text FROM fu_m
# A stored timestamptz cast to timestamp is the session zone's wall clock,
# in a projection, a WHERE, a nested cast and over an aggregate.
SET timezone = 'Europe/Berlin'
SELECT z::timestamp, z::date FROM fu_z ORDER BY z
SELECT (z::timestamp)::text FROM fu_z ORDER BY 1
SELECT count(*) FROM fu_z WHERE z::timestamp = '2030-01-01 11:00'
SELECT max(z)::timestamp FROM fu_z
RESET timezone
# A cast of NULL to a type that does not exist is 42704.
SELECT null::fu_no_such_type
SELECT null::fu_no_such_type[]
SELECT null::int4, null::text[]
# A wide numeric key changed by UPDATE onto an equal value is 23505.
UPDATE fu_n SET n = 123456789012345678901234567890123456789012.0 WHERE n = 1
SELECT count(*) FROM fu_n
DROP TABLE fu_ch, fu_p1, fu_p2, fu_w, fu_m, fu_z, fu_n
