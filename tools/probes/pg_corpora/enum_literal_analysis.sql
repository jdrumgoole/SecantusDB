# reference-version: 15
# An enum compared with an untyped literal reads it as the enum when the statement is analysed (22P02 at the literal over an empty table), in =, <, IN and IS DISTINCT FROM; beside any other typed operand there is no operator (42883) (RP-041).
select c = 'zz' from en_t
select 'zz' = c from en_t
select * from en_t where c = 'zz'
select c < 'zz' from en_t
select c = 'a' from en_u
select * from en_u where c <> 'b'
select c = d from en_t
select * from en_u where c in ('a', 'zz')
select c = 'zz'::text from en_t
create type en_f as enum ('a')
select c = 'a'::en_f from en_u
select c = c from en_u
select c::text = 'a' from en_u
select c = any('{a}') from en_u
select c is distinct from 'zz' from en_t
select c is not distinct from c from en_u
select c is distinct from 'a'::text from en_u
