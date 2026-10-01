# reference-version: 15
# Operator resolution over an array subscript, row constructors and a window
# function's result: a cross-category comparison is 42883, not no rows.
create table ops_t (id int, arr int[], t text)
insert into ops_t values (1, '{1,2}', 'a')
select id from ops_t where arr[1] = true
select id from ops_t where arr[1] = 'x'
select id from ops_t where arr[1] = 1
select id from ops_t where row(id, t) = row(1, 'a')
select id from ops_t where row(id, t) = row(true, 'a')
select id from (select id, row_number() over () as rn from ops_t) s where rn = 'x'
select id from (select id, row_number() over () as rn from ops_t) s where rn = true
select id from ops_t where arr[1:1] = '{1}'
select id from ops_t where (arr[1] + 1) = 'y'
drop table ops_t
