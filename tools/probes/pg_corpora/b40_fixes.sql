# reference-version: 15
# Every PostgreSQL 15 parameter is SHOW-able, and a ROLLBACK restores the
# default rather than forgetting the parameter; pg_settings reports the
# default in the parameter's own unit.
SHOW work_mem
BEGIN
SET work_mem = '8MB'
SHOW work_mem
ROLLBACK
SHOW work_mem
SET maintenance_work_mem = '128MB'
RESET maintenance_work_mem
SHOW maintenance_work_mem
SHOW effective_cache_size
SHOW random_page_cost
SHOW jit
SHOW client_min_messages
SHOW default_statistics_target
SELECT current_setting('work_mem')
SELECT name, setting, unit, context, vartype, boot_val FROM pg_settings WHERE name IN ('work_mem', 'maintenance_work_mem', 'temp_buffers') ORDER BY name
# A pg_temp function lives in the session's temp schema.
CREATE FUNCTION pg_temp.b40_tf() RETURNS int LANGUAGE sql AS 'select 40'
SELECT pg_temp.b40_tf()
SELECT pronamespace::regnamespace::text LIKE 'pg_temp_%' FROM pg_proc WHERE proname = 'b40_tf'
SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE proname = 'b40_tf' AND nspname = 'public'
DROP FUNCTION pg_temp.b40_tf()
# ADD PRIMARY KEY USING INDEX keeps the index's INCLUDE columns and name.
ALTER TABLE b40_pk ADD PRIMARY KEY USING INDEX b40_pk_ix
SELECT indkey, indnkeyatts, indnatts, indisprimary FROM pg_index WHERE indexrelid = 'b40_pk_ix'::regclass
SELECT pg_get_indexdef('b40_pk_ix'::regclass)
SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'b40_pk'
SELECT conname, conkey FROM pg_constraint WHERE conrelid = 'b40_pk'::regclass
DROP TABLE b40_pk
# A composite type is a pg_class relation (relkind 'c'); a built-in that
# returns name types as name; 42P16 points at the relation.
CREATE TYPE b40_comp AS (a int, b text)
SELECT relname, relkind, relnatts, relnamespace::regnamespace, reltype = 'b40_comp'::regtype::oid FROM pg_class WHERE relname = 'b40_comp'
DROP TYPE b40_comp
SELECT pg_typeof(getdatabaseencoding()), pg_typeof(current_database())
CREATE TEMP TABLE public.b40_tx (i int)
