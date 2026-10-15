DROP TABLE IF EXISTS rt_t
DROP TABLE IF EXISTS rt_c
DROP TABLE IF EXISTS rt_d
DROP TYPE IF EXISTS rt_ct
create table rt_t (a int)
alter table rt_t add column b text
create table rt_c (c char(3), v text)
create type rt_ct as (c char(3), v text)
