# reference-version: 15
# LANGUAGE internal wrappers over built-in C functions: accepted for any
# prosrc PostgreSQL 15 has, and callable where the C function has a SQL form
# here (an operator function, or a built-in of the same C function).
create function in_len(text) returns int language internal as 'textlen'
select in_len('abcd')
create function in_add(int, int) returns int language internal immutable as 'int4pl'
select in_add(2, 3)
create function in_up(text) returns text language internal as 'upper'
select in_up('abc')
create function in_abs(numeric) returns numeric language internal as 'numeric_abs'
select in_abs(-2.5)
create function in_bad(text) returns int language internal as 'no_such_fn'
select proname, prolang = (select oid from pg_language where lanname = 'internal'), prosrc from pg_proc where proname like 'in\_%' and pronamespace = 'public'::regnamespace order by 1
drop function in_len(text)
drop function in_add(int, int)
drop function in_up(text)
drop function in_abs(numeric)
