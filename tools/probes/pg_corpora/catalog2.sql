# --- information_schema.columns: types, nullability, order, defaults
SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name='cc_parent' ORDER BY ordinal_position
SELECT column_name, udt_name FROM information_schema.columns WHERE table_name='cc_parent' ORDER BY ordinal_position
SELECT numeric_precision, numeric_scale FROM information_schema.columns WHERE table_name='cc_parent' AND column_name='amount'
SELECT character_maximum_length FROM information_schema.columns WHERE table_name='cc_parent' AND column_name='label'
SELECT numeric_precision, numeric_scale FROM information_schema.columns WHERE table_name='cc_parent' AND column_name='id'
SELECT column_name, column_default FROM information_schema.columns WHERE table_name='cc_parent' AND column_default IS NOT NULL ORDER BY column_name
SELECT column_default FROM information_schema.columns WHERE table_name='cc_child' AND column_name='cid'
SELECT count(*) FROM information_schema.columns WHERE table_name='nosuch_cc'
# --- information_schema.tables
SELECT table_name, table_type FROM information_schema.tables WHERE table_name LIKE 'cc_%' ORDER BY table_name
SELECT table_schema FROM information_schema.tables WHERE table_name='cc_parent'
# --- table_constraints and key_column_usage
SELECT constraint_type, count(*) FROM information_schema.table_constraints WHERE table_name='cc_parent' GROUP BY constraint_type ORDER BY constraint_type
SELECT constraint_type, count(*) FROM information_schema.table_constraints WHERE table_name='cc_child' GROUP BY constraint_type ORDER BY constraint_type
SELECT column_name FROM information_schema.key_column_usage WHERE table_name='cc_parent' ORDER BY column_name
SELECT count(*) FROM information_schema.key_column_usage WHERE table_name='cc_child'
# --- information_schema.sequences
SELECT sequence_name, data_type, start_value, minimum_value, maximum_value, increment, cycle_option FROM information_schema.sequences WHERE sequence_name='cc_seq'
SELECT count(*) FROM information_schema.sequences WHERE sequence_name='nosuch_cc'
# --- pg_class and pg_namespace
SELECT relname, relkind FROM pg_class WHERE relname='cc_parent'
SELECT relname, relkind FROM pg_class WHERE relname='cc_seq'
SELECT relnatts FROM pg_class WHERE relname='cc_parent'
SELECT nspname FROM pg_namespace WHERE nspname IN ('public','pg_catalog','information_schema') ORDER BY nspname
# --- pg_attribute over a table
SELECT attname, attnotnull FROM pg_attribute WHERE attrelid='cc_parent'::regclass AND attnum > 0 ORDER BY attnum
SELECT count(*) FROM pg_attribute WHERE attrelid='cc_parent'::regclass AND attnum > 0
# --- the catalog scalar functions, inside expressions
SELECT version() LIKE 'PostgreSQL%'
SELECT current_schema(), current_database() IS NOT NULL
SELECT current_setting('server_version_num') ~ '^[0-9]+$'
SELECT current_setting('nosuch_guc_cc')
SELECT current_setting('nosuch_guc_cc', true) IS NULL
SELECT format_type(23, NULL), format_type(25, NULL), format_type(1700, 655366), format_type(1043, 24)
SELECT obj_description('cc_parent'::regclass) IS NULL
# --- a user table may be called `columns` without the view shadowing it
CREATE TABLE columns (id int PRIMARY KEY, x int)
INSERT INTO columns VALUES (1, 10)
SELECT id, x FROM columns
SELECT count(*) > 0 FROM information_schema.columns WHERE table_name='cc_parent'
DROP TABLE columns
