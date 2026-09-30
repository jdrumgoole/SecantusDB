# reference-version: 15
# information_schema lists PostgreSQL's own pg_catalog / information_schema
# relations and their columns, as PostgreSQL 15 does.
select count(*) from information_schema.tables where table_schema = 'pg_catalog'
select count(*) from information_schema.tables where table_schema = 'information_schema'
select table_name, table_type from information_schema.tables where table_schema = 'pg_catalog' and table_name in ('pg_class', 'pg_tables', 'pg_type') order by 1
select count(*) from information_schema.columns where table_schema in ('pg_catalog', 'information_schema')
select column_name, data_type, udt_name, is_nullable, collation_name from information_schema.columns where table_name = 'pg_prepared_statements' order by ordinal_position
select column_name, data_type, character_maximum_length from information_schema.columns where table_schema = 'information_schema' and table_name = 'tables' order by ordinal_position
select table_schema, count(*) from information_schema.tables where table_schema <> 'public' group by 1 order by 1
