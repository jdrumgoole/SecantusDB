# INSERT expressions against target columns, and VALUES rows against each other: transformInsertStmt's and transformValuesClause's errors, with their positions.
insert into ia values (1)
insert into ia values (1, 'x'), (2, 'y', 3)
insert into ia values (1, 'x', 2, 3), (2)
insert into ia (a) values (1), (2, 3)
insert into ia (a, b) values (1, 'q'), (2)
insert into ia select 1
insert into ia (a, b) select 1
insert into ia (a) select 1, 2
insert into ia select * from ia
insert into ia (a) select a, b from ia
insert into ia default values
insert into ia (a) select 1 union select 2
insert into ia (a, b) values (default, 'z') returning a, b, c
select count(*) from ia
values (1, 2), (3)
select * from (values (1), (2, 3)) v
DROP TABLE IF EXISTS ia CASCADE
