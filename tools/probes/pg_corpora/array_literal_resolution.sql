# reference-version: 15
# Untyped array literals: read as the other operand's array type under ANY / ALL, and checked for structure before their elements (RP-039).
select 2 = any('{1,2}')
select 2 = any('{1,x}')
select n from any_t where n = any('{1,2}')
select n from any_t where n = any('{1,x}')
select s from any_t where s = any('{a,b}')
select 2 <> all('{1,3}')
select 2.5 = any('{2.5}')
select 'b' = any('{a,b}')
select n from any_t where n = any('{}')
select '{1,{2,x}}'::int[]
select '{1,{2}}'::int[]
select '{{1},{2,3}}'::int[]
select '{{1},2}'::int[]
select '{{1,2},{3,4}}'::int[]
select '{{1},{x}}'::int[]
select '{1,{2,x}}'::text[]
