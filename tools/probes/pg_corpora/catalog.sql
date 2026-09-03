SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name='cat10' ORDER BY ordinal_position
SELECT column_name, column_default FROM information_schema.columns WHERE table_name='cat10' AND column_default IS NOT NULL ORDER BY column_name
SELECT numeric_precision, numeric_scale FROM information_schema.columns WHERE table_name='cat10' AND column_name='c'
SELECT table_name, table_type FROM information_schema.tables WHERE table_name='cat10'
SELECT constraint_type FROM information_schema.table_constraints WHERE table_name='cat10' ORDER BY constraint_type
SELECT relname, relkind FROM pg_class WHERE relname='cat10'
SELECT attname, atttypid::regtype::text, attnotnull FROM pg_attribute WHERE attrelid='cat10'::regclass AND attnum > 0 ORDER BY attnum
SELECT indexname FROM pg_indexes WHERE tablename='cat10' ORDER BY indexname
SELECT count(*) FROM pg_type WHERE typname='int4'
SELECT typname FROM pg_type WHERE oid = 23
SELECT current_schema(), current_database() IS NOT NULL
SELECT n.nspname FROM pg_namespace n WHERE n.nspname='public'
SELECT has_table_privilege('cat10', 'SELECT')
SELECT pg_get_expr(adbin, adrelid) FROM pg_attrdef WHERE adrelid='cat10'::regclass
SELECT format_type(23, NULL), format_type(1700, 655366)
SELECT to_regclass('cat10') IS NOT NULL, to_regclass('nope10') IS NULL
SELECT obj_description('cat10'::regclass) IS NULL
SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid=i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid='cat10'::regclass AND i.indisprimary
SELECT count(*) FROM information_schema.key_column_usage WHERE table_name='cat10'
SELECT pg_typeof(1::regclass)
SELECT version() LIKE 'PostgreSQL%'
SELECT current_setting('server_version_num') ~ '^[0-9]+$'
