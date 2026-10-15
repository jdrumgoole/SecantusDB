# reference-version: 15
# A table's row type follows ALTER TABLE (add / drop / retype / rename a column), and a row type's or composite's char(n) field is padded and checked as the column is (RP-032).
select '(1,x)'::rt_t::text
select row(1, 'x')::rt_t::text
select '(ab,x)'::rt_c::text
select row('ab', 'x')::rt_c::text
select ('(ab,x)'::rt_c).c || '|'
alter table rt_t drop column a
select '(y)'::rt_t::text
select '(abcd,x)'::rt_c::text
select '("ab  ",x)'::rt_c::text
create table rt_d (a int, c char(2))
alter table rt_d alter column c type char(4)
select '(1,z)'::rt_d::text
alter table rt_d rename column c to cc
select ('(1,z)'::rt_d).cc
select '(ab,x)'::rt_ct::text
select row('ab','x')::rt_ct::text
select '(abcd,x)'::rt_ct::text
select ('(ab,x)'::rt_ct).c || '|'
