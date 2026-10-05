DROP TABLE IF EXISTS b54_o;
DROP TABLE IF EXISTS b54_t;
create table b54_o (id int primary key, a int, n numeric, s text, b int);
create table b54_t (id int primary key, x int, n numeric, nb numeric(10,2), s text, re real, y int, z int);
insert into b54_o values (1, 1, 1.5, 'b', 0), (2, 1, -2, 'a', 3), (3, 2, 'NaN', 'c', 1), (4, null, 0, null, 0), (5, 3, 100000000000000000000.5, 'zz', 9), (6, 2, null, 'b', null), (7, 4, 2, 'A', 0), (8, 5, 0.1, 'x', 0);
insert into b54_t values (1, 1, 1.25, 1.50, 'a', 1.5, 1, 0), (2, 1, 2, 2.00, 'c', 2.25, 2, 4), (3, 1, -3, -3.10, 'B', 0.1, 3, 1), (4, 2, 'NaN', 7.77, 'd', 3.4e38, 4, 0), (5, 2, 1e-20, 0, 'e', 3.4e38, 5, 2), (6, 3, 100000000000000000000.25, 9.99, 'zzz', -1e-10, 6, 0), (7, 3, 100000000000000000001, 1, 'z', 1e30, 7, 5), (8, null, 5, 5, 'q', 7, 8, 1), (9, 4, null, null, null, null, 9, 0), (10, 4, 2.000, 2, 'A', 0.5, 10, 3), (11, 5, 0.1, 0.10, 'x', 0.1, 11, 0), (12, 5, 0.10000000000000000001, 0.1, 'y', 0.2, 12, 9);
