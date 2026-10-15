# reference-version: 15
# Input errors read from inside an array or record literal are placed at the literal (array_append('{1.5}', 2), a dollar-quoted VARIADIC array, '(1,x)'::record); one read from a column has no position (RP-015).
select array_append('{1.5}', 2)
select concat(variadic $$${1,null,3}$$::int[])
select '{1,x}'::int[]
select array_prepend(1, '{a}')
select a::int[] from ep_t
select '(1,x)'::record
select array_cat('{1}', '{y}'::int[])
select '{1.5}'::int[], '{2.5}'::int[]
select '(1,x)'::record
select row(1,'x')::record::text
select null::record is null
