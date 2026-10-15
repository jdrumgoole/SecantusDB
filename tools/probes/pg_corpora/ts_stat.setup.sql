DROP TABLE IF EXISTS tst_t
create table tst_t (id int, v tsvector, t text)
insert into tst_t values (1, 'a:1A,2B b:3 c', 'x'), (2, 'a:4 d', 'y'), (3, null, 'z')
