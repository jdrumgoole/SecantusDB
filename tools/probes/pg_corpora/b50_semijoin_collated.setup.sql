DROP TABLE IF EXISTS b50_co;
DROP TABLE IF EXISTS b50_c;
create table b50_co (id int primary key, a int, s text);
create table b50_c (id int primary key, x int, s text collate "en-x-icu", p text);
insert into b50_co values (1, 1, 'b'), (2, 2, 'a'), (3, 3, 'Zeta'), (4, 1, 'B');
insert into b50_c values (1, 1, 'a', 'a'), (2, 1, 'B', 'B'), (3, 2, 'c', 'c'), (4, 3, 'zeta', 'zeta');
