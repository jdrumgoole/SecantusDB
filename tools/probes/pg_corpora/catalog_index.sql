SELECT indkey::text, indisprimary, indnatts FROM pg_index WHERE indrelid = 'ci_t'::regclass ORDER BY indisprimary
SELECT pg_typeof(indkey) FROM pg_index WHERE indrelid = 'ci_t'::regclass LIMIT 1
SELECT 2 = ANY(indkey) FROM pg_index WHERE indrelid = 'ci_t'::regclass AND NOT indisprimary
SELECT i.indexrelid::regclass::text, a.attname FROM pg_index i, pg_attribute a WHERE a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) AND i.indrelid = 'ci_t'::regclass ORDER BY 1, 2
SELECT '2 3'::int2vector::text
SELECT '2 3'::int2vector
SELECT (SELECT indkey FROM pg_index WHERE indrelid = 'ci_t'::regclass AND NOT indisprimary)::text
SELECT i.indkey::text FROM pg_index i WHERE i.indrelid = 'ci_t'::regclass AND NOT indisprimary
SELECT indkey[0] FROM pg_index WHERE indrelid = 'ci_t'::regclass AND NOT indisprimary
SELECT 'ci_ab'::regclass::text
SELECT relkind FROM pg_class WHERE relname = 'ci_ab'
SELECT c.relname FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid WHERE i.indrelid = 'ci_t'::regclass ORDER BY 1
SELECT indkey FROM pg_index WHERE indrelid = 'ci_t'::regclass ORDER BY indisprimary
SELECT indexrelid::regclass, indisunique FROM pg_index WHERE indrelid = 'ci_t'::regclass ORDER BY 1
SELECT pg_typeof('2 3'::int2vector)
SELECT ('2 3'::int2vector)[1]
SELECT '2 3'::int2vector::text
SELECT 3 = ANY('2 3'::int2vector)
SELECT indexrelid::regclass::text FROM pg_index WHERE indrelid = 'ci_t'::regclass ORDER BY 1
SELECT i.indexrelid::regclass::text FROM pg_index i, pg_class c WHERE c.oid = i.indrelid AND c.relname = 'ci_t' ORDER BY 1
SELECT i.indexrelid::regclass::text AS n FROM pg_index i, pg_class c WHERE c.oid = i.indrelid AND c.relname = 'ci_t' ORDER BY n
SELECT (i.indexrelid::regclass)::text FROM pg_index i, pg_class c WHERE c.oid = i.indrelid AND c.relname = 'ci_t' ORDER BY 1 DESC
SELECT c.relname, i.indisunique, i.indisprimary, i.indkey::text FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = 'ci_t'::regclass ORDER BY i.indexrelid
SELECT relname, relkind FROM pg_class WHERE relname LIKE 'ci_%' ORDER BY 1
SELECT relhasindex FROM pg_class WHERE relname = 'ci_t'
SELECT 'ci_expr'::regclass::oid = (SELECT indexrelid FROM pg_index WHERE indkey::text = '0' AND indrelid = 'ci_t'::regclass)
