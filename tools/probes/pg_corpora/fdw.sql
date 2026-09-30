# reference-version: 15
# Foreign data: wrappers, servers, user mappings and foreign tables as
# catalog objects (no FDW handler: reads, writes and IMPORT FOREIGN SCHEMA
# refuse as PostgreSQL does for a handler-less wrapper), their options,
# dependencies, CASCADE, and the catalogs and information_schema views.
create foreign data wrapper fw_w
create foreign data wrapper fw_w
create foreign data wrapper fw_w2 options (debug 'true')
create server fw_s foreign data wrapper fw_w options (host 'h', port '1')
create server fw_s foreign data wrapper fw_w
create server if not exists fw_s foreign data wrapper fw_w
create server fw_bad foreign data wrapper fw_nope
create user mapping for current_user server fw_s options (user 'u', password 'p')
create user mapping for public server fw_s
create user mapping for public server fw_s
create user mapping if not exists for public server fw_s
create foreign table fw_t (a int, b text not null) server fw_s options (table_name 'remote')
create foreign table fw_t2 (a int) server fw_nope
select * from fw_t
insert into fw_t values (1, 'x')
select fdwname, fdwhandler, fdwvalidator, fdwoptions from pg_foreign_data_wrapper where fdwname like 'fw_%' order by 1
select srvname, srvtype, srvversion, srvoptions, w.fdwname from pg_foreign_server s join pg_foreign_data_wrapper w on w.oid = s.srvfdw where srvname like 'fw_%'
select srvname, usename = current_user, umoptions from pg_user_mappings where srvname like 'fw_%' order by 2
select c.relname, c.relkind, ft.ftoptions from pg_foreign_table ft join pg_class c on c.oid = ft.ftrelid where c.relname like 'fw_%'
select foreign_data_wrapper_name, foreign_data_wrapper_language from information_schema.foreign_data_wrappers where foreign_data_wrapper_name like 'fw_%' order by 1
select foreign_server_name, foreign_data_wrapper_name from information_schema.foreign_servers where foreign_server_name like 'fw_%'
select foreign_table_name, foreign_server_name from information_schema.foreign_tables where foreign_table_name like 'fw_%'
select table_name, table_type from information_schema.tables where table_name like 'fw_%'
alter server fw_s options (set host 'h2', drop port, add dbname 'd')
select srvoptions from pg_foreign_server where srvname = 'fw_s'
alter server fw_s version '2'
alter foreign data wrapper fw_w options (add x '1')
alter user mapping for current_user server fw_s options (set password 'q')
alter foreign table fw_t add column c int
alter foreign table fw_t options (set table_name 'r2')
select attname from pg_attribute where attrelid = 'fw_t'::regclass and attnum > 0 order by attnum
import foreign schema public from server fw_s into public
drop user mapping for current_user server fw_s
drop table fw_t
alter foreign table fw_nope add column z int
select * from fw_t join fw_t f2 using (a)
select (select count(*) from fw_t)
drop server fw_s
drop foreign data wrapper fw_w
drop foreign table fw_t
drop user mapping for public server fw_s
drop user mapping if exists for public server fw_s
drop server fw_s
drop server if exists fw_s
drop foreign data wrapper fw_w, fw_w2
drop foreign data wrapper fw_nope
create foreign data wrapper fw_c
create server fw_cs foreign data wrapper fw_c
create user mapping for public server fw_cs
create foreign table fw_ct (a int) server fw_cs
drop foreign data wrapper fw_c cascade
select count(*) from pg_class where relname = 'fw_ct'
select count(*) from pg_foreign_server where srvname = 'fw_cs'
create table fw_plain (a int)
drop foreign table fw_plain
drop table fw_plain
