# reference-version: 15
# pg_relation_is_updatable, pg_column_is_updatable and pg_is_other_temp_schema (RP-015).
SELECT pg_relation_is_updatable('ru'::regclass, false), pg_relation_is_updatable('ruv'::regclass, false), pg_relation_is_updatable('ruv2'::regclass, true), pg_relation_is_updatable('pg_class'::regclass, false), pg_relation_is_updatable(0, false), pg_relation_is_updatable(99999, true)
SELECT pg_column_is_updatable('ruv'::regclass, 1::int2, false), pg_column_is_updatable('ruv2'::regclass, 1::int2, false), pg_column_is_updatable('ru'::regclass, 1::int2, false)
SELECT pg_is_other_temp_schema(11), pg_is_other_temp_schema(99999), pg_relation_is_updatable(null, true)
SELECT c.relname, pg_relation_is_updatable(c.oid, false) from pg_class c where c.relname in ('ru', 'ruv', 'ruv2') order by 1
SELECT pg_settings_get_flags('work_mem'), pg_settings_get_flags('nope'), pg_settings_get_flags('server_version'), pg_settings_get_flags('WORK_MEM'), pg_settings_get_flags(null)
SELECT pg_typeof(pg_settings_get_flags('work_mem')), pg_settings_get_flags('search_path'), pg_settings_get_flags('transaction_isolation')
