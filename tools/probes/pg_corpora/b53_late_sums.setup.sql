DROP TABLE IF EXISTS b53_o;
DROP TABLE IF EXISTS b53_t;
create table b53_o (id int primary key, a int, b int);
create table b53_t (id int primary key, x int, y int, bi bigint, nu numeric, nf numeric(10,2), fl float8, re real);
insert into b53_o values (1, 1, 0), (2, 1, 5), (3, 2, 0), (4, null, 1), (5, 3, 100), (6, 9, 0), (7, 2, null), (8, 4, 0), (9, 5, 0), (10, 6, 0);
insert into b53_t values (1, 1, 1, 9223372036854775807, 1.5, 1.25, 0.1, 0.1), (2, 1, 3, null, null, null, null, null), (3, 1, 3, 9223372036854775807, 2.25, 3.5, 0.2, 0.2), (4, 2, 7, -9223372036854775808, -1, 2.1, 1e308, 1), (5, 2, 8, -5, 'NaN', 4.4, 1e308, 2), (6, 3, 2, 7, 0.333333333333333333333, 1.11, 'NaN', 3), (7, null, 1, 70, 7, 7, 7, 7), (8, 2, 7, 1, 100000000000000000000000000000000000001, 0.01, -1e308, 4), (9, 4, 1, 3, 3, 3, 'Infinity', 5), (10, 4, 2, 4, 4, 4, '-Infinity', 6), (11, 5, 1, null, null, null, 'Infinity', 1), (12, 5, 2, 2, 2.0, 2.00, 'Infinity', 2), (13, 6, 1, 1, 1e-30, 1, 1.7976931348623157e308, 1), (14, 6, 2, 1, 1e30, 1, 1.7976931348623157e308, 1);
