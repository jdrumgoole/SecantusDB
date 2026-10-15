DROP TABLE IF EXISTS rfs2_t, rfs2_w, rfs2_o CASCADE
create table rfs2_t (k int, v text)
insert into rfs2_t values (1, 'a'), (2, 'b')
create table rfs2_w (a int, b text)
insert into rfs2_w values (1, 'x'), (2, 'y')
create table rfs2_o (id int, v int, s text, p point)
insert into rfs2_o values (1, 3, 'b', '(1,1)'), (2, 1, 'a', '(0,0)'), (3, null, 'c', '(2,2)'), (4, 2, null, '(3,3)')
