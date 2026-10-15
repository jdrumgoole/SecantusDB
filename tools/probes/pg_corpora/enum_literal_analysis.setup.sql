DROP TABLE IF EXISTS en_t
DROP TABLE IF EXISTS en_u
DROP TYPE IF EXISTS en_e
DROP TYPE IF EXISTS en_f
create type en_e as enum ('a','b')
create table en_t (c en_e, d text)
create table en_u (c en_e)
insert into en_u values ('a')
