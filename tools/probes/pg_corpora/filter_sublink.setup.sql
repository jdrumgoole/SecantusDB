DROP TABLE IF EXISTS fs24_o;
DROP TABLE IF EXISTS fs24_t;
create table fs24_o (id int primary key, a int, b int);
create table fs24_t (id int primary key, x int, y int, f bool);
insert into fs24_o values (1, 10, 1), (2, 20, 2), (3, null, 1), (4, 40, null), (5, 10, 2), (6, 60, 1);
insert into fs24_t values (1, 10, 1, true), (2, 20, 5, false), (3, null, 1, true), (4, 60, 9, null);
