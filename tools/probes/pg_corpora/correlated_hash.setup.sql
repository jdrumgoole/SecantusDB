DROP TABLE IF EXISTS ch30_o;
DROP TABLE IF EXISTS ch30_t;
create table ch30_o (id int primary key, a int, b text, d date, f float8, n numeric);
create table ch30_t (id int primary key, x int, y text, d date, g float8, n numeric, v int);
insert into ch30_o values (1, 1, 'a', '2020-01-01', 1.0, 1.0), (2, 2, 'b', '2020-01-02', 2.5, 2), (3, null, 'c', null, null, null), (4, 4, null, '2020-01-04', 4, 4.00), (5, 1, 'A', '2020-01-01', -0.0, 1);
insert into ch30_t values (1, 1, 'a', '2020-01-01', 1, 1, 10), (2, 1, 'a', '2020-01-01', 1.0, 1.0, 20), (3, 2, 'b', '2020-01-02', 2.5, 2.0, null), (4, null, null, null, null, null, 40), (5, 4, 'd', '2020-01-04', 0.0, 4, 50), (6, 5, 'e', '2020-01-05', 5, 5, 60);
