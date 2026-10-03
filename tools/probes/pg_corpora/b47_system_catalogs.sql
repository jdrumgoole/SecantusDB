# reference-version: 15
# Batch 47: PostgreSQL's own indexes in pg_index / pg_indexes /
# pg_get_indexdef (they had no rows), CREATE FUNCTION's SECURITY DEFINER
# and SET clauses recorded in pg_proc and applied for the call, and an
# UPDATE of a catalog that changes nothing reports its rows (it answered 0).
SELECT count(*) FROM pg_index WHERE indexrelid < 16384
SELECT indexrelid::regclass::text, indrelid::regclass::text, indnatts, indnkeyatts, indisunique, indisprimary, indisvalid, indkey::text, indcollation::text, indclass::text, indoption::text, indexprs IS NULL, indpred IS NULL FROM pg_index WHERE indexrelid < 16384 ORDER BY 1
SELECT pg_get_indexdef(indexrelid) FROM pg_index WHERE indexrelid < 16384 ORDER BY 1
SELECT schemaname, tablename, indexname, tablespace, indexdef FROM pg_indexes WHERE schemaname = 'pg_catalog' ORDER BY indexname
SELECT tablename, count(*) FROM pg_indexes WHERE schemaname = 'pg_catalog' GROUP BY 1 ORDER BY 1
SELECT prosecdef, proconfig FROM pg_proc WHERE proname = 'b47f'
SELECT prosecdef, proconfig FROM pg_proc WHERE proname = 'b47g'
SELECT b47g(), current_setting('work_mem')
SELECT b47f(3)
UPDATE pg_class SET relname = 'pg_class' WHERE oid = 1259
UPDATE pg_class SET relname = 'pg_class', relkind = 'r' WHERE relname = 'pg_class'
UPDATE pg_class SET relname = relname WHERE oid = 0
DELETE FROM pg_class WHERE relname = 'no such relation'
UPDATE pg_class SET relname = relname WHERE relname = 'pg_class'
UPDATE pg_class SET relnatts = relnatts + 0 WHERE relname IN ('pg_class', 'pg_type')
UPDATE pg_am SET amname = lower(amname) WHERE amname = 'btree'
