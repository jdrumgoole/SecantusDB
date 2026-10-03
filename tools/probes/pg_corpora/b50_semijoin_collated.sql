select id from b50_co o where exists (select 1 from b50_c c where c.x = o.a and c.s > o.s) order by 1;
select id from b50_co o where exists (select 1 from b50_c c where c.x = o.a and c.s < o.s) order by 1;
select id from b50_co o where exists (select 1 from b50_c c where c.x = o.a and c.p > o.s) order by 1;
select id, (select count(*) from b50_c c where c.x = o.a and c.p <= o.s) from b50_co o order by 1;
