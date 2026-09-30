# The catalog functions and relations psql and ORMs read.
SELECT pg_get_constraintdef(oid), pg_get_constraintdef(oid, true) FROM pg_constraint WHERE conrelid = 'cfx_c'::regclass ORDER BY conname
SELECT pg_get_indexdef('cfx_c_v'::regclass), pg_get_indexdef('cfx_c_v'::regclass, 0, true), pg_get_indexdef('cfx_c_v'::regclass, 1, true), pg_get_indexdef('cfx_c_v'::regclass, 3, true)
SELECT pg_get_userbyid(6171), pg_table_is_visible('cfx_c'::regclass), pg_type_is_visible(23)
SELECT relname, relhastriggers, relchecks, relreplident FROM pg_class WHERE relname IN ('cfx_c', 'cfx_p', 'cfx_c_v', 'cfx_s') ORDER BY relname
SELECT seqtypid::regtype, seqstart, seqincrement FROM pg_sequence WHERE seqrelid = 'cfx_s'::regclass
SELECT nextval('cfx_s'::regclass), currval('cfx_s'::regclass), 'cfx_s'::regclass::text
SELECT collname FROM pg_collation WHERE oid = 100
SELECT amname FROM pg_am WHERE oid = 403
# An aggregate whose WHERE compares two columns, over a join.
SELECT count(*) FROM cfx_a a, cfx_b b WHERE a.id = b.id
SELECT a.id, sum(b.w) FROM cfx_a a, cfx_b b WHERE a.id = b.id GROUP BY a.id ORDER BY a.id
SELECT a.id, count(*) FROM cfx_a a JOIN cfx_b b ON true WHERE a.id = b.id AND b.w > 6 GROUP BY a.id HAVING count(*) > 1
# A subquery correlated through a FROM function's argument.
SELECT id, array(SELECT 'x' || y FROM unnest(cfx_a.opts) y) FROM cfx_a ORDER BY id
SELECT a.id, array_to_string(a.opts || array(SELECT 't.' || y FROM unnest(b.opts) y), ', ') FROM cfx_a a LEFT JOIN cfx_b b ON a.id = b.id AND b.w = 10 ORDER BY a.id
SELECT id, (SELECT count(*) FROM unnest(cfx_a.opts) u) FROM cfx_a ORDER BY id
DROP TABLE cfx_c
DROP TABLE cfx_p
DROP SEQUENCE cfx_s
DROP TABLE cfx_a
DROP TABLE cfx_b
