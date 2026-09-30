DROP TABLE IF EXISTS sj14_a;
DROP TABLE IF EXISTS sj14_b;
create table sj14_a (id int, v int);
create table sj14_b (id int, a_id int, w text);
insert into sj14_a values (1, 10), (2, 20), (3, null), (null, 40), (5, 50);
insert into sj14_b values (1, 1, 'x'), (2, 1, 'y'), (3, 3, 'x'), (4, null, 'z'), (5, 5, null);
