DROP TABLE IF EXISTS b50_o;
DROP TABLE IF EXISTS b50_t;
DROP TABLE IF EXISTS b50_u;
create table b50_o (id int primary key, a int, s text, v varchar(10), n numeric, f float8, nm name);
create table b50_t (id int primary key, x int, s text, v varchar(10), n numeric, f float8, nm name, iv interval, y int);
create table b50_u (id int primary key, tid int, w text);
insert into b50_o values (1, 1, 'b', 'B', 1.5, 1.5, 'b'), (2, 2, 'a', 'a', 2, 0.1, 'Z'), (3, 3, 'Zeta', 'é', 0.1, 2, 'a'), (4, null, null, null, null, null, null), (5, 1, '', '', 100000000000000000000000000000000000000.5, 'NaN', ''), (6, 2, 'ab', 'ab ', 'NaN', 3, 'ab');
insert into b50_t values (1, 1, 'a', 'A', 1.50, 1.5, 'a', '1 day', 10), (2, 1, 'c', 'b', 0.10, 0.1, 'c', '2 hours', 20), (3, 2, 'B', 'é', 2.0, 2, 'B', '1 mon', 30), (4, 3, 'zeta', 'Z', 100000000000000000000000000000000000000.5, 'NaN', 'zeta', '-1 day', 40), (5, null, 'x', 'x', null, null, null, null, 50), (6, 2, 'ab', 'ab', 'NaN', 3, 'ab', '0', 60);
insert into b50_u values (1, 1, 'p'), (2, 3, 'q'), (3, 3, 'r'), (4, 9, 's');
