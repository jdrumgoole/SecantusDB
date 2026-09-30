# CREATE OPERATOR: a symbol bound to a function (or a built-in operator's).
create function op_close(a int, b int) returns bool language sql as 'select abs($1 - $2) <= 1'
create operator === (leftarg = int, rightarg = int, function = op_close)
create operator ~~~ (leftarg = text, rightarg = text, procedure = textcat)
create operator !! (rightarg = int, function = int4abs)
select 1 === 2, 1 === 3, 'a' ~~~ 'b', !! -5
create table opt (a int, b int)
insert into opt values (1, 2), (1, 5), (4, 3)
select a, b from opt where a === b order by a, b
select a === b as close from opt order by a, b
create operator === (leftarg = int, rightarg = int, function = op_close)
create operator ==== (leftarg = int, rightarg = int, function = nope)
create operator === (leftarg = text, rightarg = text)
select 'x' === 'y'
select oprname, oprleft::regtype, oprright::regtype, oprresult::regtype from pg_operator where oprname in ('===', '~~~') or (oprname = '!!' and oprright = 'int4'::regtype) order by 1
drop operator === (int, int)
select 1 === 2
drop operator === (int, int)
drop operator if exists === (int, int)
drop operator ~~~ (text, text)
drop operator !! (none, int)
drop table opt
drop function op_close(int, int)
create table opt2 (a int)
select * from opt2 where a === 1
select a === 1 from opt2
select 1 ### 2
drop table opt2
