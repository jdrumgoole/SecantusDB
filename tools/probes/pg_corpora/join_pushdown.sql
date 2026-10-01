select a.id, b.id from jp14_a a join jp14_b b on b.a_id = a.id where a.v = 2 and b.id > 10 order by 1, 2;
select a.id, b.id from jp14_a a left join jp14_b b on b.a_id = a.id where a.v = 3 order by 1, 2;
select a.id, b.id from jp14_a a left join jp14_b b on b.a_id = a.id where b.id is null and a.id < 8 order by 1, 2;
select a.id, b.id from jp14_a a right join jp14_b b on b.a_id = a.id where a.id is null order by 1, 2;
select a.id, b.id from jp14_a a full join jp14_b b on b.a_id = a.id where a.v = 1 or b.id = 29 order by 1, 2;
select count(*) from jp14_a a, jp14_b b where a.id = b.a_id and a.v in (1, 2) and b.w like 'w1%';
select a.id from jp14_a a join jp14_b b on b.a_id = a.id where a.id = $$3$$::int order by 1;
select x.id, y.id from jp14_a x join jp14_a y on x.v = y.v where x.id = 4 and y.id <> 4 order by 1, 2;
select a.id from jp14_a a join jp14_b b on true where a.id = 1 and b.id = 1;
