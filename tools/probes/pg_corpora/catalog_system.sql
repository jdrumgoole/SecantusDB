# reference-version: 15
# PostgreSQL's own relations in pg_class and pg_attribute (oid < 16384: the
# reference database may hold other sessions' tables), and regclass of them.
select count(*) from pg_attribute where attrelid = 'pg_class'::regclass and attnum > 0
select attname, atttypid::regtype, attnum, attnotnull from pg_attribute where attrelid = 'pg_statistic'::regclass and attnum between 1 and 3 order by attnum
select n.nspname, c.relkind, count(*) from pg_attribute a join pg_class c on c.oid = a.attrelid join pg_namespace n on n.oid = c.relnamespace where c.oid < 16384 and a.attnum > 0 group by 1, 2 order by 1, 2
select n.nspname, c.relkind, count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where c.oid < 16384 group by 1, 2 order by 1, 2
select attname from pg_attribute where attrelid = 'information_schema.tables'::regclass and attnum = 1
select attname, atttypid::regtype from pg_attribute where attrelid = 'pg_class_oid_index'::regclass
select 'pg_statistic'::regclass, 'information_schema.tables'::regclass::text
select count(*) from pg_class where relnamespace = 'pg_catalog'::regnamespace and oid < 16384
select relname, relkind, relnatts, relhasindex, relisshared, relam from pg_class where relname in ('pg_class', 'pg_type', 'pg_tables', 'pg_class_oid_index', 'pg_authid') order by 1
select c.relkind, count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pg_catalog' and c.oid < 16384 group by 1 order by 1
select count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'information_schema' and c.oid < 16384
select 'pg_class'::regclass::oid, (select oid from pg_class where relname = 'pg_class')
select count(*) from pg_class where relname like 'pg_toast%' and oid < 16384
select pg_table_is_visible('pg_class'::regclass)
