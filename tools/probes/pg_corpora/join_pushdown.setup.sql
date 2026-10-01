DROP TABLE IF EXISTS jp14_a;
DROP TABLE IF EXISTS jp14_b;
create table jp14_a (id int primary key, v int);
create table jp14_b (id int, a_id int, w text);
insert into jp14_a select g, g % 5 from generate_series(1, 20) g;
insert into jp14_b select g, g % 25, 'w' || g from generate_series(1, 30) g;
