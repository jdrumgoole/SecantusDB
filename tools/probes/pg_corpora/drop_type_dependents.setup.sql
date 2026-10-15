DROP TABLE IF EXISTS dtd_t
DROP TABLE IF EXISTS dtd_u
DROP TABLE IF EXISTS dtd_v
DROP DOMAIN IF EXISTS dtd_d
DROP DOMAIN IF EXISTS dtd_d2
DROP TYPE IF EXISTS dtd_c
DROP TYPE IF EXISTS dtd_e
create type dtd_e as enum ('a')
create table dtd_t (id int, c dtd_e, d dtd_e[])
insert into dtd_t values (1, 'a', '{a}')
create domain dtd_d as dtd_e
create table dtd_u (x dtd_d, y int)
create type dtd_c as (f dtd_e, g int)
create domain dtd_d2 as int
create table dtd_v (id int, a dtd_d2, b dtd_d2)
insert into dtd_v values (1, 2, 3)
