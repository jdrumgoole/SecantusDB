# Operators resolve by type at plan time: a comparison across type
# categories has no operator (42883), where a lowered MQL filter would have
# matched nothing and answered an empty result.
SELECT * FROM opt_t WHERE s = 1
SELECT * FROM opt_t WHERE v = 1
SELECT * FROM opt_t WHERE n = 'x'::text
SELECT * FROM opt_t WHERE n = s
SELECT * FROM opt_t WHERE s LIKE 1
SELECT * FROM opt_t WHERE d = 1
SELECT * FROM opt_t WHERE b = 1
SELECT * FROM opt_t WHERE s > 1.5
SELECT * FROM opt_t WHERE n IN ('1'::text)
SELECT * FROM opt_t WHERE s IN (1, 2)
SELECT 1 WHERE 'a'::text = 1
SELECT 1 WHERE 'a'::text ~~ 1
SELECT abs(s) FROM opt_t
SELECT * FROM opt_t WHERE s < n
UPDATE opt_t SET n = 2 WHERE s = 1
DELETE FROM opt_t WHERE d > 5
SELECT * FROM opt_t t1 JOIN opt_t t2 ON t1.s = t2.n
SELECT relname FROM pg_class WHERE relname = 1
# What does resolve: unknown literals take the other side's type, and the
# numeric and date/time families compare among themselves.
SELECT count(*) FROM opt_t WHERE n = '1' AND s = '1' AND d = '2020-01-01' AND b = 't'
SELECT count(*) FROM opt_t WHERE n = 1.5 OR n <> 2::bigint
SELECT 1 FROM opt_t WHERE 1
# A date column against a timestamp: the date promoted to midnight.
SELECT id FROM opt_d WHERE d < now() ORDER BY id
SELECT id FROM opt_d WHERE d < localtimestamp ORDER BY id
SELECT id FROM opt_d WHERE d = '2020-01-01 00:00'::timestamp ORDER BY id
SELECT id FROM opt_d WHERE d < '2020-01-01 00:00:01'::timestamp ORDER BY id
SELECT id FROM opt_d WHERE d <> '2020-01-01 00:00:01'::timestamp ORDER BY id
SELECT id FROM opt_d WHERE d >= '2020-01-01 00:00:01'::timestamp ORDER BY id
SELECT id FROM opt_d WHERE d BETWEEN '2019-01-01'::timestamp AND '2020-01-01 12:00'::timestamp ORDER BY id
SELECT id FROM opt_d WHERE d NOT BETWEEN '2019-01-01'::timestamp AND '2020-01-01 00:00:01'::timestamp ORDER BY id
# An untyped literal against a date column is read as a date first.
SELECT id FROM opt_d WHERE d < '2020-01-01 10:00' ORDER BY id
SELECT id FROM opt_d WHERE d = '2020-01-01 10:00' ORDER BY id
SELECT id FROM opt_d WHERE d IN ('2020-01-01 05:00') ORDER BY id
SELECT id FROM opt_d WHERE d BETWEEN '2020-01-01 01:00' AND '2020-01-02' ORDER BY id
