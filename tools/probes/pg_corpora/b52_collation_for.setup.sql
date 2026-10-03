DROP TABLE IF EXISTS b52c;
create table b52c (i int, t text, n name, c text collate "C", ts timestamptz);
insert into b52c values (1, 'a', 'a', 'a', now());
