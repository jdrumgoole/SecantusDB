# reference-version: 15
# ORDER BY a range or multirange: range_cmp / multirange_cmp, not the text (RP-037).
SELECT r from (values ('[5,7)'::int4range), ('[1,3)'), ('empty'), ('[10,11)'), ('(,3)'), ('[1,)'), ('[-5,-1)'), ('[1,2)')) v(r) order by r
SELECT r from (values ('[5,7)'::int4range), ('[1,3)'), ('empty'), ('[10,11)')) v(r) order by r desc
SELECT r from (values ('{[5,7)}'::int4multirange), ('{[1,3),[4,5)}'), ('{}'), ('{[1,3)}')) v(r) order by r
SELECT r from (values ('[1.5,2]'::numrange), ('(1.5,2]'), ('[-10.25,0)'), ('[-2.5,1)'), ('[0.05,1)'), ('[100,200)'), ('empty'), ('(,1)'), ('[1.5,2)')) v(r) order by r
SELECT r from (values ('[2020-01-05,2020-02-01)'::daterange), ('[2019-12-31,2020-01-01)'), ('(,2020-01-01)'), ('[2020-01-05,)')) v(r) order by r
SELECT r from (values ('[2020-01-05 10:00,2020-02-01)'::tsrange), ('[2020-01-05 09:30:00.5,2020-01-06)'), ('[2020-01-05 09:30,2020-01-06)')) v(r) order by r
SELECT r from (values ('[5,7)'::int8range), ('[-9223372036854775807,0)'), ('[1,3)')) v(r) order by r
CREATE table rt (k int, r int4range)
INSERT into rt values (1, '[5,7)'), (2, 'empty'), (3, '[1,3)'), (4, null)
SELECT k from rt order by r, k
SELECT k from rt order by r desc nulls last, k
SELECT k, rank() over (order by r) from rt order by k
DROP TABLE rt
