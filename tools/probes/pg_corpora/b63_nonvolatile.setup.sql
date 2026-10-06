DROP TABLE IF EXISTS b63_nv CASCADE
CREATE TABLE b63_nv (x int)
INSERT INTO b63_nv VALUES (1), (2)
CREATE OR REPLACE FUNCTION b63_s_ins() RETURNS int LANGUAGE sql STABLE AS $$ select 1; insert into b63_nv values (10); select 2 $$
CREATE OR REPLACE FUNCTION b63_i_upd() RETURNS int LANGUAGE sql IMMUTABLE AS $$ update b63_nv set x = 5; select 1 $$
CREATE OR REPLACE FUNCTION b63_s_cte() RETURNS int LANGUAGE sql STABLE AS $$ with w as (insert into b63_nv values (11) returning x) select x from w $$
CREATE OR REPLACE FUNCTION b63_s_lock() RETURNS int LANGUAGE sql STABLE AS $$ select x from b63_nv order by x limit 1 for update $$
CREATE OR REPLACE FUNCTION b63_s_show() RETURNS int LANGUAGE sql STABLE AS $$ show work_mem; select 1 $$
CREATE OR REPLACE FUNCTION b63_s_read() RETURNS int LANGUAGE sql STABLE AS $$ select count(*)::int from b63_nv $$
CREATE OR REPLACE FUNCTION b63_v_ins() RETURNS int LANGUAGE sql VOLATILE AS $$ insert into b63_nv values (12); select 1 $$
CREATE OR REPLACE FUNCTION b63_p_ins() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin insert into b63_nv values (13); return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_del() RETURNS int LANGUAGE plpgsql IMMUTABLE AS $$ begin delete from b63_nv; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_ddl() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin create table b63_nv_z (a int); return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_trunc() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin perform 1; truncate b63_nv; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_set() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin set work_mem = '5MB'; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_exec() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin execute 'insert into b63_nv values (14)'; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_forupd() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin perform x from b63_nv for update; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_forshare() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin perform x from b63_nv for share; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_notify() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin notify b63_chan; return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_read() RETURNS int LANGUAGE plpgsql STABLE AS $$ declare n int; begin select count(*) into n from b63_nv; raise notice 'n=%', n; return n; end $$
CREATE OR REPLACE FUNCTION b63_p_vw() RETURNS int LANGUAGE plpgsql VOLATILE AS $$ begin insert into b63_nv values (15); return 1; end $$
CREATE OR REPLACE FUNCTION b63_p_calls_v() RETURNS int LANGUAGE plpgsql STABLE AS $$ begin perform b63_p_vw(); return 1; end $$
CREATE OR REPLACE FUNCTION b63_v_calls_s() RETURNS int LANGUAGE plpgsql VOLATILE AS $$ begin insert into b63_nv values (16); perform b63_p_ins(); return 1; end $$
