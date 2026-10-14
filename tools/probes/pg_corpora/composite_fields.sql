# reference-version: 15
# Fields of a composite column: selected one at a time, expanded with
# `.*`, and assigned by UPDATE and INSERT.
SELECT (v).a, (v).b FROM cf_t ORDER BY id
SELECT (v).* FROM cf_t ORDER BY id
SELECT id, (v).* FROM cf_t ORDER BY id
SELECT (t.v).* FROM cf_t t ORDER BY t.id
SELECT (cf_t.v).a FROM cf_t ORDER BY id
SELECT id, (v).b FROM cf_t WHERE (v).a = 1
SELECT (v).a + 1, ((v)).a FROM cf_t ORDER BY id
SELECT sum((v).a), count((v).b) FROM cf_t
SELECT (v).b, count(*) FROM cf_t GROUP BY (v).b ORDER BY 1
SELECT (v).nope FROM cf_t
SELECT (n.w).inner_b, (n.w).c, ((n.w).inner_b).a FROM cf_n n ORDER BY id
SELECT ((n.w).inner_b).* FROM cf_n n ORDER BY id
SELECT (n.w).* FROM cf_n n ORDER BY id
SELECT ((row(1, 'x')::cf_c)).*
SELECT (information_schema._pg_expandarray(ARRAY['a', 'b'])).*
SELECT (information_schema._pg_expandarray(ARRAY['a', 'b'])).n, pg_typeof((information_schema._pg_expandarray(ARRAY['a', 'b'])).n)
# A predicate is a boolean, whatever its operands' types.
SELECT (v).a BETWEEN 0 AND 5, id IN (1, 2), (v).b LIKE 'x%' FROM cf_t ORDER BY id
UPDATE cf_t SET v.a = 7 WHERE id = 1 RETURNING (v).a, (v).b
UPDATE cf_t SET v.b = 'm', v.a = 8 WHERE id = 3 RETURNING v
UPDATE cf_t SET v.b = 'n' WHERE id = 2 RETURNING v
UPDATE cf_t SET v.a = 1, v.a = 2 WHERE id = 1 RETURNING v
UPDATE cf_t SET v.nope = 1
UPDATE cf_t SET id.a = 1
INSERT INTO cf_t (id, v.a, v.b) VALUES (9, 4, 'q') RETURNING (v).*
INSERT INTO cf_t (id, v.b) VALUES (10, 'w'), (11, 'z') RETURNING v
INSERT INTO cf_t (id, v.nope) VALUES (12, 2)
SELECT id, v FROM cf_t ORDER BY id
# age(xid): a special xid is INT_MAX old, and a catalog's frozen xid is a
# sane age (below 2^31) on both.
SELECT age('0'::xid), age('2'::xid), age(relfrozenxid) FROM pg_class WHERE relname = 'pg_tables'
SELECT age(datfrozenxid) BETWEEN 0 AND 2000000000 FROM pg_database WHERE datname = current_database()
SELECT age(relfrozenxid) BETWEEN 0 AND 2000000000 FROM pg_class WHERE relname = 'cf_t'
SELECT age('3'::xid) >= 0, age(pg_current_xact_id()::xid)
SELECT '4294967300'::xid8::xid
