DROP TABLE IF EXISTS b55_o;
DROP TABLE IF EXISTS b55_i;
create table b55_o (id int primary key, a int, s text, n numeric);
create table b55_i (id int primary key, x int, s text collate "en-x-icu", p text, v numeric, w numeric, c text collate "C", u text collate "und-x-icu");
insert into b55_o values (1, 1, 'b', 1.5), (2, 2, 'a', 2), (3, 3, 'Zeta', null), (4, 1, 'B', 'NaN'), (5, 2, 'é', -1), (6, 4, null, 0), (7, 1, 'ab', 10.25);
insert into b55_i values (1, 1, 'a', 'a', 1.50, 3, 'a', 'a'), (2, 1, 'B', 'B', 'NaN', 1.5, 'B', 'B'), (3, 2, 'c', 'c', -2.5, null, 'c', 'c'), (4, 3, 'zeta', 'zeta', 7, 7, 'zeta', 'zeta'), (5, 2, 'E', 'E', 2.000, 2, 'E', 'E'), (6, 1, 'ä', 'ä', null, 10.250, 'ä', 'ä'), (7, 1, 'Ab', 'Ab', 1e10, -0.5, 'Ab', 'Ab');
