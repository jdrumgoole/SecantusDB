# reference-version: 15
# A char(n) field too long for a row type: a text literal is input (22001 at the literal), a record cast truncates; an assignment's 22001 has no position (RP-032).
select '(abcd,x)'::ovf_t
select '(abcd,x)'::ovf_t::text
select 1, '(abc)'::ovf_ct
insert into ovf_t values ('abcd', 'x')
select 'abcd'::char(3)
select row('abcd', 'x')::ovf_t
update ovf_t set c = 'abcde'
select row('ab  cd', 'x')::ovf_t::text
select row(null, 'x')::ovf_t::text
select row('a', 'x')::ovf_t::text
