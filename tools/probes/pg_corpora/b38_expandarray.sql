# reference-version: 15
# information_schema._pg_expandarray: pgjdbc's getPrimaryKeys builds on it.
SELECT information_schema._pg_expandarray(ARRAY[10,20,30])
SELECT (information_schema._pg_expandarray(ARRAY[10,20])).x
SELECT (information_schema._pg_expandarray(ARRAY['a','b'])).n
SELECT * FROM information_schema._pg_expandarray(ARRAY[10,20])
SELECT x, n FROM information_schema._pg_expandarray(ARRAY['p','q','r'])
SELECT * FROM information_schema._pg_expandarray(NULL::int[])
SELECT (information_schema._pg_expandarray(i.indkey)).n AS key_seq, (information_schema._pg_expandarray(i.indkey)).x FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid WHERE c.relname = 'b38_pk' ORDER BY 1
SELECT result.column_name, result.key_seq, result.pk_name FROM (SELECT NULL AS TABLE_CAT, n.nspname AS TABLE_SCHEM, ct.relname AS TABLE_NAME, a.attname AS COLUMN_NAME, (information_schema._pg_expandarray(i.indkey)).n AS KEY_SEQ, ci.relname AS PK_NAME, information_schema._pg_expandarray(i.indkey) AS KEYS, a.attnum AS A_ATTNUM FROM pg_catalog.pg_class ct JOIN pg_catalog.pg_attribute a ON (ct.oid = a.attrelid) JOIN pg_catalog.pg_namespace n ON (ct.relnamespace = n.oid) JOIN pg_catalog.pg_index i ON ( a.attrelid = i.indrelid) JOIN pg_catalog.pg_class ci ON (ci.oid = i.indexrelid) WHERE true AND ct.relname = E'b38_pk' AND i.indisprimary ) result where result.A_ATTNUM = (result.KEYS).x ORDER BY result.table_name, result.pk_name, result.key_seq
SELECT a.attname, (information_schema._pg_expandarray(i.indkey)).n FROM pg_attribute a JOIN pg_index i ON a.attrelid = i.indrelid JOIN pg_class c ON c.oid = i.indrelid WHERE c.relname = 'b38_pk' AND a.attnum > 0 ORDER BY 1, 2
DROP TABLE b38_pk
