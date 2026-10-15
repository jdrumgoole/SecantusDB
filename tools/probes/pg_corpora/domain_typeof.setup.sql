DROP TABLE IF EXISTS dt_t
DROP DOMAIN IF EXISTS dt_d
DROP DOMAIN IF EXISTS dt_nn
create domain dt_d as int
create domain dt_nn as text not null check (value <> '')
create table dt_t (id int, c dt_d, n dt_nn, p int)
insert into dt_t values (1, 5, 'x', 2), (2, null, 'y', 3)
